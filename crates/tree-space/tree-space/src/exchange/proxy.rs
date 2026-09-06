//! Proxy write (protocol `交换协议/01-目标与设计.md` §3.2): a non-owner asks the
//! owner to write on its behalf (async "sudo"). The receipt confirms success /
//! rejects with a reason / times out (upstream death). Transport reuses
//! `Source`/`SharedRegion`; only IPC is in scope (01 §8).
//!
//! E-2 lands the semantics (payload types), the in-process owner-side handler
//! and the IPC byte framing (`02-施工路线图.md` §6.3); it does not spawn child
//! processes. The handler runs through the owner's `metadata_lock` (via
//! `TbLibrary::commit`), so owner direct writes and proxy writes serialize
//! logically single-writer (01 §3.2).

use crate::block::RefId;
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::{ExchangeKey, IpcSource, RuntimeTable, Source};
use crate::layout::TbLayout;
use crate::layout::tb::derive_ref_rows;
use crate::shared::{RegionHandle, SharedRegion};
use crate::tb_library::TbLibrary;
use crate::xpath::XPath;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A proxy write request (01 §3.2): a non-owner asks the owner to commit on
/// its behalf. The payload mirrors the owner's direct write
/// (`TbLibrary::commit`): canonical tree bytes + leaf references + bucket.
///
/// `Eq`/`PartialEq` are implemented manually because `Bucket` (frozen, echoing
/// E-1) has no `PartialEq`; equality compares `request_id`, `target`, the
/// canonical tree bytes, the leaf set **as a multiset** (order-insensitive)
/// and the bucket envelope set.
#[derive(Clone, Debug)]
pub struct ProxyRequest {
    /// Stable request identity (monotonic, caller-supplied); receipts echo it.
    pub request_id: u64,
    /// The target owner, identified by its `Source` (routing hint; IPC only).
    pub target: Source,
    /// Canonical tree bytes (the whole tree — never splittable/lazy, 01 §1/§4).
    pub tree_bytes: Vec<u8>,
    /// Leaf references (xpath → block identity), mirrors `TbLibrary::commit`.
    pub leaf_refs: Vec<(XPath, RefId)>,
    /// The bucket to commit (envelopes), mirrors `TbLibrary::commit`.
    pub bucket: Bucket,
}

impl PartialEq for ProxyRequest {
    fn eq(&self, other: &Self) -> bool {
        self.request_id == other.request_id
            && self.target == other.target
            && self.tree_bytes == other.tree_bytes
            && sorted_leaves(&self.leaf_refs) == sorted_leaves(&other.leaf_refs)
            && buckets_equal(&self.bucket, &other.bucket)
    }
}

impl Eq for ProxyRequest {}

/// Why the owner rejected a proxy write (01 §3.2 rejection semantics).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    /// The recipient is not the owner of the target (routed to a non-owner).
    NotOwner,
    /// The write payload failed validation (dangling refs, malformed tree, …).
    InvalidPayload,
    /// The owner's policy refuses proxy writes entirely.
    ProxyRefused,
}

/// The owner's reply to a proxy request (01 §3.2: async, receipt-confirmed).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyReceipt {
    /// The owner accepted and committed the write.
    Accepted {
        /// The echoed request id.
        request_id: u64,
    },
    /// The owner rejected the write with a reason.
    Rejected {
        /// The echoed request id.
        request_id: u64,
        /// Why the write was rejected.
        reason: RejectReason,
    },
    /// The upstream died before replying — manifests as timeout (01 §3.2).
    TimedOut {
        /// The echoed request id (or the scanned/zero id when nothing could be
        /// read from the reply region).
        request_id: u64,
    },
}

/// Proxy-write policy gate on the owner side (01 §3.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyPolicy {
    /// The owner accepts proxy writes.
    Allow,
    /// The owner rejects every proxy write.
    Deny,
}

/// Default [`ProxyPolicy`] when none is supplied at the call site
/// (`02-施工路线图.md` §6.6): proxy writes are allowed by default.
impl Default for ProxyPolicy {
    fn default() -> Self {
        Self::Allow
    }
}

impl ProxyReceipt {
    /// True iff the receipt is `Accepted` (the write succeeded).
    pub fn is_accepted(&self) -> bool {
        matches!(self, ProxyReceipt::Accepted { .. })
    }
    /// The echoed request id.
    pub fn request_id(&self) -> u64 {
        match self {
            ProxyReceipt::Accepted { request_id }
            | ProxyReceipt::Rejected { request_id, .. }
            | ProxyReceipt::TimedOut { request_id } => *request_id,
        }
    }
}

/// Handles a proxy write on the owner's side (01 §3.2): checks ownership +
/// policy, then commits through the owner's `metadata_lock` (via
/// `TbLibrary::commit`), so all writes serialize logically single-writer.
///
/// Ownership check: `require_owner(table, ExchangeKey::Tree)` must pass, else
/// `Rejected(NotOwner)`. Policy `Deny` → `Rejected(ProxyRefused)`. Commit
/// failure (dangling ref / malformed tree / digest mismatch) →
/// `Rejected(InvalidPayload)`.
pub fn handle_proxy_request<L: TbLayout>(
    library: &TbLibrary<L>,
    table: &RuntimeTable,
    policy: ProxyPolicy,
    request: &ProxyRequest,
) -> ProxyReceipt {
    let request_id = request.request_id;
    if crate::exchange::ownership::require_owner(table, ExchangeKey::Tree).is_err() {
        return ProxyReceipt::Rejected {
            request_id,
            reason: RejectReason::NotOwner,
        };
    }
    if policy == ProxyPolicy::Deny {
        return ProxyReceipt::Rejected {
            request_id,
            reason: RejectReason::ProxyRefused,
        };
    }
    match library.commit(&request.tree_bytes, &request.leaf_refs, &request.bucket) {
        Ok(_) => ProxyReceipt::Accepted { request_id },
        Err(_) => ProxyReceipt::Rejected {
            request_id,
            reason: RejectReason::InvalidPayload,
        },
    }
}

/// The proxy-write client (01 §3.2, IPC only): publishes the request into a
/// shared region, hands the handle to the owner, and collects the receipt from
/// a reply region. An expired wait yields `TimedOut` (upstream death =
/// timeout).
#[derive(Clone, Debug)]
pub struct ProxyClient {
    /// The wait budget for a reply; consumed by the transport helpers
    /// (`await_receipt`) in the real cross-process flow (E-3/E-4). The E-2
    /// same-process model exercises it via `await_receipt` directly.
    #[allow(dead_code)] // E-2 same-process: timeout is exercised by await_receipt
    timeout: Duration,
}

impl ProxyClient {
    /// Builds a client from a runtime config's sync timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
    /// Sends a proxy request to `owner` and awaits the receipt. Same-process
    /// model (the owner library lives in this process; cross-process modeling
    /// is the same shape as `shared_region_test`). Runs
    /// [`handle_proxy_request`] and returns its receipt; timeout is exercised
    /// by the transport helpers ([`await_receipt`]).
    pub fn request<L: TbLayout>(
        &self,
        owner: &TbLibrary<L>,
        table: &RuntimeTable,
        policy: ProxyPolicy,
        request: ProxyRequest,
    ) -> ProxyReceipt {
        handle_proxy_request(owner, table, policy, &request)
    }
}

// ---- IPC transport (reuses Source/SharedRegion; zero new deps) ----
//
// Wire form of a request:
//
// ```text
// [request_id u64 LE]
// [target tag u8] [target payload]        tag 1 = Disk: [path len u64 LE][utf8];
//                                         tag 2 = Ipc: no payload (routing hint
//                                         only — the handle is delivered by
//                                         The SharedRegion itself/handoff)
// [tree_bytes len u64 LE][tree bytes]
// [ref_table len u64 LE][ref_table ipc]   = encode_ref_table(derive_ref_rows(...))
// [bucket count u64 LE][per envelope: len u64 LE][envelope bytes]
// ```
//
// Both the request wire and the receipt wire lead with `request_id u64 LE`, so
// a timed-out waiter can echo the id it scanned even when the region it opened
// never carried a receipt.

/// Publishes a proxy request into a shared region, returning the region and its
/// export handle (for hand-off to the owner). Byte form reuses existing codecs:
/// tree canonical bytes + `encode_ref_table` (leaf_refs) + per-envelope framing
/// (bucket); no new dependencies.
pub fn publish_request(request: &ProxyRequest) -> Result<(SharedRegion, RegionHandle)> {
    let mut bytes = Vec::new();
    push_u64(&mut bytes, request.request_id);
    match &request.target {
        Source::Disk(path) => {
            bytes.push(1);
            push_bytes(&mut bytes, path.to_string_lossy().as_bytes());
        }
        Source::Ipc(_) => {
            // Routing hint only (01 §3.2: the target owner, identified by its
            // Source). A RegionHandle is not portable bytes (Windows name /
            // Unix owned fd), so the wire carries the variant tag and the
            // handle is delivered out-of-band via the shared region hand-off.
            bytes.push(2);
        }
    }
    push_bytes(&mut bytes, &request.tree_bytes);
    let rows = derive_ref_rows(&request.leaf_refs, &request.bucket)?;
    let ref_table = crate::layout::tb::encode_ref_table(&rows)?;
    push_bytes(&mut bytes, &ref_table);
    push_u64(&mut bytes, request.bucket.len() as u64);
    for id in request.bucket.ids() {
        let envelope = request
            .bucket
            .get(id)
            .expect("bucket ids iterate stored envelopes");
        push_bytes(&mut bytes, &envelope.encode());
    }
    let region = SharedRegion::publish(bytes)?;
    let handle = region.export()?;
    Ok((region, handle))
}

/// Decodes a proxy request received via a shared region.
pub fn receive_request(region: &SharedRegion) -> Result<ProxyRequest> {
    let mut reader = Reader::new(region.bytes());
    let request_id = reader.take_u64()?;
    let target = match reader.take_u8()? {
        1 => {
            let path_bytes = reader.take_bytes()?;
            let path = String::from_utf8(path_bytes.to_vec())
                .map_err(|_| payload("proxy request target disk path is not valid utf-8"))?;
            Source::Disk(PathBuf::from(path))
        }
        2 => {
            // Reconstructed as a routing-marker IPC source: E-2 carries the
            // handle out-of-band, so the decoded source is a fresh placeholder
            // handle of the same variant.
            let placeholder = SharedRegion::publish(Vec::new())?;
            Source::Ipc(IpcSource::new(placeholder.export()?))
        }
        tag => return Err(payload(format!("invalid proxy request target tag {tag}"))),
    };
    let tree_bytes = reader.take_bytes()?.to_vec();
    let ref_bytes = reader.take_bytes()?;
    let rows = crate::layout::tb::decode_ref_table(ref_bytes)?;
    let leaf_refs = rows
        .iter()
        .map(|row| {
            let xpath = crate::index::xpath_from_canonical_bytes(&row.xpath)?;
            Ok((xpath, RefId::from_bytes(row.ref_id)))
        })
        .collect::<Result<Vec<_>>>()?;
    let count = reader.take_u64()?;
    let mut bucket = Bucket::new();
    for _ in 0..count {
        let envelope_bytes = reader.take_bytes()?;
        let envelope = crate::block::Envelope::decode(envelope_bytes)?;
        bucket.put_envelope(envelope)?;
    }
    reader.finish()?;
    Ok(ProxyRequest {
        request_id,
        target,
        tree_bytes,
        leaf_refs,
        bucket,
    })
}

/// Publishes a proxy receipt into a shared region (small fixed framing).
pub fn publish_receipt(receipt: &ProxyReceipt) -> Result<(SharedRegion, RegionHandle)> {
    let mut bytes = Vec::with_capacity(10);
    let (request_id, tag, reason) = match receipt {
        ProxyReceipt::Accepted { request_id } => (*request_id, 1_u8, None),
        ProxyReceipt::Rejected { request_id, reason } => (
            *request_id,
            2_u8,
            Some(match reason {
                RejectReason::NotOwner => 1_u8,
                RejectReason::InvalidPayload => 2_u8,
                RejectReason::ProxyRefused => 3_u8,
            }),
        ),
        ProxyReceipt::TimedOut { request_id } => (*request_id, 3_u8, None),
    };
    push_u64(&mut bytes, request_id);
    bytes.push(tag);
    if let Some(reason) = reason {
        bytes.push(reason);
    }
    let region = SharedRegion::publish(bytes)?;
    let handle = region.export()?;
    Ok((region, handle))
}

/// Waits for a receipt reachable by `handle`, timing out after `timeout`.
/// Open failure (region never published) or deadline expiry → `TimedOut`
/// (upstream death = timeout, 01 §3.2).
///
/// A region that opens but does not carry a receipt (e.g. the request carrier)
/// is scanned for its leading `request_id` so the timeout receipt can echo it;
/// when no bytes are readable the echoed id is `0`.
pub fn await_receipt(handle: &RegionHandle, timeout: Duration) -> ProxyReceipt {
    let deadline = Instant::now() + timeout;
    let mut echoed = 0_u64;
    loop {
        if let Ok(region) = open_duplicated(handle) {
            match decode_receipt(&region) {
                Ok(receipt) => return receipt,
                Err(()) => {
                    if let Some(id) = leading_request_id(&region) {
                        echoed = id;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return ProxyReceipt::TimedOut { request_id: echoed };
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Opens `handle`, duplicating the underlying descriptor/value on backing
/// platforms so a single `&RegionHandle` survives repeated polls.
#[cfg(windows)]
fn open_duplicated(handle: &RegionHandle) -> Result<SharedRegion> {
    SharedRegion::open(handle.clone())
}

#[cfg(unix)]
fn open_duplicated(handle: &RegionHandle) -> Result<SharedRegion> {
    use std::os::unix::io::{FromRawFd, OwnedFd};
    let duplicated = libc::dup(handle.fd());
    if duplicated < 0 {
        // Nothing to open: the upstream never published (or died). Do **not**
        // wrap the negative value in an OwnedFd: `from_raw_fd` requires a
        // valid descriptor, so a failed dup returns the error directly.
        return Err(TreeSpaceError::new(
            ErrorCode::RequiredDataMissing,
            "the shared-region descriptor cannot be duplicated (upstream dead)",
        ));
    }
    SharedRegion::open(RegionHandle::from_fd(
        unsafe { OwnedFd::from_raw_fd(duplicated) },
        handle.len(),
    ))
}

#[cfg(not(any(windows, unix)))]
fn open_duplicated(_handle: &RegionHandle) -> Result<SharedRegion> {
    Err(TreeSpaceError::new(
        ErrorCode::PayloadMalformed,
        "cross-process sharing is unavailable on this platform",
    ))
}

/// Decodes a receipt from a region, or `Err(())` when the bytes are not a
/// receipt (request carrier, garbage, truncated, …).
fn decode_receipt(region: &SharedRegion) -> std::result::Result<ProxyReceipt, ()> {
    let mut reader = Reader::new(region.bytes());
    let request_id = reader.take_u64().map_err(|_| ())?;
    let tag = reader.take_u8().map_err(|_| ())?;
    let receipt = match tag {
        1 => ProxyReceipt::Accepted { request_id },
        2 => {
            let reason = reader.take_u8().map_err(|_| ())?;
            let reason = match reason {
                1 => RejectReason::NotOwner,
                2 => RejectReason::InvalidPayload,
                3 => RejectReason::ProxyRefused,
                _ => return Err(()),
            };
            ProxyReceipt::Rejected { request_id, reason }
        }
        3 => ProxyReceipt::TimedOut { request_id },
        _ => return Err(()),
    };
    reader.finish().map_err(|_| ())?;
    Ok(receipt)
}

/// Reads the leading `request_id u64 LE` of a region, when present.
fn leading_request_id(region: &SharedRegion) -> Option<u64> {
    let bytes = region.bytes();
    Some(u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?))
}

/// Hand-rolled little-endian reader over the request/receipt wire forms.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take_u8(&mut self) -> Result<u8> {
        let byte = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| payload("truncated proxy wire payload"))?;
        self.pos += 1;
        Ok(byte)
    }
    fn take_u64(&mut self) -> Result<u64> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 8)
            .ok_or_else(|| payload("truncated proxy wire payload"))?;
        self.pos += 8;
        Ok(u64::from_le_bytes(slice.try_into().expect("8-byte slice")))
    }
    /// Takes a length-prefixed byte run: `[len u64 LE][bytes]`.
    fn take_bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.take_u64()?;
        let len = usize::try_from(len)
            .map_err(|_| payload("proxy wire length overflows the host usize"))?;
        // The length is wire-controlled: guard `pos + len` so a hostile/corrupt
        // frame cannot overflow the offset arithmetic (debug builds would
        // otherwise panic on the range construction below).
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| payload("proxy wire length overflows the host usize"))?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| payload("truncated proxy wire payload"))?;
        self.pos = end;
        Ok(slice)
    }
    fn finish(&self) -> Result<()> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(payload("trailing bytes in proxy wire payload"))
        }
    }
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    push_u64(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
}

fn payload(message: impl Into<String>) -> TreeSpaceError {
    TreeSpaceError::new(ErrorCode::PayloadMalformed, message)
}

/// Comparable, order-insensitive leaf set (`xpath canonical bytes`, `ref_id`).
fn sorted_leaves(leaf_refs: &[(XPath, RefId)]) -> Vec<(Vec<u8>, RefId)> {
    let mut out = leaf_refs
        .iter()
        .map(|(xpath, id)| (crate::index::canonical_xpath_bytes(xpath), *id))
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

/// Whether two buckets hold the identical envelope set, keyed by `RefId`.
fn buckets_equal(left: &Bucket, right: &Bucket) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ids().all(|id| left.get(id) == right.get(id))
}

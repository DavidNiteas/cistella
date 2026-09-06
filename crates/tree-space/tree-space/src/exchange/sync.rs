//! Synchronization (protocol `交换协议/01-目标与设计.md` §5): pull (fetch →
//! merge → replace), push (proxy sync) and the fetch/reachability helpers that
//! back them. No history, no old/new — only source and destination (01 §5.1).
//!
//! E-3 lands the behavior over the frozen E-1/E-2 types (`02-施工路线图.md` §7):
//! the pull entry (`fetch_snapshot` → `merge_image` → whole-tree
//! `TbLibrary::commit`) reuses the E-2 full-read and ownership primitives; the
//! push entry reuses the E-2 proxy semantics verbatim (`build_push_request` +
//! a thin `push` wrapper over `ProxyClient::request`) — zero new transport.

use crate::block::RefId;
use crate::bucket::Bucket;
use crate::error::{ErrorCode, Result, TreeSpaceError};
use crate::exchange::access::OwnedSnapshot;
use crate::exchange::merge::{MergeSpec, SyncScope, merge_image, reachable_refs};
use crate::exchange::ownership::require_owner;
use crate::exchange::proxy::{ProxyClient, ProxyPolicy, ProxyReceipt, ProxyRequest};
use crate::exchange::{ExchangeKey, RuntimeTable, Source};
use crate::layout::TbLayout;
use crate::layout::tb::image_leaf_refs;
use crate::tb_library::{TbCommitReceipt, TbLibrary};
use crate::tree::codec::encode;

/// Fetches the source's scoped data: the full tree image plus the blocks
/// reachable within `scope` (01 §5.2 抓取). The image is always whole (the tree
/// is never splittable, 01 §1/§4); the bucket is the source's whole bucket for
/// [`SyncScope::FullTree`] and is trimmed to the in-scope reachable set via
/// [`reachable_refs`] for [`SyncScope::TreeFragment`] (02 §7.4
/// `e_3_fetch_snapshot_full_tree`/`e_3_fetch_snapshot_fragment_trims_bucket`).
///
/// [`SyncScope::Blocks`] is not a tree fetch → `Err(PayloadMalformed)` (use
/// [`fetch_blocks`]). The source is a local `TbLibrary` (same-process modeling,
/// 02 §7.6); cross-process mapped IPC fetch is E-4.
pub fn fetch_snapshot<L: TbLayout>(
    source: &TbLibrary<L>,
    scope: &SyncScope,
) -> Result<OwnedSnapshot> {
    match scope {
        SyncScope::Blocks(_) => Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "Blocks is a bucket-level scope; use fetch_blocks, not fetch_snapshot (02 §7.3)",
        )),
        SyncScope::FullTree => OwnedSnapshot::read_full(source),
        SyncScope::TreeFragment(_) => {
            let snapshot = OwnedSnapshot::read_full(source)?;
            let reachable = reachable_refs(&snapshot.image, scope)?;
            let mut bucket = Bucket::new();
            for id in reachable {
                let envelope = snapshot.bucket.get(id).ok_or_else(|| {
                    TreeSpaceError::new(
                        ErrorCode::DanglingReference,
                        "a reachable tree leaf has no envelope in the source bucket",
                    )
                    .with_context("ref_id", id.to_string())
                })?;
                bucket.put_envelope(envelope.clone())?;
            }
            Ok(OwnedSnapshot {
                image: snapshot.image,
                bucket,
            })
        }
    }
}

/// Fetches the given blocks from the source bucket into a caller-owned bucket
/// (01 §5.2 桶的若干块 — bucket-level sync). No tree merge. Unknown id →
/// `Err(DanglingReference)`. Persisting the result into a TB destination is the
/// caller's/E-4's concern (02 §7.7 裁定 ②).
pub fn fetch_blocks<L: TbLayout>(source: &TbLibrary<L>, ids: &[RefId]) -> Result<Bucket> {
    let source_bucket = source.bucket()?;
    let mut fetched = Bucket::new();
    for id in ids {
        let envelope = source_bucket.get(*id).ok_or_else(|| {
            TreeSpaceError::new(
                ErrorCode::DanglingReference,
                "block is absent from the source bucket",
            )
            .with_context("ref_id", id.to_string())
        })?;
        fetched.put_envelope(envelope.clone())?;
    }
    Ok(fetched)
}

/// pull = 抓取 → merge → 整棵替换 (01 §5.2). The tree's physical operation is
/// always whole-tree replacement through the destination's `commit` — even a
/// fragment pull replaces the whole tree after the merge produces the edited
/// image (02 §7.2).
///
/// Steps: (1) `require_owner(dest_table, ExchangeKey::Tree)` — only the owner
/// writes (01 §3.1); (2) `fetch_snapshot(source, spec.scope)`; (3)
/// `merge_image(dest.tree_image(), snapshot.image, spec)`; (4) merged bucket =
/// `snapshot.bucket ∪ dest.bucket` (commit only persists leaf-reachable blocks,
/// so the union safely covers in-scope source blocks + out-of-scope destination
/// blocks); (5) `encode` + `image_leaf_refs` + `dest.commit`. Returns the
/// commit receipt (same as a direct `commit`).
///
/// `spec.scope` must be `FullTree` or `TreeFragment`; `Blocks` →
/// `Err(PayloadMalformed)` (block-level sync uses [`fetch_blocks`]).
///
/// The destination must be *loaded* (`TbLibrary::open`) so `tree_image`/`bucket`
/// answer; `commit` persists to disk only and does not refresh the in-memory
/// state, so the observable effect is on disk (re-open/`verify`, 02 §7.6).
pub fn pull<L: TbLayout>(
    source: &TbLibrary<L>,
    dest: &TbLibrary<L>,
    dest_table: &RuntimeTable,
    spec: &MergeSpec,
) -> Result<TbCommitReceipt> {
    require_owner(dest_table, ExchangeKey::Tree)?;
    if matches!(spec.scope, SyncScope::Blocks(_)) {
        return Err(TreeSpaceError::new(
            ErrorCode::PayloadMalformed,
            "pull with a Blocks scope is rejected; block-level sync uses fetch_blocks (02 §7.3)",
        ));
    }
    let snapshot = fetch_snapshot(source, &spec.scope)?;
    let merged = merge_image(dest.tree_image()?, &snapshot.image, spec)?;
    let tree_bytes = encode(&merged)?;
    let leaf_refs = image_leaf_refs(&merged)?;

    // Merged bucket = source snapshot bucket ∪ destination bucket.
    let mut bucket = snapshot.bucket;
    {
        let dest_bucket = dest.bucket()?;
        for id in dest_bucket.ids() {
            let envelope = dest_bucket
                .get(id)
                .expect("bucket ids iterate stored envelopes");
            bucket.put_envelope(envelope.clone())?;
        }
    }
    dest.commit(&tree_bytes, &leaf_refs, &bucket)
}

/// Builds a proxy-sync request from a library's loaded state (whole tree):
/// `encode(tree_image())` → `tree_bytes`,
/// `image_leaf_refs(tree_image())` → `leaf_refs`, `bucket().clone()` → `bucket`.
/// Mirrors the owner's direct `commit` (01 §5.3 push = 代理同步). This is the
/// E-3 convenience E-2 did not ship (E-2 tests built `ProxyRequest` by hand).
///
/// Push scope = the whole tree (a proxy write is a whole-tree commit, 02 §7.6);
/// fragment/block-level push is the composition "locally merge, then push the
/// whole tree" (02 §7.7 裁定 ④).
pub fn build_push_request<L: TbLayout>(
    library: &TbLibrary<L>,
    request_id: u64,
    target: Source,
) -> Result<ProxyRequest> {
    let image = library.tree_image()?.clone();
    let tree_bytes = encode(&image)?;
    let leaf_refs = image_leaf_refs(&image)?;
    let bucket = library.bucket()?.clone();
    Ok(ProxyRequest {
        request_id,
        target,
        tree_bytes,
        leaf_refs,
        bucket,
    })
}

/// Proxy-sync push (01 §5.3): submits a sync application to the owner; the
/// owner writes it through the E-2 proxy handler (async "sudo", receipt-
/// confirmed). Thin wrapper over [`ProxyClient::request`] — the E-2 proxy
/// semantics are reused verbatim (zero new transport).
pub fn push<L: TbLayout>(
    client: &ProxyClient,
    owner: &TbLibrary<L>,
    table: &RuntimeTable,
    policy: ProxyPolicy,
    request: ProxyRequest,
) -> ProxyReceipt {
    client.request(owner, table, policy, request)
}

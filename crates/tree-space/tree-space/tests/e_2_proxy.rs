//! E-2: proxy write semantics — acceptance / rejection with reasons / timeout,
//! receipt variants, and the request–receipt byte roundtrip over `SharedRegion`.
//!
//! Asserts the protocol `交换协议/02-施工路线图.md` §6.4 obligations: the
//! owner-side handler accepts an owned valid request and rejects non-owners,
//! invalid payloads and refused policies; receipts echo the request id; and the
//! transport framing roundtrips requests and receipts byte-for-byte, timing out
//! on a reply that never arrives.

use std::path::PathBuf;
use std::time::Duration;
use tree_space::shared::SharedRegion;
use tree_space::tree::codec::encode_node;
use tree_space::{
    Blob, Bucket, Holder, IpcSource, ProxyClient, ProxyPolicy, ProxyReceipt, ProxyRequest,
    RejectReason, RuntimeEntry, RuntimeTable, Source, TreeCodec, TreeNode, Value, await_receipt,
    handle_proxy_request, publish_receipt, publish_request, receive_request,
};

/// The proxy payload fixture mirrors the owner's direct write: a typed tree
/// (`LabSample`) whose `block` leaf references a bucket envelope.
#[derive(TreeCodec, TreeNode, Clone, Debug, PartialEq)]
struct LabSample {
    name: Value,
    block: Blob,
}

fn build_bucket_fixture() -> (Bucket, LabSample) {
    let block = Blob::new(vec![9, 8, 7]);
    let fixture = LabSample {
        name: Value::Utf8("lab".into()),
        block: block.clone(),
    };
    let mut bucket = Bucket::new();
    bucket.put(&block);
    (bucket, fixture)
}

fn valid_request(request_id: u64, bucket: &Bucket, fixture: &LabSample) -> ProxyRequest {
    ProxyRequest {
        request_id,
        target: Source::Disk(PathBuf::from("owner-library")),
        tree_bytes: encode_node(fixture).unwrap(),
        leaf_refs: fixture.leaf_refs(),
        bucket: bucket.clone(),
    }
}

#[test]
fn e_2_proxy_handle_accepts_owned_valid_request() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let (bucket, fixture) = build_bucket_fixture();
    let request = valid_request(7, &bucket, &fixture);

    let receipt = handle_proxy_request(
        &library,
        &RuntimeTable::default(),
        ProxyPolicy::Allow,
        &request,
    );
    assert_eq!(receipt, ProxyReceipt::Accepted { request_id: 7 });
    assert!(receipt.is_accepted());

    // The proxy write committed through the owner's write path: the library now
    // verifies its one-leaf reference table and tree blob.
    assert_eq!(library.verify().unwrap(), 1);
}

#[test]
fn e_2_proxy_rejects_not_owner() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let (bucket, fixture) = build_bucket_fixture();
    let request = valid_request(8, &bucket, &fixture);

    // The table marks the tree as mapped from a peer — the recipient is not the
    // owner, so the request is rejected before any write happens.
    let mut table = RuntimeTable::default();
    table.set_tree_entry(RuntimeEntry {
        holder: Holder::Mapped(Source::Disk(PathBuf::from("peer-library"))),
        ..RuntimeEntry::default()
    });
    let receipt = handle_proxy_request(&library, &table, ProxyPolicy::Allow, &request);
    assert_eq!(
        receipt,
        ProxyReceipt::Rejected {
            request_id: 8,
            reason: RejectReason::NotOwner,
        }
    );
    assert!(!receipt.is_accepted());
}

#[test]
fn e_2_proxy_rejects_invalid_payload() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    // The bucket is missing the block the tree leaves reference → the commit
    // path fails validation (dangling reference).
    let (_, fixture) = build_bucket_fixture();
    let request = ProxyRequest {
        request_id: 9,
        bucket: Bucket::new(),
        ..valid_request(9, &Bucket::new(), &fixture)
    };

    let receipt = handle_proxy_request(
        &library,
        &RuntimeTable::default(),
        ProxyPolicy::Allow,
        &request,
    );
    assert_eq!(
        receipt,
        ProxyReceipt::Rejected {
            request_id: 9,
            reason: RejectReason::InvalidPayload,
        }
    );
}

#[test]
fn e_2_proxy_rejects_by_policy() {
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let (bucket, fixture) = build_bucket_fixture();
    let request = valid_request(10, &bucket, &fixture);

    // Own target + valid payload, but the owner's policy refuses proxy writes.
    let receipt = handle_proxy_request(
        &library,
        &RuntimeTable::default(),
        ProxyPolicy::Deny,
        &request,
    );
    assert_eq!(
        receipt,
        ProxyReceipt::Rejected {
            request_id: 10,
            reason: RejectReason::ProxyRefused,
        }
    );
}

#[test]
fn e_2_proxy_receipt_variants_and_request_id_echo() {
    let accepted = ProxyReceipt::Accepted { request_id: 1 };
    let rejected = ProxyReceipt::Rejected {
        request_id: 2,
        reason: RejectReason::NotOwner,
    };
    let timed_out = ProxyReceipt::TimedOut { request_id: 3 };

    assert!(accepted.is_accepted());
    assert!(!rejected.is_accepted());
    assert!(!timed_out.is_accepted());
    assert_eq!(accepted.request_id(), 1);
    assert_eq!(rejected.request_id(), 2);
    assert_eq!(timed_out.request_id(), 3);
    assert_eq!(
        rejected,
        ProxyReceipt::Rejected {
            request_id: 2,
            reason: RejectReason::NotOwner,
        }
    );
    assert_ne!(accepted, rejected);
    assert_eq!(rejected.request_id(), 2);
}

#[test]
fn e_2_proxy_request_receipt_byte_roundtrip() {
    // Request byte roundtrip: publish → export → open → receive → equal.
    let (bucket, fixture) = build_bucket_fixture();
    let request = valid_request(42, &bucket, &fixture);
    let (published, handle) = publish_request(&request).unwrap();
    assert_eq!(
        published.bytes(),
        publish_request(&request).unwrap().0.bytes()
    );
    let decoded = receive_request(&SharedRegion::open(handle).unwrap()).unwrap();
    assert_eq!(decoded, request);

    // An IPC-target request roundtrips to the same source variant (the handle
    // itself is a routing hint delivered out-of-band, `02 §6.6`).
    let carrier = SharedRegion::publish(vec![1_u8, 2]).unwrap();
    let ipc_request = ProxyRequest {
        target: Source::Ipc(IpcSource::new(carrier.export().unwrap())),
        ..request
    };
    // The publisher keeps the request region alive while the handle is consumed.
    let (ipc_region, ipc_handle) = publish_request(&ipc_request).unwrap();
    let decoded = receive_request(&SharedRegion::open(ipc_handle).unwrap()).unwrap();
    assert!(matches!(decoded.target, Source::Ipc(_)));
    drop(ipc_region);

    // Receipt byte roundtrip: publish → await on the export handle → restored.
    let receipt = ProxyReceipt::Rejected {
        request_id: 42,
        reason: RejectReason::InvalidPayload,
    };
    let (receipt_region, receipt_handle) = publish_receipt(&receipt).unwrap();
    let restored = await_receipt(&receipt_handle, Duration::from_millis(500));
    assert_eq!(restored, receipt);
    drop(receipt_region);
}

#[test]
fn e_2_proxy_timeout_on_missing_reply() {
    // A handle delivered by `publish_request` for which no receipt is ever
    // published: the waiter scans the leading request id and times out —
    // upstream death manifests as timeout (01 §3.2). The publisher keeps the
    // request region alive (it carries the request, not a reply).
    let (bucket, fixture) = build_bucket_fixture();
    let request = valid_request(55, &bucket, &fixture);
    let (region, handle) = publish_request(&request).unwrap();
    // The request region stays alive and is never replaced by a receipt region.
    let _publisher = region;

    let awaited = await_receipt(&handle, Duration::from_millis(30));
    assert_eq!(awaited, ProxyReceipt::TimedOut { request_id: 55 });
}

#[test]
fn e_2_proxy_client_roundtrip() {
    let temp = tempfile::tempdir().unwrap();
    let owner = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    let (bucket, fixture) = build_bucket_fixture();
    let request = valid_request(99, &bucket, &fixture);

    let client = ProxyClient::with_timeout(Duration::from_secs(1));
    let receipt = client.request(
        &owner,
        &RuntimeTable::default(),
        ProxyPolicy::Allow,
        request,
    );
    assert_eq!(receipt, ProxyReceipt::Accepted { request_id: 99 });
}

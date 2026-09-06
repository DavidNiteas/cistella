//! M11 cross-process shared region roundtrip (host backend: Windows named mapping).

use tree_space::shared::{CustodianChain, SharedRegion, SharedRegionName};

#[test]
fn shared_region_publish_export_open_roundtrip() {
    let payload: Vec<u8> = (0..256).map(|index| (index % 251) as u8).collect();
    let region = SharedRegion::publish(payload.clone()).unwrap();

    // The published bytes are readable without copying.
    assert_eq!(region.bytes(), payload.as_slice());

    // Export a handle and open the same bytes in a "second process".
    let handle = region.export().expect("host backend must support export");
    let opened = SharedRegion::open(handle).unwrap();
    assert_eq!(opened.bytes(), payload.as_slice());
}

#[test]
fn shared_region_bytes_are_borrowed_immutably() {
    let payload = b"tree-space-m11".to_vec();
    let region = SharedRegion::publish(payload.clone()).unwrap();
    let first = region.bytes();
    let second = region.bytes();
    assert_eq!(first, second);
    assert_eq!(first, payload.as_slice());
}

#[test]
fn custodian_chain_promotes_oldest_reader_on_takeover() {
    let payload = b"custodian-chain".to_vec();
    let region = SharedRegion::publish(payload.clone()).unwrap();
    let chain = CustodianChain::new();
    chain.add_reader(region.clone());
    chain.add_reader(SharedRegion::open(region.export().unwrap()).unwrap());

    // Oldest reader takes over custody and still reads the same bytes.
    let custodian = chain.take_over().expect("oldest reader becomes custodian");
    assert_eq!(custodian.bytes(), payload.as_slice());
    assert_eq!(chain.pending_readers(), 1);
}

#[test]
fn region_survives_original_publisher_while_readers_hold_it() {
    let payload = b"tree-space".to_vec();
    // Publish, then clone into multiple readers before dropping the publisher.
    let region = SharedRegion::publish(payload.clone()).unwrap();
    let region_clone = region.clone();
    let handle = region.export().unwrap();
    drop(region); // original publisher is gone

    // The clone and an opened handle both keep the region readable.
    assert_eq!(region_clone.bytes(), payload.as_slice());
    let opened = SharedRegion::open(handle).unwrap();
    assert_eq!(opened.bytes(), payload.as_slice());
}

#[test]
fn shared_region_name_is_portable() {
    assert!(SharedRegionName::new("tree-space-test").is_ok());
    assert!(SharedRegionName::new("").is_err());
}

#[cfg(unix)]
#[test]
fn scm_rights_passes_memfd_fd_between_sockets() {
    use std::os::unix::io::{FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use tree_space::shared::{RegionHandle, SharedRegion, scm};

    let (tx, rx) = UnixStream::pair().unwrap();
    let payload = b"scm-rights-memfd".to_vec();
    let region = SharedRegion::publish(payload.clone()).unwrap();
    let handle = region.export().unwrap();
    let len = handle.len();

    std::thread::scope(|scope| {
        // Sender: pass the memfd descriptor over the socket.
        scope.spawn(move || {
            scm::send_fd(&tx, handle.fd()).unwrap();
        });
        // Receiver: receive the fd, reconstruct the handle, and read zero-copy.
        let received = scm::recv_fd(&rx).unwrap();
        let fd = unsafe { OwnedFd::from_raw_fd(received) };
        let opened = SharedRegion::open(RegionHandle::from_fd(fd, len)).unwrap();
        assert_eq!(opened.bytes(), payload.as_slice());
    });
}

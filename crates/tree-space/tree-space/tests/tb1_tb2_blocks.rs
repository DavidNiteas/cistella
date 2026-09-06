use tree_space::{Blob, Bucket};

#[test]
fn tb2_bucket_deduplicates_verifies_and_collects() {
    let mut bucket = Bucket::new();
    let block = Blob::new(vec![8, 9]);
    let id = bucket.put(&block);
    assert_eq!(bucket.put(&block), id);
    assert_eq!(bucket.len(), 1);
    bucket.verify_all().unwrap();
    let restored = bucket.read(id).unwrap();
    assert_eq!(restored.kind(), tree_space::BlockKind::Blob);
    assert_eq!(restored.as_blob().unwrap().bytes(), &[8, 9]);
    assert_eq!(bucket.gc([]), 1);
    assert!(bucket.is_empty());
}

#[test]
fn tb2_bucket_keeps_root() {
    let mut bucket = Bucket::new();
    let id = bucket.put(&Blob::new(vec![1]));
    assert_eq!(bucket.gc([id]), 0);
}

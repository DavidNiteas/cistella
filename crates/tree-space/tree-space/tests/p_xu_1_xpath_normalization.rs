//! P-XU-1: xpath addressing normalization — `Step::Key` is gone and every
//! unique address has exactly one spelling (`Field` for struct fields and map
//! keys, `Index` for positional entries).
//!
//! Covers: String-keyed and `[u8; 16]`-keyed map block leaves end-to-end
//! (commit → verify clean → reopen → Field-spelled query → typed project byte
//! roundtrip), and the two negative obligations (`[key]` parse error, 0x4b
//! canonical-byte decode error). Spec: `_dev/树与桶管道/01-目标与设计.md` §2.

use arrow::array::Int32Array;
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::collections::BTreeMap;
use std::sync::Arc;
use tree_space::layout::tb::hex16;
use tree_space::tree::AccessOut;
use tree_space::tree::codec::{decode, encode_node};
use tree_space::xpath::{Step, XPath};
use tree_space::{
    ArrowTable, Bucket, TreeCodec, TreeNode, Value, canonical_xpath_bytes,
    xpath_from_canonical_bytes,
};

fn table(ids: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(ids.to_vec()))]).unwrap(),
    )
    .unwrap()
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct StringMapTree {
    name: Value,
    maps: BTreeMap<String, ArrowTable>,
}

#[derive(TreeCodec, TreeNode, Clone, Debug)]
struct KeyedMapTree {
    name: Value,
    keyed: BTreeMap<[u8; 16], ArrowTable>,
}

#[test]
fn p_xu_1_string_map_block_leaves_verify_clean_and_reopen() {
    let alpha = table(&[1, 2]);
    let beta = table(&[3]);
    let mut bucket = Bucket::new();
    let alpha_id = bucket.put(&alpha);
    bucket.put(&beta);

    let tree = StringMapTree {
        name: Value::Utf8("exp".into()),
        maps: BTreeMap::from([("alpha".to_string(), alpha), ("beta".to_string(), beta)]),
    };

    // The typed leaf walk spells map entries as Field steps only (`Step::Key`
    // no longer exists).
    let leaf_refs = tree.leaf_refs();
    assert!(leaf_refs.contains(&(XPath::root().field("maps").field("alpha"), alpha_id,)));
    assert!(leaf_refs.iter().all(|(xpath, _)| {
        xpath
            .steps()
            .iter()
            .all(|step| matches!(step, Step::Field(_) | Step::Index(_)))
    }));

    let tree_bytes = encode_node(&tree).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();

    // Core verification point: verify is clean for a String-keyed map leaf
    // tree — the P-IO-4 false positive is gone because both sides derive the
    // same Field steps.
    let reopened = tree_space::TbLibrary::open(temp.path().join("library")).unwrap();
    assert_eq!(reopened.verify().unwrap(), leaf_refs.len());

    // Reopen index answers the canonical Field spelling `/maps/alpha`.
    let index = reopened.index().unwrap();
    let hit = index
        .get(&XPath::root().field("maps").field("alpha"))
        .unwrap();
    assert_eq!(hit, &alpha_id);
    let parsed = index.get(&XPath::parse("/maps/alpha").unwrap()).unwrap();
    assert_eq!(parsed, &alpha_id);

    // The image re-walk and the typed re-walk agree exactly on canonical bytes.
    let image = decode(&tree_bytes).unwrap();
    let mut walked = tree_space::layout::tb::image_leaf_refs(&image).unwrap();
    walked.sort_by_key(|(xpath, id)| (canonical_xpath_bytes(xpath), id.as_bytes()));
    let mut typed = leaf_refs.clone();
    typed.sort_by_key(|(xpath, id)| (canonical_xpath_bytes(xpath), id.as_bytes()));
    assert_eq!(walked, typed);

    // Typed project roundtrips to byte-identical canonical bytes.
    let rebuilt: StringMapTree = reopened.project().unwrap();
    assert_eq!(encode_node(&rebuilt).unwrap(), tree_bytes);
}

#[test]
fn p_xu_1_bytes16_map_block_leaves_verify_clean_and_reopen() {
    let key_a = [7_u8; 16];
    let key_b = [8_u8; 16];
    let a = table(&[9]);
    let b = table(&[10]);
    let mut bucket = Bucket::new();
    let a_id = bucket.put(&a);
    bucket.put(&b);

    let tree = KeyedMapTree {
        name: Value::Utf8("exp".into()),
        keyed: BTreeMap::from([(key_a, a), (key_b, b)]),
    };

    // bytes16 keys derive a Field step carrying the lowercase 32-char hex name
    // (single injection; previously the derive did not even compile for
    // `BTreeMap<[u8; 16], Table>`).
    let leaf_refs = tree.leaf_refs();
    assert!(leaf_refs.contains(&(XPath::root().field("keyed").field(hex16(&key_a)), a_id,)));

    let tree_bytes = encode_node(&tree).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let library = tree_space::TbLibrary::create(temp.path().join("library")).unwrap();
    library.commit(&tree_bytes, &leaf_refs, &bucket).unwrap();

    let reopened = tree_space::TbLibrary::open(temp.path().join("library")).unwrap();
    assert_eq!(reopened.verify().unwrap(), leaf_refs.len());

    // Field(hex) spelling hits through the reopened index.
    let hex_a = hex16(&key_a);
    let hit = reopened
        .index()
        .unwrap()
        .get(&XPath::root().field("keyed").field(hex_a.clone()))
        .unwrap();
    assert_eq!(hit, &a_id);

    // TreeNode navigation on the typed instance answers the same spelling.
    match tree
        .get(&XPath::root().field("keyed").field(&hex_a))
        .unwrap()
    {
        AccessOut::Ref(id) => assert_eq!(id, a_id),
        other => panic!("expected Ref, got {other:?}"),
    }

    // Typed project roundtrips to byte-identical canonical bytes.
    let rebuilt: KeyedMapTree = reopened.project().unwrap();
    assert_eq!(encode_node(&rebuilt).unwrap(), tree_bytes);
    assert_eq!(rebuilt.keyed.len(), 2);
}

#[test]
fn p_xu_1_key_syntax_parse_is_rejected() {
    // `[...]` now accepts index-only content; the old `[key]` spelling is a
    // parse error.
    for bad in [
        "/maps[alpha]",
        "/maps[k1]",
        "/run/grows[a]",
        "/scenarios[s1]/meta",
        "/m[key]/x",
    ] {
        assert!(XPath::parse(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn p_xu_1_key_byte_0x4b_decode_is_rejected() {
    // A bare 0x4b tag is rejected immediately.
    let error = xpath_from_canonical_bytes(&[0x4b]).unwrap_err();
    assert_eq!(error.code, tree_space::ErrorCode::PayloadMalformed);

    // A formerly valid Key-step byte sequence (`0x4b ++ u64le(len) ++ bytes`)
    // is rejected as well.
    let mut legacy = vec![0x4b, 8, 0, 0, 0, 0, 0, 0, 0];
    legacy.extend_from_slice(b"somekey!");
    let error = xpath_from_canonical_bytes(&legacy).unwrap_err();
    assert_eq!(error.code, tree_space::ErrorCode::PayloadMalformed);
}

//! PL-2 S2: identity golden re-freeze + the cross-encoding identity anchors
//! (`_dev/插件化改造/02-施工路线图.md` §6.4).
//!
//! Three anchors:
//!
//! - `p_l_2_semantic_identity_cross_encoding` (**the M2 closing core**): the
//!   same logical content built through IPC encoding, the Arrow Parquet disk
//!   encoding, and a manual semantic-fingerprint computation yields the same
//!   `RefId`; the same logical tree in different physical row orders yields
//!   the same `TreeId` (semantic identity is physical-encoding independent);
//! - `p_l_2_identity_goldens_refrozen`: the identity-class goldens pass at
//!   their M2 re-frozen values while the address / schema byte classes keep
//!   their historical values (the `ab1`/`a2_1` address-vs-identity separation
//!   structure is kept);
//! - `p_l_2_converter_hook`: `converter(Arrow55, Arrow55)` returns the
//!   built-in identity converter; unregistered combinations return `None`.

use arrow::array::{DictionaryArray, Int32Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use std::sync::Arc;
use tree_space::tree::codec::{ImageContent, TreeImage, encode, named_field, tree_id};
use tree_space::{
    ArrowParquetBlockPlugin, ArrowTable, Blob, Block, BlockKind, BlockPlugin, MemLayout,
    PluginRegistry, RefId, Value, block_blob_address, block_ref_id, semantic_table_value,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex16(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

/// Two-column (id, label) table with a plain Utf8 label column.
fn plain_table(ids: &[i32], labels: &[&str]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![
        Arc::new(Field::new("id", DataType::Int32, false)),
        Arc::new(Field::new("label", DataType::Utf8, false)),
    ]));
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(ids.to_vec())),
                Arc::new(StringArray::from(labels.to_vec())),
            ],
        )
        .unwrap(),
    )
    .unwrap()
}

/// One-column `Int32` table: the P-IO-7.2 parquet sample.
fn one_col_table(rows: &[i32]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int32,
        false,
    )]));
    ArrowTable::try_new(
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(rows.to_vec()))]).unwrap(),
    )
    .unwrap()
}

/// The same logical table with the label column Dictionary-encoded (a
/// different physical layout over the same logical values).
fn dictionary_table(ids: &[i32], labels: &[&str]) -> ArrowTable {
    let schema = Arc::new(Schema::new(vec![
        Arc::new(Field::new("id", DataType::Int32, false)),
        Arc::new(Field::new(
            "label",
            DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8)),
            false,
        )),
    ]));
    let keys = Int32Array::from((0..labels.len() as i32).collect::<Vec<_>>());
    let dictionary = Arc::new(StringArray::from(labels.to_vec())) as Arc<dyn arrow::array::Array>;
    let encoded = DictionaryArray::try_new(keys, dictionary).unwrap();
    ArrowTable::try_new(
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(ids.to_vec())), Arc::new(encoded)],
        )
        .unwrap(),
    )
    .unwrap()
}

/// One logical tree, materialized in two different physical insertion orders
/// (named children reshuffled — the canonical walk reorders them).
fn image_ordered() -> TreeImage {
    TreeImage::new(vec![
        named_field("alpha", ImageContent::Inline(Value::I32(1))),
        named_field("bravo", ImageContent::Ref(RefId::from_bytes([0x11; 16]))),
        named_field("charlie", ImageContent::Inline(Value::Utf8("x".into()))),
    ])
}

fn image_reshuffled() -> TreeImage {
    TreeImage::new(vec![
        named_field("charlie", ImageContent::Inline(Value::Utf8("x".into()))),
        named_field("alpha", ImageContent::Inline(Value::I32(1))),
        named_field("bravo", ImageContent::Ref(RefId::from_bytes([0x11; 16]))),
    ])
}

/// The golden-18 sample tree (`block` Ref + `chunks` Table group of two).
fn golden_18_image() -> TreeImage {
    TreeImage::new(vec![
        named_field("block", ImageContent::Ref(RefId::from_bytes([0x11; 16]))),
        named_field(
            "chunks",
            ImageContent::ChunkGroup {
                kind: BlockKind::Table,
                children: vec![
                    tree_space::tree::codec::positioned_field(ImageContent::ChunkEntry(
                        RefId::from_bytes([0x22; 16]),
                    )),
                    tree_space::tree::codec::positioned_field(ImageContent::ChunkEntry(
                        RefId::from_bytes([0x33; 16]),
                    )),
                ],
            },
        ),
    ])
}

/// The golden-17 sample blob (`b"tb-sample\0"`).
fn sample_blob() -> Blob {
    Blob::new(vec![
        0x74, 0x62, 0x2d, 0x73, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x00,
    ])
}

/// M2 closing core: the same logical content builds the same `RefId` /
/// `TreeId` through every physical encoding path.
#[test]
fn p_l_2_semantic_identity_cross_encoding() {
    let plain = plain_table(&[1, 2, 3], &["a", "b", "c"]);

    // Path 1: the canonical IPC payload through `block_ref_id` (the semantic
    // formula resolves the batch from the canonical bytes).
    let ipc_ref = block_ref_id(BlockKind::Table, &plain.payload());

    // Path 2: the Arrow Parquet disk encoding — the plugin resolves the batch
    // from the Parquet payload and must agree with the IPC path.
    let parquet_frame = ArrowParquetBlockPlugin
        .encode(&plain.envelope())
        .expect("table block encodes as Parquet");
    let parquet_payload = &parquet_frame[11..]; // envelope framing is not identity
    let parquet_ref = ArrowParquetBlockPlugin.ref_id(BlockKind::Table, parquet_payload);
    assert_eq!(
        ipc_ref, parquet_ref,
        "IPC and Parquet physical encodings of the same logical table agree"
    );

    // Path 3: a Dictionary-encoded physical layout of the same logical values
    // (a future Polars-style encoding stand-in).
    let dictionary = dictionary_table(&[1, 2, 3], &["a", "b", "c"]);
    let dictionary_ref = block_ref_id(BlockKind::Table, &dictionary.payload());
    assert_eq!(
        ipc_ref, dictionary_ref,
        "dictionary-encoded layout of the same logical table agrees"
    );

    // Path 4: the manual semantic-fingerprint computation (M2 cross-encoding
    // anchor — the fingerprint is the sole identity input).
    let fingerprint = semantic_table_value(plain.as_batch()).expect("sample table semanticizes");
    let manual = RefId(tree_space::hash::canonical_digest(
        &BlockKind::Table.domain(),
        [fingerprint.encode()],
    ));
    assert_eq!(
        ipc_ref, manual,
        "the semantic formula is exactly the fingerprint digest"
    );
    assert_eq!(
        hex(&ipc_ref.as_bytes()),
        "a7370f0f8f36fb637e57eab707ae160d",
        "M2 cross-encoding RefId golden (same logical table, any encoding)"
    );

    // The P-IO-7.2 one-column sample pins the same cross-encoding promise at
    // the frozen parquet identity.
    let one_col = one_col_table(&[1, 2, 3]);
    assert_eq!(
        hex(&block_ref_id(BlockKind::Table, &one_col.payload()).as_bytes()),
        "4ee1fd39d16713d6aedcbb37056c6b65",
        "P-IO-7.2 sample-table semantic identity (refrozen-m2)"
    );

    // TreeId: the canonical encoder already normalizes the physical row order
    // (order_children sorts named/keyed children), so the two insertion orders
    // above encode to the same bytes — the semantic-walk guarantee is asserted
    // as formula equivalence: `tree_id(bytes) == hash(tb-tree,
    // [semantic_tree(image)])` (02 §6.4; the storage order attaches to the
    // image, never to the identity).
    let tree_bytes = encode(&image_ordered()).unwrap();
    let tree_bytes_reshuffled = encode(&image_reshuffled()).unwrap();
    assert_eq!(
        tree_bytes, tree_bytes_reshuffled,
        "the codec's canonical ordering normalizes the insertion order"
    );
    let walk = tree_space::semantic_tree(&image_ordered());
    let manual_tree = tree_space::TreeId::from_bytes(
        tree_space::hash::canonical_digest(b"tb-tree", [walk]).as_bytes(),
    );
    assert_eq!(
        tree_id(&tree_bytes),
        manual_tree,
        "the tree identity formula is exactly the semantic-walk digest"
    );

    // The tree closure standard: identity ⊥ address on the tree side too.
    let tree_bytes = encode(&golden_18_image()).unwrap();
    assert_eq!(
        hex(&tree_id(&tree_bytes).as_bytes()),
        "2a34434f6038528a916fe19d506a8438",
        "M2 golden 18 TreeId (re-frozen at the semantic formula)"
    );
    assert_ne!(
        hex16("daeccfca911e826a3609331e7cd83f18"),
        tree_id(&tree_bytes).as_bytes(),
        "tree-blob addressing stays independent of the TreeId"
    );
}

/// The identity-class goldens pass at their M2 values; the address/schema
/// byte classes keep their historical values untouched.
#[test]
fn p_l_2_identity_goldens_refrozen() {
    // Identity class (M2 re-frozen values; 02 §6.2/§6.4).
    assert_eq!(
        hex(&block_ref_id(
            BlockKind::Table,
            &tree_space::block::Table::default().payload()
        )
        .as_bytes()),
        "1c6c9cb0056fc09ed09f02dc3e033560",
        "golden 1 (refrozen-m2): empty Table"
    );
    assert_eq!(
        hex(&sample_blob().ref_id().as_bytes()),
        "d647966165bab1e26a2cbec5ec794735",
        "golden 17 RefId (refrozen-m2)"
    );

    // Address / schema byte classes keep their historical values
    // (17/18/22/23 addresses, 19/24 commit bytes + id, P-IO-7.2 parquet
    // address — 02 §6.2 keeps them untouched, the derived assertions below
    // reuse the `ab1`/`a2_1` address-vs-identity separation structure).
    let blob_frame = sample_blob().envelope().encode();
    assert_eq!(
        hex(&block_blob_address(&blob_frame).as_bytes()),
        "d796cdd97c2ecac5dbeb3cc6ac9dc605",
        "golden 17 address stays frozen"
    );
    assert_ne!(
        hex16("d796cdd97c2ecac5dbeb3cc6ac9dc605"),
        sample_blob().ref_id().as_bytes(),
        "storage addressing stays independent of the semantic RefId"
    );

    let tree_bytes = encode(&golden_18_image()).unwrap();
    assert_eq!(
        hex(&tree_space::tree_blob_address(&tree_bytes).as_bytes()),
        "daeccfca911e826a3609331e7cd83f18",
        "golden 18 tree-blob address stays frozen"
    );

    // Golden 19/24 commit ids: the five-part formula is untouched (only the
    // tree reference rows / addresses feed it).
    let (parent, root, tree_blob, refs) = ([0x0a; 16], [0x1a; 16], [0x2a; 16], [0x3a; 16]);
    assert_eq!(
        tree_space::layout::tb::tb_commit_id(9, parent, root, tree_blob, refs).to_string(),
        "ea454fc52da0fdc1bcce099eb7f75c9e",
        "golden 19/24 commit id stays frozen"
    );

    // P-IO-7.2 parquet address stays frozen.
    assert_eq!(
        hex(&block_blob_address(
            &ArrowParquetBlockPlugin
                .encode(&one_col_table(&[1, 2, 3]).envelope())
                .unwrap()
        )
        .as_bytes()),
        "efaf05e1754df32a1ebec16a0a5a881d",
        "parquet storage address stays frozen"
    );

    // Digest protocol constants stay frozen.
    assert_eq!(
        hex(&tree_space::hash::empty_domain_digest().as_bytes()),
        "855652511297084ea790a61be3641b2e",
        "digest protocol: empty-domain digest"
    );
}

/// The M2 converter hook: registry-converter registration, identity
/// conversion, and the none-for-unregistered-combinations contract.
#[test]
fn p_l_2_converter_hook() {
    let registry = tree_space::PluginRegistry::global();

    // `converter(Arrow55, Arrow55)` is always available (built-in identity).
    let identity = registry
        .converter(MemLayout::Arrow55, MemLayout::Arrow55)
        .expect("the same-version identity converter is auto-registered");
    let image = TreeImage::new(vec![named_field("x", ImageContent::Inline(Value::I32(1)))]);
    assert_eq!(
        identity.convert_tree(&image).unwrap(),
        image,
        "identity tree conversion returns the input"
    );
    let envelope = sample_blob().envelope();
    assert_eq!(
        identity.convert_block(&envelope).unwrap(),
        envelope,
        "identity block conversion returns the input"
    );

    // An empty registry has no converters at all.
    let empty = PluginRegistry::empty();
    assert!(
        empty
            .converter(MemLayout::Arrow55, MemLayout::Arrow55)
            .is_none(),
        "unregistered (from, to) combination → None"
    );

    // A second converter for the same (from, to) pair is a TypeConflict.
    let mut custom = PluginRegistry::empty();
    custom
        .register_converter(Arc::new(tree_space::IdentityMemConverter::for_layout(
            MemLayout::Arrow55,
        )))
        .expect("first registration is accepted");
    let conflict = custom.register_converter(Arc::new(
        tree_space::IdentityMemConverter::for_layout(MemLayout::Arrow55),
    ));
    assert_eq!(
        conflict.unwrap_err().code,
        tree_space::ErrorCode::TypeConflict,
        "duplicate (from, to) converter registration"
    );
    assert!(
        custom
            .converter(MemLayout::Arrow55, MemLayout::Arrow55)
            .is_some()
    );
}

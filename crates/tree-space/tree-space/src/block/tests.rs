use super::*;
#[test]
fn tb1_value_roundtrip_and_order() {
    let values = vec![
        Value::Null,
        Value::Bool(true),
        Value::I64(-4),
        Value::Utf8("x".into()),
        Value::Binary(vec![1, 2]),
        Value::Timestamp(TimestampUnit::Nanosecond, Some("UTC".into()), 7),
        Value::Decimal256(Decimal256 {
            precision: 10,
            scale: -2,
            value: [3; 32],
        }),
    ];
    for value in values {
        let sequence = Sequence::new(vec![value.clone()]);
        assert_eq!(
            Sequence::decode(&sequence.encode()).unwrap().values(),
            &[value]
        );
    }
    assert!(Value::I8(1) < Value::I8(2));
}

#[test]
fn tb1_value_arrow_payloads_roundtrip() {
    let values = [
        Value::Null,
        Value::Bool(true),
        Value::I8(-1),
        Value::I16(-2),
        Value::I32(-3),
        Value::I64(-4),
        Value::U8(5),
        Value::U16(6),
        Value::U32(7),
        Value::U64(8),
        Value::F32(0x3fc00000),
        Value::F64(0xc004000000000000),
        Value::Utf8("x".into()),
        Value::Binary(vec![1, 2]),
        Value::Date32(9),
        Value::Date64(10),
        Value::Time32(Time32Unit::Millisecond, 11),
        Value::Time64(Time64Unit::Nanosecond, 12),
        Value::Timestamp(TimestampUnit::Nanosecond, Some("UTC".into()), 13),
        Value::Duration(DurationUnit::Microsecond, 14),
        Value::Decimal128(Decimal128 {
            precision: 10,
            scale: -2,
            value: [15; 16],
        }),
        Value::Decimal256(Decimal256 {
            precision: 20,
            scale: -3,
            value: [16; 32],
        }),
    ];
    for value in values {
        let sequence = Sequence::new(vec![value.clone()]);
        assert_eq!(
            Sequence::decode(&sequence.encode()).unwrap().values(),
            &[value]
        );
    }
}

#[test]
fn tb1_arrow_payloads_use_ipc_and_preserve_heterogeneous_values() {
    let values = vec![
        Value::I32(1),
        Value::Utf8("x".into()),
        Value::Timestamp(TimestampUnit::Nanosecond, Some("UTC".into()), 3),
        Value::Decimal128(Decimal128 {
            precision: 10,
            scale: -2,
            value: [4; 16],
        }),
    ];
    let sequence = Sequence::new(values.clone());
    assert!(sequence.encode().starts_with(b"ARROW1"));
    assert_eq!(
        Sequence::decode(&sequence.encode()).unwrap().values(),
        values
    );

    let kv = Kv::try_new(vec![
        (Value::Utf8("b".into()), Value::I32(2)),
        (Value::Utf8("a".into()), Value::I32(1)),
    ])
    .unwrap();
    assert!(kv.encode().starts_with(b"ARROW1"));
    assert_eq!(Kv::decode(&kv.encode()).unwrap(), kv);

    let blob = Blob::new(b"payload".to_vec());
    assert!(blob.payload().starts_with(b"ARROW1"));
    assert_eq!(
        Blob::new(super::arrow::decode_blob(&blob.payload()).unwrap()),
        blob
    );
}
#[test]
fn tb1_sequence_and_kv_are_canonical() {
    let seq = Sequence::new(vec![Value::Null, Value::U16(3)]);
    assert_eq!(Sequence::decode(&seq.encode()).unwrap(), seq);
    let kv = Kv::try_new(vec![
        (Value::Utf8("b".into()), Value::I32(2)),
        (Value::Utf8("a".into()), Value::I32(1)),
    ])
    .unwrap();
    assert_eq!(Kv::decode(&kv.encode()).unwrap(), kv);
    assert!(Kv::try_new(vec![(Value::Null, Value::Null)]).is_err());
}

#[test]
fn tb1_arrow_payload_decode_rejects_truncated_or_wrong_schema() {
    assert!(Sequence::decode(&[0, 1, 2, 3]).is_err());
    let mut payload = Sequence::new(vec![Value::I32(1)]).encode();
    payload.push(0);
    assert!(Sequence::decode(&payload).is_err());
}
#[test]
fn tb1_envelope_identity_excludes_framing() {
    let blob = Blob::new(vec![1, 2, 3]);
    let env = blob.envelope();
    assert_eq!(blob.ref_id(), block_ref_id(BlockKind::Blob, &env.payload));
    assert_eq!(Envelope::decode(&env.encode()).unwrap(), env);
}

#[test]
fn ab1_identity_goldens_are_frozen() {
    // PL-2 M2 re-freeze (02 §6.4): the identity formula switched from the
    // canonical-byte digest to the semantic formula (01 §4-4); the block
    // kinds 1–4 are now the semantic values of the empty payloads.
    assert_eq!(
        block_ref_id(BlockKind::Table, &Table::default().payload()),
        RefId::from_bytes(hex("1c6c9cb0056fc09ed09f02dc3e033560")),
    );
    assert_eq!(
        Sequence::new(Vec::<Value>::new()).ref_id(),
        RefId::from_bytes(hex("ddf9ce63037f24a8f605b6f630848176")),
    );
    assert_eq!(
        Kv::try_new(Vec::<(Value, Value)>::new()).unwrap().ref_id(),
        RefId::from_bytes(hex("82bdddacfd3099c21048930ad865cdef")),
    );
    assert_eq!(
        Blob::new(Vec::<u8>::new()).ref_id(),
        RefId::from_bytes(hex("5a9fa702e7a3f651e83b67167fad78b8")),
    );
}

fn hex(value: &str) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    bytes
}

#[test]
fn tb1_typed_sequence_and_kv_validation() {
    let seq = Sequence::new(vec![Value::I32(1), Value::I32(2)]);
    assert_eq!(seq.validate_kind(ValueKind::I32), Ok(()));
    assert!(seq.validate_kind(ValueKind::I64).is_err());
    assert_eq!(seq.clone().into_typed(ValueKind::I32).unwrap().len(), 2);

    let kv = Kv::try_new(vec![(Value::Utf8("k".into()), Value::Bool(true))]).unwrap();
    assert_eq!(kv.validate_kinds(ValueKind::Utf8, ValueKind::Bool), Ok(()));
    assert!(kv.validate_kinds(ValueKind::Utf8, ValueKind::I32).is_err());
}

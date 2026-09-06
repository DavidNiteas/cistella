//! N-5 xpath parsing: canonical form, steps, and rejection cases.

use tree_space::xpath::{Step, XPath};

#[test]
fn xpath_parse_simple() {
    let path = XPath::parse("/a/b/c").expect("parse");
    assert_eq!(
        path.steps(),
        &[
            Step::Field("a".into()),
            Step::Field("b".into()),
            Step::Field("c".into())
        ]
    );
}

#[test]
fn xpath_parse_index() {
    let path = XPath::parse("/runs[3]/x").expect("parse");
    assert_eq!(
        path.steps(),
        &[
            Step::Field("runs".into()),
            Step::Index(3),
            Step::Field("x".into())
        ]
    );
}

#[test]
fn xpath_parse_rejects_key_syntax() {
    // P-XU: `[key]` is no longer a valid spelling — only `[i]` indices are.
    for bad in [
        "/scenarios[s1]/meta",
        "/scenarios/k1[x]",
        "/map[key]",
        "/map[three]",
        "/run/grows[a]",
    ] {
        assert!(XPath::parse(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn xpath_parse_root_and_empty() {
    assert!(XPath::parse("").expect("parse").is_root());
    assert!(XPath::parse("/").is_err(), "bare '/' has an empty segment");
}

#[test]
fn xpath_parse_rejects_invalid() {
    for bad in [
        "a/b", "//a", "/a//b", "/a/b[", "/[3]", "/a[3", "/a[b]", "/a[3][4]",
    ] {
        assert!(XPath::parse(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn xpath_display_is_canonical() {
    let original = "/runs[3]/x";
    assert_eq!(XPath::parse(original).unwrap().to_string(), original);
    // A map entry is a plain Field step under its map field.
    let keyed = "/scenarios/s1/meta";
    assert_eq!(XPath::parse(keyed).unwrap().to_string(), keyed);
}

#[test]
fn xpath_join_appends_steps() {
    let a = XPath::parse("/runs[2]").unwrap();
    let b = XPath::parse("/x/y").unwrap();
    let joined = a.join(&b);
    assert_eq!(
        joined.steps(),
        &[
            Step::Field("runs".into()),
            Step::Index(2),
            Step::Field("x".into()),
            Step::Field("y".into())
        ]
    );
}

#[test]
fn xpath_take_first_splits() {
    let path = XPath::parse("/a/b").unwrap();
    let (head, rest) = path.take_first().expect("has head");
    assert_eq!(head, Step::Field("a".into()));
    assert_eq!(rest.to_string(), "/b");
    assert!(rest.take_first().is_some());
    let (_, tail) = rest.take_first().unwrap();
    assert!(tail.is_root());
}

#[test]
fn xpath_builders() {
    let built = XPath::root().field("runs").index(0).field("x");
    assert_eq!(built.to_string(), "/runs[0]/x");
    // Map keys are appended as plain Field steps (`.key()` no longer exists).
    let keyed = XPath::root().field("scenarios").field("k1");
    assert_eq!(keyed.to_string(), "/scenarios/k1");
}

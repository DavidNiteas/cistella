//! M0 compile-contract gate. The actual compiler job is executed by CI/Pixi with Rust 1.95.

#[test]
fn manifest_freezes_msrv_and_wire_dependencies() {
    let cargo = include_str!("../Cargo.toml");
    assert!(cargo.contains("rust-version = \"1.95\""));
    assert!(cargo.contains("arrow = { version = \"55\""));
    // File locking uses std::fs::File (MSRV 1.95); fs4 must not be reintroduced.
    assert!(!cargo.contains("fs4"));
}

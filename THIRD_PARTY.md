# Third-party vendored crates

The `crates/tree-space/` subtree is a local vendored snapshot of the upstream
`tree-space` project and its immediate companion crates:

- `crates/tree-space/tree-space`
- `crates/tree-space/tree-space-derive`
- `crates/tree-space/perfkit`

These crates are kept inside this repository so cistella can build without an
external sibling checkout.

fn main() {
    // bin_test/release builds must pick up frontend-only edits.  Tauri embeds
    // frontendDist at compile time, so explicitly watch both the source tree
    // and the generated dist directory to force the GUI binary to refresh when
    // a UI fix is made without Rust changes.
    println!("cargo:rerun-if-changed=../src");
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rerun-if-changed=../package.json");
    println!("cargo:rerun-if-changed=../vite.config.ts");
    tauri_build::build()
}

// Embeds tasca.manifest (Per-Monitor-V2 DPI awareness) into the binary with the
// MSVC linker itself, so no resource-compiler crate is needed. Bins only: the
// test harnesses don't get it.
fn main() {
    println!("cargo:rerun-if-changed=tasca.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}\\tasca.manifest");
    }
}

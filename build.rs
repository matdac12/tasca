// The icon is the only resource compiled by rc.exe. The DPI manifest stays on
// the linker line below (/MANIFEST:EMBED), so the exe keeps exactly one
// manifest and gains the icon.
fn main() {
    println!("cargo:rerun-if-changed=tasca.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}\\tasca.manifest");
        println!("cargo:rerun-if-changed=assets/tasca.rc");
        println!("cargo:rerun-if-changed=assets/tasca.ico");
        embed_resource::compile_for("assets/tasca.rc", ["tasca"], embed_resource::NONE)
            .manifest_optional()
            .expect("compile assets/tasca.rc");
    }
}

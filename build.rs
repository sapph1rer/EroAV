// Puts libmpv-2.dll (from ../rmp/third_party/libmpv) next to the built exe so the player works from `cargo run`.
fn main() {
    let src = std::path::Path::new("../rmp/third_party/libmpv/libmpv-2.dll");
    println!("cargo:rerun-if-changed={}", src.display());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<profile>/build/<pkg>/out
    if let Some(dir) = out.ancestors().nth(3) {
        let dst = dir.join("libmpv-2.dll");
        if src.exists() && !dst.exists() {
            let _ = std::fs::copy(src, dst);
        }
    }
}

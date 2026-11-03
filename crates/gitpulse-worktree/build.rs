fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // Cargo scopes `cargo:rustc-link-arg` to the defining package's own
        // final link targets, so linking `gitpulse_lib` requires mirroring its
        // Swift runtime rpath and weak FoundationModels link here.
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,FoundationModels");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}

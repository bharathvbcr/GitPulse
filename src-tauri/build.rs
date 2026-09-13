fn main() {
    embed_test_manifest();
    build_apple_intelligence();
    // tauri-winres links its .res only as `rustc-link-arg-bins`. That .res
    // also carries RT_MANIFEST when the default app manifest is included, so
    // a second `/MANIFEST:EMBED` on bins is CVT1100 (duplicate resource).
    // Omit the default from the .res and embed comctl32 once via catch-all
    // link args, which is the only cargo instruction that reaches
    // `unittests src/lib.rs`.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest()),
    )
    .unwrap_or_else(|error| panic!("error found during tauri-build: {error}"));
}

/// Gives every Windows MSVC artifact of this crate an application manifest
/// declaring the comctl32 version 6 dependency.
///
/// Without it, two integration suites died on Windows with
/// STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) before `main` ran, so nothing in
/// them was ever checked. Only those two reach the window code in `muda` and
/// `wry`, which is why only they failed.
///
/// What was verified on the runner: the failing binaries import comctl32
/// alongside the rest of the GUI stack, and every DLL they import resolves in
/// System32 -- so the loader was failing on a missing EXPORT, not a missing
/// DLL. Embedding this manifest makes both suites load and pass.
///
/// What was NOT pinned down: which single import was unresolvable. The
/// manifest decides which comctl32 the loader binds -- the 5.82 copy in
/// System32, or the version 6 assembly in the side-by-side store -- and the
/// three subclassing functions those crates call by name (`SetWindowSubclass`,
/// `RemoveWindowSubclass`, `DefSubclassProc`) are exported by the System32
/// copy too, so the unresolved import is something else. Naming it would take
/// another run against a Windows host; the fix is the same either way.
///
/// `rustc-link-arg-tests` covers integration binaries only. `tauri-winres`
/// covers named bins only. The lib unit-test harness is a third executable
/// (`unittests src/lib.rs`) and still died at load with 0xc0000139 after
/// those suites passed. Catch-all `rustc-link-arg` reaches that harness.
/// Do not also pass `/MANIFEST:EMBED` as `-bins` while tauri-winres still
/// ships RT_MANIFEST: that is CVT1100 on `gitpulse.exe`.
fn embed_test_manifest() {
    println!("cargo:rerun-if-changed=tests.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests.manifest");
    let path = manifest.display();
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{path}");
}

/// Compiles the Swift Foundation Models bridge into a static archive and links it.
///
/// Sets `cfg(apple_intelligence)` on success and nothing on failure, so the Rust
/// side can tell "this binary has no bridge" from "the bridge is here and the
/// framework says no" — two different answers a reader must never see merged.
///
/// Three things here are easy to get wrong and each fails in a way that does not
/// name its cause:
///
///   1. `xcrun --show-sdk-path` with no `--sdk` returns the Command Line Tools
///      SDK, which has no usable `FoundationModels.framework`. Asking that way
///      looks exactly like "this Mac has no Apple Intelligence". Always
///      `--sdk macosx`.
///   2. The Swift is built at its own floor (`SWIFT_DEPLOYMENT_TARGET`), NOT the
///      crate's. FoundationModels needs macOS 26 anyway, so nothing is lost —
///      and building lower drags in the back-deployment shims
///      (`libswiftCompatibility56.a` and friends, which live beside the
///      *compiler* and never in the SDK). Measured: at 11.0 the archive has two
///      undefined `swiftCompatibility` symbols, at 26.0 it has none. Avoiding
///      them means one less `-L` and, more importantly, no compatibility
///      shims installing global-executor hooks process-wide.
///   3. The framework is weak-linked explicitly. A plain
///      `rustc-link-lib=framework=` is an `LC_LOAD_DYLIB`, and a hard
///      dependency on a macOS-26-only framework stops the app launching at all
///      on anything older. The linker happens to weak-link it automatically
///      while the crate's deployment target is below 26, so this is belt and
///      braces against that changing silently — verify with
///      `otool -l | grep -B2 FoundationModels` showing `LC_LOAD_WEAK_DYLIB`.
///   4. The build must degrade rather than break. A machine without Xcode, or
///      with an older SDK, still has to produce a working GitPulse — just one
///      without on-device drafting.
///
/// `GITPULSE_DISABLE_APPLE_INTELLIGENCE=1` forces the bridge-less build, which is
/// how the "not compiled in" path gets tested. Use a separate `CARGO_TARGET_DIR`
/// for that, or each configuration invalidates the other's cached build.
fn build_apple_intelligence() {
    println!("cargo:rustc-check-cfg=cfg(apple_intelligence)");
    println!("cargo:rerun-if-changed=apple/GitPulseAppleIntelligence.swift");
    println!("cargo:rerun-if-env-changed=GITPULSE_DISABLE_APPLE_INTELLIGENCE");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    if std::env::var("GITPULSE_DISABLE_APPLE_INTELLIGENCE").is_ok_and(|value| value != "0") {
        println!("cargo:warning=Apple Intelligence bridge disabled by GITPULSE_DISABLE_APPLE_INTELLIGENCE");
        return;
    }
    match compile_apple_bridge() {
        Ok(()) => println!("cargo:rustc-cfg=apple_intelligence"),
        // A warning, never a panic: the rest of the app does not depend on this.
        Err(error) => {
            println!("cargo:warning=Apple Intelligence bridge not built ({error}); on-device drafting will report itself unavailable")
        }
    }
}

fn compile_apple_bridge() -> Result<(), String> {
    use std::process::Command;

    let out_dir = std::env::var("OUT_DIR").map_err(|_| "OUT_DIR is unset".to_string())?;
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("apple")
        .join("GitPulseAppleIntelligence.swift");
    if !source.is_file() {
        return Err(format!("{} is missing", source.display()));
    }

    let sdk = xcrun(&["--sdk", "macosx", "--show-sdk-path"])?;
    let framework =
        std::path::Path::new(&sdk).join("System/Library/Frameworks/FoundationModels.framework");
    if !framework.is_dir() {
        return Err(format!("{} has no FoundationModels.framework", sdk));
    }

    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(other) => return Err(format!("unsupported macOS arch {other}")),
        Err(_) => return Err("CARGO_CFG_TARGET_ARCH is unset".into()),
    };
    // FoundationModels is macOS 26 only, so the bridge has nothing to gain from
    // a lower floor — and a lower floor is what pulls in the back-deployment
    // shims. Every 26-only API inside the Swift sits behind `if #available`, so
    // the archive still links into a binary whose own target is older.
    const SWIFT_DEPLOYMENT_TARGET: &str = "26.0";
    let archive = std::path::Path::new(&out_dir).join("libgitpulse_apple_intelligence.a");

    let status = Command::new("xcrun")
        .args(["--sdk", "macosx", "swiftc", "-emit-library", "-static"])
        .arg("-o")
        .arg(&archive)
        .args(["-module-name", "GitPulseAppleIntelligence"])
        .arg("-target")
        .arg(format!("{arch}-apple-macosx{SWIFT_DEPLOYMENT_TARGET}"))
        .arg("-sdk")
        .arg(&sdk)
        .arg("-O")
        .arg(&source)
        .status()
        .map_err(|error| format!("could not run swiftc: {error}"))?;
    if !status.success() {
        return Err(format!("swiftc exited with {status}"));
    }

    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    println!("cargo:rustc-link-lib=static=gitpulse_apple_intelligence");
    // Weak, so a Mac older than 26 still launches: the framework simply is not
    // there and the bridge reports itself unavailable.
    println!("cargo:rustc-link-arg=-Wl,-weak_framework,FoundationModels");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    Ok(())
}

/// Runs `xcrun` and returns its trimmed stdout.
fn xcrun(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("xcrun")
        .args(args)
        .output()
        .map_err(|error| format!("could not run xcrun {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "xcrun {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err(format!("xcrun {} returned nothing", args.join(" ")));
    }
    Ok(text)
}

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);

    // The grammar versions are stamped only when the grammars are compiled in.
    //
    // They are read back by `cache.rs`, which is `#[cfg(feature = "parse")]`
    // and is their only consumer. Emitting them unconditionally meant this
    // build script read the *workspace's* `Cargo.lock` and failed unless all
    // thirty tree-sitter packages were resolved in it — with `parse` off, when
    // none of them is a dependency at all.
    //
    // That made the feature only half a feature. `--no-default-features` built
    // fine inside this workspace, whose lockfile happens to carry the grammars
    // for the other members, and could not build anywhere else: a consumer that
    // vendored or copied this crate to answer queries about a persisted map hit
    // "tree-sitter-python is absent from …/Cargo.lock" for a package it had
    // deliberately excluded. The point of the feature is that such a consumer
    // needs none of this.
    if env::var_os("CARGO_FEATURE_PARSE").is_some() {
        stamp_grammar_versions(&manifest_dir)?;
    }

    build_vendored_grammars(&manifest_dir)?;
    Ok(())
}

/// Records the resolved version of every grammar, for the extraction cache key.
fn stamp_grammar_versions(manifest_dir: &Path) -> Result<(), Box<dyn Error>> {
    let lock_path = manifest_dir.join("../..").join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());

    let lock = fs::read_to_string(&lock_path)?;
    let versions = package_versions(&lock);
    for (package, env_name) in [
        ("tree-sitter-python", "DEVMAP_GRAMMAR_PYTHON_VERSION"),
        (
            "tree-sitter-javascript",
            "DEVMAP_GRAMMAR_JAVASCRIPT_VERSION",
        ),
        (
            "tree-sitter-typescript",
            "DEVMAP_GRAMMAR_TYPESCRIPT_VERSION",
        ),
        ("tree-sitter-rust", "DEVMAP_GRAMMAR_RUST_VERSION"),
        ("tree-sitter-go", "DEVMAP_GRAMMAR_GO_VERSION"),
        ("tree-sitter-hcl", "DEVMAP_GRAMMAR_HCL_VERSION"),
        ("tree-sitter-astro-next", "DEVMAP_GRAMMAR_ASTRO_VERSION"),
        ("tree-sitter-kotlin-ng", "DEVMAP_GRAMMAR_KOTLIN_VERSION"),
        ("tree-sitter-svelte-ng", "DEVMAP_GRAMMAR_SVELTE_VERSION"),
        ("tree-sitter-java", "DEVMAP_GRAMMAR_JAVA_VERSION"),
        ("tree-sitter-c-sharp", "DEVMAP_GRAMMAR_CSHARP_VERSION"),
        ("tree-sitter-php", "DEVMAP_GRAMMAR_PHP_VERSION"),
        ("tree-sitter-ruby", "DEVMAP_GRAMMAR_RUBY_VERSION"),
        ("tree-sitter-c", "DEVMAP_GRAMMAR_C_VERSION"),
        ("tree-sitter-cpp", "DEVMAP_GRAMMAR_CPP_VERSION"),
        ("tree-sitter-objc", "DEVMAP_GRAMMAR_OBJC_VERSION"),
        ("tree-sitter-cuda", "DEVMAP_GRAMMAR_CUDA_VERSION"),
        ("tree-sitter-swift", "DEVMAP_GRAMMAR_SWIFT_VERSION"),
        ("tree-sitter-scala", "DEVMAP_GRAMMAR_SCALA_VERSION"),
        ("tree-sitter-dart", "DEVMAP_GRAMMAR_DART_VERSION"),
        ("tree-sitter-pascal", "DEVMAP_GRAMMAR_PASCAL_VERSION"),
        ("tree-sitter-lua", "DEVMAP_GRAMMAR_LUA_VERSION"),
        ("tree-sitter-luau", "DEVMAP_GRAMMAR_LUAU_VERSION"),
        ("tree-sitter-r", "DEVMAP_GRAMMAR_R_VERSION"),
        ("tree-sitter-cfml", "DEVMAP_GRAMMAR_CFML_VERSION"),
        ("tree-sitter-erlang", "DEVMAP_GRAMMAR_ERLANG_VERSION"),
        ("tree-sitter-solidity", "DEVMAP_GRAMMAR_SOLIDITY_VERSION"),
        ("tree-sitter-nix", "DEVMAP_GRAMMAR_NIX_VERSION"),
        ("tree-sitter-bash", "DEVMAP_GRAMMAR_BASH_VERSION"),
        ("tree-sitter-sequel", "DEVMAP_GRAMMAR_SQL_VERSION"),
    ] {
        let Some(version) = versions.get(package) else {
            return Err(format!("{package} is absent from {}", lock_path.display()).into());
        };
        println!("cargo:rustc-env={env_name}={version}");
    }
    Ok(())
}

/// Compile grammars vendored as C source.
///
/// Some languages have no Rust binding crate that works with a current
/// tree-sitter: the published wrappers pin tree-sitter 0.20, whose `Language`
/// type is a different type from ours, so they cannot be linked at all. The
/// *generated parser* is fine — `parser.c` emits ABI 13/14, which the 0.25
/// runtime accepts — so only the stale Rust wrapper was ever the problem.
/// Compiling the C directly and declaring the entry point ourselves sidesteps
/// it and keeps the grammar on the same runtime as every other one.
fn build_vendored_grammars(manifest_dir: &Path) -> Result<(), Box<dyn Error>> {
    let vendor = manifest_dir.join("../../vendor/grammars");
    if !vendor.is_dir() {
        return Ok(());
    }
    // Watch the *root*, not just the grammar directories found on this run:
    // registering only the latter means adding a new grammar never re-triggers
    // the build script, so its C is silently never compiled and the link fails
    // with an undefined symbol.
    println!("cargo:rerun-if-changed={}", vendor.display());
    for entry in fs::read_dir(&vendor)? {
        let dir = entry?.path();
        let parser = dir.join("parser.c");
        if !parser.is_file() {
            continue;
        }
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("vendored grammar directory has no name")?
            .to_string();
        println!("cargo:rerun-if-changed={}", dir.display());

        let mut build = cc::Build::new();
        build.include(&dir).warnings(false).opt_level(2);
        build.file(&parser);
        // An external scanner is optional and may be C or C++.
        for scanner in ["scanner.c", "scanner.cc"] {
            let path = dir.join(scanner);
            if path.is_file() {
                if scanner.ends_with(".cc") {
                    let mut cpp = cc::Build::new();
                    cpp.cpp(true)
                        .include(&dir)
                        .warnings(false)
                        .opt_level(2)
                        .file(&path)
                        .compile(&format!("tree_sitter_{name}_scanner"));
                } else {
                    build.file(&path);
                }
            }
        }
        build.compile(&format!("tree_sitter_{name}"));
    }
    Ok(())
}

fn package_versions(lock: &str) -> BTreeMap<String, String> {
    let mut versions = BTreeMap::new();
    let mut name: Option<String> = None;
    for line in lock.lines() {
        if line == "[[package]]" {
            name = None;
            continue;
        }
        if let Some(value) = line
            .strip_prefix("name = \"")
            .and_then(|v| v.strip_suffix('"'))
        {
            name = Some(value.to_string());
            continue;
        }
        if let Some(version) = line
            .strip_prefix("version = \"")
            .and_then(|v| v.strip_suffix('"'))
        {
            if let Some(package) = name.take() {
                versions.insert(package, version.to_string());
            }
        }
    }
    versions
}

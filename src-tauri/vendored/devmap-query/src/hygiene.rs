//! Cleanup eligibility is deliberately narrower than disk-usage classification.
use std::io::Read;
use std::path::Path;

fn marker(path: &Path, prefix: &str) -> bool {
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut text = String::new();
    file.take(1024).read_to_string(&mut text).is_ok() && text.starts_with(prefix)
}

pub fn protected_artifact(path: &str) -> bool {
    path.split(['/', '\\']).any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            ".git"
                | ".venv"
                | "venv"
                | ".tox"
                | "node_modules"
                | "pods"
                | ".terraform"
                | ".devcouncil"
                | ".devmap"
                | ".gitnexus"
                | ".claude"
                | ".cursor"
                | ".agents"
                | ".gemini"
                | ".antigravity"
                | ".opencode"
                | ".gopath"
                | ".gomodcache"
                | ".gradle"
                | ".pnpm-store"
                | ".npm-cache"
                | ".yarn-cache"
                | ".cache"
                | "tmp"
                | ".tmp"
                | "temp"
                | "logs"
                | "log"
        )
    })
}

/// Recognized names are a discovery hint; producer evidence, the index, age,
/// complete traversal and activity are checked separately before removal.
pub fn local_provider(repo: &Path, relative: &str) -> Result<&'static str, String> {
    if relative.is_empty()
        || relative.len() > 4096
        || relative.contains('\\')
        || relative.chars().any(char::is_control)
        || Path::new(relative).is_absolute()
        || Path::new(relative)
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        return Err("Artifact must be a bounded literal relative path".into());
    }
    if protected_artifact(relative) {
        return Err(
            "Preserved: dependencies, environments, shared caches or persistent local state."
                .into(),
        );
    }
    let path = repo.join(relative);
    let parent = path.parent().ok_or("Missing artifact parent")?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid artifact name")?;
    let has = |marker: &str| parent.join(marker).is_file();
    match name {
        n if (n == "target" || n.starts_with("target-")) && has("Cargo.toml") => {
            if !marker(
                &path.join("CACHEDIR.TAG"),
                "Signature: 8a477f597d28d172789f06886806bc55",
            ) || !path.join(".rustc_info.json").is_file()
            {
                return Err(
                    "Cargo target markers are missing; this directory requires manual review."
                        .into(),
                );
            }
            Ok("Rust / Cargo")
        }
        "__pycache__" => Ok("Python bytecode"),
        ".gocache"
            if marker(
                &path.join("README"),
                "This directory holds cached build artifacts from the Go build system.",
            ) =>
        {
            Ok("Go build cache")
        }
        ".pytest_cache" | ".mypy_cache" | ".ruff_cache" if path.join("CACHEDIR.TAG").is_file() => {
            Ok("Python tooling")
        }
        ".next" | ".nuxt" | ".output" | ".svelte-kit" | ".parcel-cache" | ".turbo" | ".vite"
            if has("package.json") =>
        {
            Ok("JavaScript / TypeScript")
        }
        "build" if has("build.gradle") || has("build.gradle.kts") => Ok("JVM / Gradle"),
        "target" if has("pom.xml") => Ok("JVM / Maven"),
        "cmake-build-debug" | "cmake-build-release"
            if has("CMakeLists.txt") && path.join("CMakeCache.txt").is_file() =>
        {
            Ok("C / C++ / CMake")
        }
        "obj"
            if std::fs::read_dir(parent)
                .map_err(|e| e.to_string())?
                .take(512)
                .any(|e| {
                    e.is_ok_and(|e| {
                        matches!(
                            e.path().extension().and_then(|s| s.to_str()),
                            Some("csproj" | "fsproj" | "vbproj")
                        )
                    })
                }) =>
        {
            Ok(".NET intermediates")
        }
        _ => Err("No verified cleanup adapter for this directory. Review it manually.".into()),
    }
}

pub const CACHE_ADVICE: &[(&str, &str, &str)] = &[
    ("Cargo", "Automatic global GC", "Cargo 1.88+ expires unused global data during substantial online commands. Leave it enabled; target directories need separate review. Prefer per-project targets or a compiler cache over one shared target directory."),
    ("Go", "Automatic build-cache expiry", "Go already expires unused build-cache entries. Clear the build cache only to reclaim space or investigate cgo changes. Preserve module downloads and fuzzing inputs."),
    ("Gradle", "Automatic cache retention", "Use Gradle's built-in cache retention. Keep module downloads, wrappers and daemon state; avoid invoking project clean tasks during discovery."),
    ("Swift / Xcode", "Preserve package checkouts", "Use swift package clean or Xcode's build cleanup after stopping builds. Do not delete all of .build: it also holds dependency checkouts and repository state."),
    ("Python / Node", "Keep environments and dependencies", "Prefer uv cache prune, npm cache verify and pnpm store prune. Virtual environments, node_modules, model weights and datasets remain protected."),
];

/// Shared by generated agent guides and host applications; guidance is not an
/// authorization grant. Hosts still own consent, activity checks and execution.
pub const AGENT_RULES: &str = "## Repository hygiene\n\n\
- Keep generated output in ignored, producer-owned directories. Use per-workspace Cargo targets and the native Go build cache; reuse valid caches instead of clearing them after every build.\n\
- Before cleanup, measure the exact paths and check producer markers, the Git index, ignore rules, nested repositories, symlinks, active tasks, open files and retention. An unavailable or partial check is a refusal, not approval.\n\
- Preserve source, uncommitted work, secrets, environments, dependencies, models, datasets, agent state and Git history. Never use home-wide rm sweeps or git clean -X as routine hygiene.\n\
- Prefer Cargo automatic GC, Go automatic expiry, npm cache verify, uv cache prune and pnpm store prune for shared caches. Explain download and rebuild costs; never silently change global target/cache configuration.\n\
- Agents must not enable or widen cleanup schedules. Only execute a user's saved scope and limits, with fresh checks immediately before mutation; stop when files, policy or activity change.\n\
- Record what was measured, removed, skipped and failed, including incomplete scans and logical versus physical bytes. Cancellation does not restore entries already removed.";

/// Bounds are common to every host; hosts may tighten them but cannot treat
/// invalid values as an instruction to disable a limit.
pub fn validate_retention(days: u32, scheduled: bool) -> Result<(), String> {
    let minimum = if scheduled { 7 } else { 1 };
    if !(minimum..=3650).contains(&days) {
        return Err(format!("Retention must be between {minimum} and 3650 days"));
    }
    Ok(())
}

#[cfg(test)]
mod hygiene_tests {
    use super::{protected_artifact, validate_retention, AGENT_RULES};
    #[test]
    fn every_host_gets_bounded_retention_and_preservation_rules() {
        for days in [0, 3651, u32::MAX] {
            assert!(validate_retention(days, false).is_err());
        }
        for days in 1..7 {
            assert!(validate_retention(days, true).is_err());
        }
        assert!(validate_retention(7, true).is_ok());
        assert!(validate_retention(3650, false).is_ok());
        for path in [
            ".venv",
            "a/.terraform/state",
            "node_modules",
            "nested/.CLAUDE/state",
        ] {
            assert!(protected_artifact(path), "{path}");
        }
        assert!(AGENT_RULES.contains("Agents must not enable or widen"));
        assert!(AGENT_RULES.contains("unavailable or partial"));
    }
    #[test]
    fn generated_guides_include_the_canonical_rules_without_overwriting_custom_guides() {
        let text =
            crate::guides::agent_guide_text(&serde_json::json!({}), "m.json", "g.json", "s.sqlite");
        assert!(text.contains(AGENT_RULES));
        assert_eq!(text.matches("## Repository hygiene").count(), 1);
    }
}

/// Conservative preservation of known state, credential and data formats.
/// This is evidence of a refusal, not proof that other files are disposable.
pub fn protected_entry(relative: &Path) -> bool {
    if protected_artifact(&relative.to_string_lossy()) {
        return true;
    }
    let Some(name) = relative.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    let name = name.to_ascii_lowercase();
    let extension = relative
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.starts_with(".env")
        || matches!(
            name.as_str(),
            "pyvenv.cfg" | "credentials.json" | "service-account.json" | "id_rsa" | "id_ed25519"
        )
        || matches!(
            extension.as_str(),
            "safetensors"
                | "gguf"
                | "ckpt"
                | "pem"
                | "key"
                | "pt"
                | "pth"
                | "onnx"
                | "h5"
                | "hdf5"
                | "parquet"
                | "arrow"
        )
}

#[cfg(test)]
mod boundary_tests {
    use super::{local_provider, protected_artifact, protected_entry};
    use std::path::Path;
    #[test]
    fn public_adapter_rejects_non_relative_and_parent_paths_before_probing() {
        for path in [
            "",
            "/tmp/__pycache__",
            "../__pycache__",
            "a/../../__pycache__",
            "a\\..\\__pycache__",
        ] {
            assert!(local_provider(Path::new("/repo"), path).is_err(), "{path}");
        }
    }
    #[test]
    fn preserves_state_on_case_insensitive_and_windows_style_paths() {
        for path in [
            "nested/.CLAUDE/state",
            "nested\\.claude\\state",
            ".VENV",
            "Pods",
        ] {
            assert!(protected_artifact(path), "{path}");
        }
        for path in [
            ".ENV.production",
            "MODEL.ONNX",
            "table.PARQUET",
            "weights.PTH",
            "id_ed25519",
        ] {
            assert!(protected_entry(Path::new(path)), "{path}");
        }
    }
}

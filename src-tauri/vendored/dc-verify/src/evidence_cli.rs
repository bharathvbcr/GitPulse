//! Filesystem/process-entry adapter only. Acceptance semantics live in dc-evidence.
//! Bundle `schema_version` stays 1; additive fields (outcome, side records) are optional.
use dc_evidence::{
    ArtifactInputs, ExpectedRun, MAX_ARTIFACT_BYTES, MAX_BUNDLE_BYTES, MAX_CONTRACT_BYTES,
    MAX_TOTAL_ARTIFACT_BYTES,
};
use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const FLAGS: &[&str] = &[
    "--contract",
    "--bundle",
    "--artifacts-root",
    "--expected-contract-sha256",
    "--expected-capability-sha256",
    "--expected-run-id",
    "--expected-session-id",
    "--expected-epoch",
];

pub(super) fn run(args: &[String]) -> Result<String, String> {
    let mut flags = BTreeMap::new();
    for pair in args.chunks(2) {
        if pair.len() != 2 || !FLAGS.contains(&pair[0].as_str()) {
            return Err(
                "evidence-check requires named flag/value pairs; see rust/dc-evidence/PROTOCOL.md"
                    .into(),
            );
        }
        if flags.insert(pair[0].as_str(), pair[1].as_str()).is_some() {
            return Err(format!("duplicate flag {}", pair[0]));
        }
    }
    for flag in FLAGS {
        if !flags.contains_key(flag) {
            return Err(format!("evidence-check missing {flag}"));
        }
    }
    let get = |name: &str| -> Result<&str, String> {
        flags
            .get(name)
            .copied()
            .ok_or_else(|| format!("missing {name}"))
    };
    let expected = ExpectedRun {
        contract_sha256: get("--expected-contract-sha256")?.into(),
        capability_sha256: get("--expected-capability-sha256")?.into(),
        run_id: get("--expected-run-id")?.into(),
        session_id: get("--expected-session-id")?.into(),
        epoch: get("--expected-epoch")?
            .parse()
            .map_err(|_| "invalid expected epoch")?,
    };
    let contract_bytes = read_regular(Path::new(get("--contract")?), MAX_CONTRACT_BYTES)?;
    let bundle_bytes = read_regular(Path::new(get("--bundle")?), MAX_BUNDLE_BYTES)?;
    // Validate the independent contract before opening any bundle-named artifact.
    dc_evidence::parse_contract(&contract_bytes).map_err(|e| e.to_string())?;
    if dc_evidence::sha256(&contract_bytes) != expected.contract_sha256 {
        let inputs = ArtifactInputs::new();
        let report = dc_evidence::verify(&contract_bytes, &bundle_bytes, &expected, &inputs)
            .map_err(|e| e.to_string())?;
        return serde_json::to_string(&report).map_err(|e| e.to_string());
    }
    let bundle = dc_evidence::parse_bundle(&bundle_bytes).map_err(|e| e.to_string())?;
    let root = checked_root(Path::new(get("--artifacts-root")?))?;
    let mut loaded = BTreeMap::new();
    let mut remaining = MAX_TOTAL_ARTIFACT_BYTES;
    for artifact in &bundle.artifacts {
        let bytes = checked_child(&root, &artifact.path)
            .and_then(|path| read_regular(&path, remaining.min(MAX_ARTIFACT_BYTES)));
        if let Ok(bytes) = &bytes {
            remaining -= bytes.len();
        }
        loaded.insert(artifact.id.clone(), bytes);
    }
    let inputs: ArtifactInputs<'_> = loaded
        .iter()
        .map(|(id, value)| {
            (
                id.as_str(),
                value.as_ref().map(Vec::as_slice).map_err(String::as_str),
            )
        })
        .collect();
    let report = dc_evidence::verify(&contract_bytes, &bundle_bytes, &expected, &inputs)
        .map_err(|e| e.to_string())?;
    serde_json::to_string(&report).map_err(|e| e.to_string())
}

fn checked_root(root: &Path) -> Result<PathBuf, String> {
    reject_symlinks(root)?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("artifact root unavailable: {e}"))?;
    if !root.is_dir() {
        return Err("artifact root is not a directory".into());
    }
    Ok(root)
}

fn checked_child(root: &Path, relative: &str) -> Result<PathBuf, String> {
    dc_evidence::validate_artifact_path(relative).map_err(|e| e.to_string())?;
    let path = root.join(relative);
    reject_symlinks(&path)?;
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("artifact unavailable: {e}"))?;
    if !resolved.starts_with(root) {
        return Err("artifact escapes root".into());
    }
    Ok(path)
}

fn reject_symlinks(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if component == Component::ParentDir {
            return Err("parent path components are refused".into());
        }
        current.push(component);
        let meta = std::fs::symlink_metadata(&current)
            .map_err(|e| format!("cannot inspect input: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err("symlink input paths are refused".into());
        }
    }
    Ok(())
}

fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    reject_symlinks(path)?;
    let before =
        std::fs::symlink_metadata(path).map_err(|e| format!("cannot inspect input: {e}"))?;
    if !before.is_file() || before.len() > limit as u64 {
        return Err(format!(
            "input must be a regular file at most {limit} bytes"
        ));
    }
    let file = File::open(path).map_err(|e| format!("cannot open input: {e}"))?;
    let opened = file
        .metadata()
        .map_err(|e| format!("cannot inspect opened input: {e}"))?;
    if !same_file(&before, &opened) || !opened.is_file() {
        return Err("input changed while opening".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read input: {e}"))?;
    if bytes.len() > limit {
        return Err(format!("input exceeds {limit} byte limit"));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file(before: &Metadata, opened: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(before: &Metadata, opened: &Metadata) -> bool {
    before.is_file() == opened.is_file()
        && before.len() == opened.len()
        && before.modified().ok() == opened.modified().ok()
}

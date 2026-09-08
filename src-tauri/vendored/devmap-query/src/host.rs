//! Checked host boundary for Dev Map's JSON and HTML artifacts.
//!
//! [`crate::viz`] and [`crate::map_preview`] deliberately accept in-memory
//! [`serde_json::Value`] documents: the CLI already owns a graph value and
//! should not serialize and read it back merely to draw it. A host application
//! starts from files, however, and needs a different contract. Missing input,
//! a FIFO, an oversized file, malformed JSON and an incompatible schema must
//! remain different failures instead of becoming the same empty-looking page.
//!
//! [`ArtifactProvider`] is the replaceable seam. The filesystem implementation
//! resolves paths through [`crate::paths`]; an application can substitute a
//! cache or IPC source by implementing [`ArtifactProvider::load_artifact`].
//! Its checked default read, payload and HTML helpers validate the returned
//! value. An implementation that overrides those methods is responsible for
//! preserving the same artifact contract.

use crate::artifacts::ArtifactFingerprint;
use crate::map_preview::{build_preview_payload, fingerprint_for, render_map_preview_html};
use crate::viz::{build_payload, render_html, VizOptions};
use crate::CODE_GRAPH_SCHEMA_VERSION;
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Default ceiling for one JSON artifact read by an embedded host.
///
/// The verbose graph is intentionally larger than the compact encoding and is
/// routinely tens of MiB. 128 MiB leaves measured repositories room without
/// turning a corrupted length or an unexpected device into an unbounded read.
pub const DEFAULT_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;

/// The two JSON artifacts accepted by the host boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    CodeGraph,
    RepoMap,
}

impl ArtifactKind {
    fn label(self) -> &'static str {
        match self {
            Self::CodeGraph => "code graph",
            Self::RepoMap => "repository map",
        }
    }
}

/// A classified refusal from the host artifact boundary.
#[derive(Debug)]
#[non_exhaustive]
pub enum ArtifactError {
    Missing {
        kind: ArtifactKind,
        path: PathBuf,
    },
    Metadata {
        kind: ArtifactKind,
        path: PathBuf,
        source: std::io::Error,
    },
    NotRegular {
        kind: ArtifactKind,
        path: PathBuf,
    },
    TooLarge {
        kind: ArtifactKind,
        path: PathBuf,
        bytes: u64,
        limit: u64,
    },
    InvalidLimit {
        kind: ArtifactKind,
        requested: u64,
        maximum: u64,
    },
    Read {
        kind: ArtifactKind,
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidJson {
        kind: ArtifactKind,
        path: PathBuf,
        source: serde_json::Error,
    },
    InvalidShape {
        kind: ArtifactKind,
        detail: String,
    },
    UnsupportedSchema {
        kind: ArtifactKind,
        found: Option<u64>,
        expected: u32,
    },
}

impl ArtifactError {
    /// Artifact whose read or validation failed.
    pub fn kind(&self) -> ArtifactKind {
        match self {
            Self::Missing { kind, .. }
            | Self::Metadata { kind, .. }
            | Self::NotRegular { kind, .. }
            | Self::TooLarge { kind, .. }
            | Self::InvalidLimit { kind, .. }
            | Self::Read { kind, .. }
            | Self::InvalidJson { kind, .. }
            | Self::InvalidShape { kind, .. }
            | Self::UnsupportedSchema { kind, .. } => *kind,
        }
    }

    /// Construct a shape refusal in a custom provider without depending on the
    /// filesystem implementation's private checks.
    pub fn invalid_shape(kind: ArtifactKind, detail: impl Into<String>) -> Self {
        Self::InvalidShape {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { kind, path } => {
                write!(f, "{} artifact {} does not exist", kind.label(), path.display())
            }
            Self::Metadata { kind, path, source } => write!(
                f,
                "cannot inspect {} artifact {}: {source}",
                kind.label(),
                path.display()
            ),
            Self::NotRegular { kind, path } => write!(
                f,
                "{} artifact {} is not a regular file",
                kind.label(),
                path.display()
            ),
            Self::TooLarge {
                kind,
                path,
                bytes,
                limit,
            } => write!(
                f,
                "{} artifact {} is {bytes} bytes, above the {limit}-byte limit",
                kind.label(),
                path.display()
            ),
            Self::InvalidLimit {
                kind,
                requested,
                maximum,
            } => write!(
                f,
                "{} artifact byte limit {requested} is invalid; choose 1..={maximum}",
                kind.label()
            ),
            Self::Read { kind, path, source } => write!(
                f,
                "cannot read {} artifact {}: {source}",
                kind.label(),
                path.display()
            ),
            Self::InvalidJson { kind, path, source } => write!(
                f,
                "{} artifact {} is not valid JSON: {source}",
                kind.label(),
                path.display()
            ),
            Self::InvalidShape { kind, detail } => {
                write!(f, "{} artifact has an invalid shape: {detail}", kind.label())
            }
            Self::UnsupportedSchema {
                kind,
                found,
                expected,
            } => match found {
                Some(found) => write!(
                    f,
                    "{} artifact uses schema {found}; this build reads schema {expected}",
                    kind.label()
                ),
                None => write!(
                    f,
                    "{} artifact does not declare a numeric schema; this build reads schema {expected}",
                    kind.label()
                ),
            },
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Metadata { source, .. } | Self::Read { source, .. } => Some(source),
            Self::InvalidJson { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Replaceable source for validated Dev Map artifacts.
///
/// Implementors provide a candidate value through [`Self::load_artifact`].
/// Consumers call [`Self::read_artifact`] or one of its typed helpers; those
/// default methods apply Dev Map's schema and shape checks after loading.
/// Implementors that override a checked default must preserve that contract.
pub trait ArtifactProvider {
    /// Load a candidate artifact from this provider.
    ///
    /// This is the only method a typical plugin needs to implement. Prefer the
    /// checked [`Self::read_artifact`] method in application code.
    fn load_artifact(&self, kind: ArtifactKind) -> Result<Value, ArtifactError>;

    fn read_artifact(&self, kind: ArtifactKind) -> Result<Value, ArtifactError> {
        let value = self.load_artifact(kind)?;
        validate_artifact(kind, &value)?;
        Ok(value)
    }

    fn read_code_graph(&self) -> Result<Value, ArtifactError> {
        self.read_artifact(ArtifactKind::CodeGraph)
    }

    fn read_repo_map(&self) -> Result<Value, ArtifactError> {
        self.read_artifact(ArtifactKind::RepoMap)
    }

    fn code_graph_payload(&self, options: &VizOptions) -> Result<Value, ArtifactError> {
        Ok(build_payload(&self.read_code_graph()?, options))
    }

    fn repo_map_payload(&self) -> Result<Value, ArtifactError> {
        Ok(build_preview_payload(&self.read_repo_map()?))
    }

    fn code_graph_html(&self, options: &VizOptions) -> Result<String, ArtifactError> {
        Ok(render_html(&self.read_code_graph()?, options))
    }

    fn repo_map_html(&self) -> Result<String, ArtifactError> {
        let map = self.read_repo_map()?;
        let fingerprint: ArtifactFingerprint = fingerprint_for(&map);
        Ok(render_map_preview_html(&map, &fingerprint))
    }
}

/// Filesystem-backed provider using Dev Map's canonical state-directory rules.
#[derive(Debug, Clone)]
pub struct FilesystemArtifactProvider {
    repo_root: PathBuf,
    code_graph_path: Option<PathBuf>,
    repo_map_path: Option<PathBuf>,
    max_bytes: u64,
}

impl FilesystemArtifactProvider {
    pub fn for_repo(root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: root.into(),
            code_graph_path: None,
            repo_map_path: None,
            max_bytes: DEFAULT_ARTIFACT_BYTES,
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn with_code_graph_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.code_graph_path = Some(path.into());
        self
    }

    pub fn with_repo_map_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.repo_map_path = Some(path.into());
        self
    }

    pub fn artifact_path(&self, kind: ArtifactKind) -> PathBuf {
        let override_path = match kind {
            ArtifactKind::CodeGraph => self.code_graph_path.as_ref(),
            ArtifactKind::RepoMap => self.repo_map_path.as_ref(),
        };
        if let Some(path) = override_path {
            return if path.is_absolute() {
                path.clone()
            } else {
                self.repo_root.join(path)
            };
        }
        match kind {
            ArtifactKind::CodeGraph => crate::paths::code_graph_path(&self.repo_root),
            ArtifactKind::RepoMap => crate::paths::repo_map_path(&self.repo_root),
        }
    }
}

impl ArtifactProvider for FilesystemArtifactProvider {
    fn load_artifact(&self, kind: ArtifactKind) -> Result<Value, ArtifactError> {
        read_json_file(kind, &self.artifact_path(kind), self.max_bytes)
    }
}

/// Read and validate a code-graph file without constructing a provider.
pub fn read_code_graph(path: &Path, max_bytes: u64) -> Result<Value, ArtifactError> {
    let value = read_json_file(ArtifactKind::CodeGraph, path, max_bytes)?;
    validate_artifact(ArtifactKind::CodeGraph, &value)?;
    Ok(value)
}

/// Read and validate a repository-map file without constructing a provider.
pub fn read_repo_map(path: &Path, max_bytes: u64) -> Result<Value, ArtifactError> {
    let value = read_json_file(ArtifactKind::RepoMap, path, max_bytes)?;
    validate_artifact(ArtifactKind::RepoMap, &value)?;
    Ok(value)
}

/// Validate a candidate supplied by any provider.
pub fn validate_artifact(kind: ArtifactKind, value: &Value) -> Result<(), ArtifactError> {
    if !value.is_object() {
        return Err(ArtifactError::invalid_shape(
            kind,
            "top level must be an object",
        ));
    }
    match kind {
        ArtifactKind::CodeGraph => {
            // Versionless artifacts predate the schema marker and remain a
            // supported compatibility shape. Once a producer declares a
            // version, it must be numeric and exactly one this renderer knows.
            if let Some(declared) = value.get("schema_version") {
                let found = declared.as_u64();
                if found != Some(u64::from(CODE_GRAPH_SCHEMA_VERSION)) {
                    return Err(ArtifactError::UnsupportedSchema {
                        kind,
                        found,
                        expected: CODE_GRAPH_SCHEMA_VERSION,
                    });
                }
            }
            require_object_array(kind, value, "nodes")?;
            require_object_array(kind, value, "edges")?;
            require_string_fields(kind, value, "nodes", &["id", "kind"])?;
            require_string_fields(kind, value, "edges", &["source", "target", "kind"])?;
            validate_complete_graph_export(kind, value)?;
        }
        ArtifactKind::RepoMap => {
            // Repo maps predate the Rust engine and carry no common numeric
            // schema. Preserve legacy compatibility while refusing shapes the
            // renderer would otherwise flatten into an empty page.
            if value.get("files").is_some() {
                require_object_array(kind, value, "files")?;
                require_string_fields(kind, value, "files", &["path"])?;
            }
            require_object_array(kind, value, "subsystems")?;
            require_string_fields(kind, value, "subsystems", &["area"])?;
            validate_modern_repo_map(kind, value)?;
        }
    }
    Ok(())
}

fn validate_modern_repo_map(kind: ArtifactKind, value: &Value) -> Result<(), ArtifactError> {
    if value.get("map_engine").and_then(Value::as_str) != Some("devmap-rust") {
        return Ok(());
    }

    let Some(liveness) = value.get("liveness_meta").and_then(Value::as_object) else {
        return Err(ArtifactError::invalid_shape(
            kind,
            "`liveness_meta` must be an object for devmap-rust maps",
        ));
    };
    if liveness.get("engine").and_then(Value::as_str) != Some("devmap-rust") {
        return Err(ArtifactError::invalid_shape(
            kind,
            "`liveness_meta.engine` must be the string `devmap-rust`",
        ));
    }

    for (list, counts) in [
        ("entry_roots", "entry_roots"),
        ("important_files", "important_files"),
        ("unwired_candidates", "unwired"),
        ("dead_symbol_candidates", "dead_symbol"),
    ] {
        require_array(kind, value, list)?;
        let actual = value[list]
            .as_array()
            .expect("validated by require_array")
            .len();
        validate_cap_fields(kind, liveness, counts, actual)?;
    }

    let subsystems = value["subsystems"]
        .as_array()
        .expect("validated by require_object_array");
    let Some(counts) = liveness.get("subsystems").and_then(Value::as_object) else {
        return Err(ArtifactError::invalid_shape(
            kind,
            "`liveness_meta.subsystems` must be an object for devmap-rust maps",
        ));
    };
    validate_cap_object(
        kind,
        counts,
        "liveness_meta.subsystems",
        "",
        subsystems.len(),
    )?;

    let mut role_files_shown = 0usize;
    let mut role_files_total = 0usize;
    for (index, subsystem) in subsystems.iter().enumerate() {
        let role_files = subsystem
            .get("role_files")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ArtifactError::invalid_shape(
                    kind,
                    format!("`subsystems[{index}].role_files` must be an object"),
                )
            })?;
        let role_counts = subsystem
            .get("role_file_counts")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ArtifactError::invalid_shape(
                    kind,
                    format!(
                        "`subsystems[{index}].role_file_counts` must be an object for devmap-rust maps"
                    ),
                )
            })?;
        for (role, paths) in role_files {
            let shown = paths.as_array().map(Vec::len).ok_or_else(|| {
                ArtifactError::invalid_shape(
                    kind,
                    format!("`subsystems[{index}].role_files.{role}` must be an array"),
                )
            })?;
            let total = role_counts
                .get(role)
                .and_then(Value::as_u64)
                .and_then(|total| usize::try_from(total).ok())
                .ok_or_else(|| {
                    ArtifactError::invalid_shape(
                        kind,
                        format!(
                            "`subsystems[{index}].role_file_counts.{role}` must be a non-negative integer"
                        ),
                    )
                })?;
            if total < shown {
                return Err(ArtifactError::invalid_shape(
                    kind,
                    format!(
                        "`subsystems[{index}].role_file_counts.{role}` ({total}) undercounts its {shown}-item sample"
                    ),
                ));
            }
            role_files_shown = role_files_shown.checked_add(shown).ok_or_else(|| {
                ArtifactError::invalid_shape(
                    kind,
                    "`subsystems[].role_files` aggregate count overflowed",
                )
            })?;
            role_files_total = role_files_total.checked_add(total).ok_or_else(|| {
                ArtifactError::invalid_shape(
                    kind,
                    "`subsystems[].role_file_counts` aggregate count overflowed",
                )
            })?;
        }
    }
    validate_cap_object(
        kind,
        counts,
        "liveness_meta.subsystems",
        "role_files_",
        role_files_shown,
    )?;
    if counts.get("role_files_total").and_then(Value::as_u64)
        != u64::try_from(role_files_total).ok()
    {
        return Err(ArtifactError::invalid_shape(
            kind,
            "`liveness_meta.subsystems.role_files_total` must equal the sum of `role_file_counts`",
        ));
    }
    Ok(())
}

fn validate_cap_fields(
    kind: ArtifactKind,
    liveness: &serde_json::Map<String, Value>,
    field: &str,
    actual: usize,
) -> Result<(), ArtifactError> {
    let counts = liveness
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ArtifactError::invalid_shape(
                kind,
                format!("`liveness_meta.{field}` must be an object for devmap-rust maps"),
            )
        })?;
    validate_cap_object(kind, counts, &format!("liveness_meta.{field}"), "", actual)
}

fn validate_cap_object(
    kind: ArtifactKind,
    counts: &serde_json::Map<String, Value>,
    path: &str,
    prefix: &str,
    actual: usize,
) -> Result<(), ArtifactError> {
    let shown_key = format!("{prefix}shown");
    let total_key = format!("{prefix}total");
    let truncated_key = format!("{prefix}truncated");
    let shown = counts.get(&shown_key).and_then(Value::as_u64);
    let total = counts.get(&total_key).and_then(Value::as_u64);
    let truncated = counts.get(&truncated_key).and_then(Value::as_bool);
    let actual = u64::try_from(actual).unwrap_or(u64::MAX);
    if shown != Some(actual)
        || total.is_none_or(|total| total < actual)
        || truncated != total.map(|total| total > actual)
    {
        return Err(ArtifactError::invalid_shape(
            kind,
            format!(
                "`{path}.{total_key}`, `{shown_key}`, and `{truncated_key}` must honestly describe the emitted items"
            ),
        ));
    }
    Ok(())
}

fn validate_complete_graph_export(kind: ArtifactKind, value: &Value) -> Result<(), ArtifactError> {
    let Some(meta) = value.get("meta") else {
        return Ok(());
    };
    let Some(meta) = meta.as_object() else {
        return Err(ArtifactError::invalid_shape(
            kind,
            "`meta` must be an object",
        ));
    };
    if let Some(tier) = meta.get("compatibility_export_tier") {
        match tier.as_str() {
            Some("slim") => {}
            Some(tier) => {
                return Err(ArtifactError::invalid_shape(
                    kind,
                    format!(
                        "compatibility export tier `{tier}` is incomplete; only `slim` is complete"
                    ),
                ));
            }
            None => {
                return Err(ArtifactError::invalid_shape(
                    kind,
                    "`meta.compatibility_export_tier` must be the string `slim` when present",
                ));
            }
        }
    }
    if let Some(reason) = meta.get("graph_export_incomplete_reason") {
        if reason.is_null() {
            return Ok(());
        }
        let Some(reason) = reason.as_str() else {
            return Err(ArtifactError::invalid_shape(
                kind,
                "`meta.graph_export_incomplete_reason` must be a string or null",
            ));
        };
        if !reason.trim().is_empty() {
            return Err(ArtifactError::invalid_shape(
                kind,
                format!("graph export is incomplete: {reason}"),
            ));
        }
    }
    Ok(())
}

fn require_array(kind: ArtifactKind, value: &Value, field: &str) -> Result<(), ArtifactError> {
    if value.get(field).is_some_and(Value::is_array) {
        return Ok(());
    }
    Err(ArtifactError::invalid_shape(
        kind,
        format!("`{field}` must be an array"),
    ))
}

fn require_object_array(
    kind: ArtifactKind,
    value: &Value,
    field: &str,
) -> Result<(), ArtifactError> {
    require_array(kind, value, field)?;
    for (index, item) in value[field]
        .as_array()
        .expect("validated by require_array")
        .iter()
        .enumerate()
    {
        if !item.is_object() {
            return Err(ArtifactError::invalid_shape(
                kind,
                format!("`{field}[{index}]` must be an object"),
            ));
        }
    }
    Ok(())
}

fn require_string_fields(
    kind: ArtifactKind,
    value: &Value,
    field: &str,
    required: &[&str],
) -> Result<(), ArtifactError> {
    for (index, item) in value[field]
        .as_array()
        .expect("validated by require_object_array")
        .iter()
        .enumerate()
    {
        for name in required {
            if item
                .get(*name)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(ArtifactError::invalid_shape(
                    kind,
                    format!("`{field}[{index}].{name}` must be a non-empty string"),
                ));
            }
        }
    }
    Ok(())
}

fn read_json_file(kind: ArtifactKind, path: &Path, max_bytes: u64) -> Result<Value, ArtifactError> {
    if !(1..=DEFAULT_ARTIFACT_BYTES).contains(&max_bytes) {
        return Err(ArtifactError::InvalidLimit {
            kind,
            requested: max_bytes,
            maximum: DEFAULT_ARTIFACT_BYTES,
        });
    }
    // Inspect before open so a FIFO is refused rather than blocking forever.
    // Symlinks are refused too: the artifact writer uses atomic regular files,
    // and following a link would move the trust boundary outside the state dir.
    // This is a portable steady-path check, not a claim of race-proof open: a
    // hostile process with write access can swap the path between metadata and
    // File::open on platforms without a portable no-follow/nonblocking open.
    let metadata = std::fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ArtifactError::Missing {
                kind,
                path: path.to_path_buf(),
            }
        } else {
            ArtifactError::Metadata {
                kind,
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    if !metadata.file_type().is_file() {
        return Err(ArtifactError::NotRegular {
            kind,
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > max_bytes {
        return Err(ArtifactError::TooLarge {
            kind,
            path: path.to_path_buf(),
            bytes: metadata.len(),
            limit: max_bytes,
        });
    }

    let file = File::open(path).map_err(|source| ArtifactError::Read {
        kind,
        path: path.to_path_buf(),
        source,
    })?;
    let opened = file.metadata().map_err(|source| ArtifactError::Metadata {
        kind,
        path: path.to_path_buf(),
        source,
    })?;
    if !opened.file_type().is_file() {
        return Err(ArtifactError::NotRegular {
            kind,
            path: path.to_path_buf(),
        });
    }
    if opened.len() > max_bytes {
        return Err(ArtifactError::TooLarge {
            kind,
            path: path.to_path_buf(),
            bytes: opened.len(),
            limit: max_bytes,
        });
    }

    // Re-check through the bytes too: a regular file may grow after metadata.
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ArtifactError::Read {
            kind,
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u128 > u128::from(max_bytes) {
        return Err(ArtifactError::TooLarge {
            kind,
            path: path.to_path_buf(),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit: max_bytes,
        });
    }
    serde_json::from_slice(&bytes).map_err(|source| ArtifactError::InvalidJson {
        kind,
        path: path.to_path_buf(),
        source,
    })
}

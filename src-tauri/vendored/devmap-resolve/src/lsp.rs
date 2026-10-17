//! Opt-in language-server edges for `devmap build --lsp`.
//!
//! The syntax resolver remains the owner of every deterministic and
//! unique-global claim. This pass may only *add* [`Resolution::LanguageServer`]
//! or [`Resolution::LanguageServerDispatch`] edges for sites that are still in
//! the unresolved ledger, and only when a server on `PATH`, installed outside
//! the repository, actually names a target inside it. A missing, hung, oversized, or disagreeing
//! answer never falls through to a bare-name [`Resolution::UniqueGlobal`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use devmap_extract::model::{EdgeKind, ExtractedSymbol, Extraction};
use devmap_extract::subprocess::{self, Session};
use serde_json::{json, Value};

use crate::model::{
    Resolution, ResolutionResult, ResolvedEdge, UnresolvedKind, UnresolvedReference,
};

/// Hard ceiling on how many unresolved *files* this pass will open against a
/// language server in one build. Past this, remaining files are reported as
/// did-not-run rather than silently skipped into a "zero edges" answer.
pub const LSP_FILE_BUDGET: usize = 256;

/// Wall-clock budget for the whole opt-in pass. A hang killed at this bound
/// counts unfinished files as did-not-run.
pub const LSP_PASS_BUDGET: Duration = Duration::from_secs(90);

/// Per-request read deadline. A server that stops answering is killed and the
/// files it had not finished are did-not-run.
pub const LSP_REQUEST_BUDGET: Duration = Duration::from_secs(8);

/// Maximum JSON-RPC frame this pass will accept. Larger responses are refused
/// rather than parsed into edges.
pub const LSP_PAYLOAD_CAP: usize = 256 * 1024;

/// Longest header line accepted. A server streaming bytes with no newline would
/// otherwise grow one `String` without bound.
const LSP_HEADER_LINE_CAP: u64 = 8 * 1024;

/// How long `shutdown` waits for a server to exit on its own before killing it.
const LSP_EXIT_GRACE: Duration = Duration::from_millis(500);

/// Servers this pass will start, in order, and only when the binary is on
/// `PATH` outside the repository (see `which_binary`). A missing or refused
/// binary is a status line that the pass did not run — never "zero new edges
/// from a run".
pub const LSP_SERVERS: &[LspServerSpec] = &[
    LspServerSpec {
        binary: "rust-analyzer",
        args: &[],
        extensions: &[".rs"],
    },
    LspServerSpec {
        binary: "gopls",
        args: &["serve"],
        extensions: &[".go"],
    },
    LspServerSpec {
        binary: "pyright",
        args: &["--stdio"],
        extensions: &[".py", ".pyi"],
    },
    LspServerSpec {
        binary: "typescript-language-server",
        args: &["--stdio"],
        extensions: &[".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs"],
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspServerSpec {
    pub binary: &'static str,
    pub args: &'static [&'static str],
    pub extensions: &'static [&'static str],
}

impl LspServerSpec {
    pub fn covers_path(self, path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        self.extensions.iter().any(|ext| lower.ends_with(ext))
    }
}

/// Why a server did not produce edges for the files it was asked about.
///
/// `MissingBinary` is no candidate on PATH; `RefusedBinary` is a candidate
/// found and refused without being started — inside the repository, reached
/// through a link the repository carries, or through an empty or relative
/// PATH entry — carrying what was refused and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspDidNotRun {
    MissingBinary,
    RefusedBinary(String),
    NonZeroExit { code: Option<i32> },
    TimedOut,
    OversizedPayload { bytes: usize },
    Cancelled,
    Protocol(String),
}

impl LspDidNotRun {
    pub fn label(&self) -> &'static str {
        match self {
            LspDidNotRun::MissingBinary => "missing_binary",
            LspDidNotRun::RefusedBinary(_) => "refused_binary",
            LspDidNotRun::NonZeroExit { .. } => "non_zero_exit",
            LspDidNotRun::TimedOut => "timed_out",
            LspDidNotRun::OversizedPayload { .. } => "oversized_payload",
            LspDidNotRun::Cancelled => "cancelled",
            LspDidNotRun::Protocol(_) => "protocol",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspServerStatus {
    pub binary: String,
    pub version: Option<String>,
    pub ran: bool,
    pub did_not_run: Option<LspDidNotRun>,
    pub files_considered: usize,
    pub files_finished: usize,
    pub edges_added: usize,
    /// Sites never asked about: the row carries no source position.
    pub sites_without_position: usize,
    /// Files never opened: no file URI inside the repository names them
    /// (`..`, an empty component, or a path the host cannot spell as a URI).
    pub files_unaddressable: usize,
    /// Locations the server named that were dropped because the URI does not
    /// decode to a path: a bad escape, a query or fragment, not `file:`.
    pub locations_undecodable: usize,
    /// Locations dropped because they decode to a path outside the repository.
    pub locations_outside_repo: usize,
}

impl LspServerStatus {
    /// Whether anything this server was asked, or answered, was dropped
    /// rather than turned into an edge or an abstention.
    fn dropped_any(&self) -> bool {
        self.sites_without_position
            + self.files_unaddressable
            + self.locations_undecodable
            + self.locations_outside_repo
            > 0
    }
}

/// Why a location a server named did not become a repository file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationDrop {
    /// The URI does not decode to a path on this host.
    Undecodable,
    /// It decodes, to a path outside the repository.
    OutsideRepo,
}

/// Per-answer tally of [`LocationDrop`]s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LocationDrops {
    undecodable: usize,
    outside_repo: usize,
}

impl LocationDrops {
    fn count(&mut self, drop: LocationDrop) {
        match drop {
            LocationDrop::Undecodable => self.undecodable += 1,
            LocationDrop::OutsideRepo => self.outside_repo += 1,
        }
    }
}

/// A definition location a server named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub file: String,
    pub line: u32,
    pub character: u32,
}

/// Decision input for one unresolved site, after the client has asked a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspSiteAnswer {
    pub server: String,
    pub server_version: String,
    pub locations: Vec<LspLocation>,
}

/// Stable edge-payload spelling so a later reader can see who claimed the edge.
pub fn lsp_edge_details(server: &str, version: &str) -> String {
    format!("lsp:{server}@{version}")
}

/// Convert an LSP line/character into a byte offset using the target source.
pub fn location_byte_in_source(source: &str, line: u32, character: u32) -> usize {
    let mut current_line = 0u32;
    let mut at = 0usize;
    if line > 0 {
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                current_line += 1;
                at = offset + 1;
                if current_line == line {
                    break;
                }
            }
        }
        if current_line != line {
            return source.len();
        }
    }
    let line_text = source.get(at..).unwrap_or("");
    let mut utf16 = 0u32;
    for (byte_offset, ch) in line_text.char_indices() {
        if ch == '\n' {
            return at + byte_offset;
        }
        if utf16 >= character {
            return at + byte_offset;
        }
        utf16 += ch.len_utf16() as u32;
    }
    at + line_text.len()
}

/// Where each line of one source starts, built once per file.
///
/// Turning a site into an LSP position used to count newlines in everything
/// before it, once per site — quadratic in a file with many unresolved sites.
/// With the index it is a binary search plus the width of the one line.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|&(_, byte)| byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        Self { starts }
    }

    /// The zero-based line and UTF-16 column of `byte`, clamped to the source
    /// and moved back to the nearest character boundary.
    fn position(&self, source: &str, byte: usize) -> (u32, u32) {
        let mut byte = byte.min(source.len());
        while !source.is_char_boundary(byte) {
            byte -= 1;
        }
        let line = self.starts.partition_point(|&start| start <= byte) - 1;
        let start = self.starts[line];
        let character = source[start..byte].encode_utf16().count();
        (line as u32, character as u32)
    }
}

/// The byte a server is asked about for `site`, or `None` when the row
/// carries no position this source can answer.
///
/// That is the callee's own name inside the span the resolver recorded, so
/// `a.run()` is asked about `run` and not about `a`: the definition of `a` is
/// a local binding, which would bind the call to the function declaring it.
/// The receiver text is skipped first, so `run.run()` is asked about the
/// second `run`. When the name is not spelled inside the span — `with
/// Engine() as e` records `__enter__` — the span's start is asked instead.
fn query_byte(source: &str, site: &UnresolvedReference) -> Option<usize> {
    let bytes = site.site?;
    let text = source.get(bytes.start..bytes.end.min(source.len()))?;
    let name = site.callee_name.as_str();
    if name.is_empty() {
        return Some(bytes.start);
    }
    let mut from = site
        .receiver
        .as_deref()
        .filter(|receiver| text.starts_with(receiver))
        .map_or(0, str::len);
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    while let Some(found) = text.get(from..).and_then(|rest| rest.find(name)) {
        let at = from + found;
        let end = at + name.len();
        let bounded_before = text[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let bounded_after = text[end..].chars().next().is_none_or(|c| !is_ident(c));
        if bounded_before && bounded_after {
            return Some(bytes.start + at);
        }
        from = at + text[at..].chars().next().map_or(1, char::len_utf8);
    }
    Some(bytes.start)
}

/// Turn a server's locations into a resolution, or abstain.
///
/// Rules, pinned by the adversarial tests:
/// - zero in-repo locations that map to indexed symbols → abstain
/// - one → `LanguageServer`
/// - several → `LanguageServerDispatch`
/// - locations outside the repository are already dropped by the client
/// - never invent a `UniqueGlobal` from the callee's bare name
pub fn decide_language_server_resolution(
    answer: &LspSiteAnswer,
    symbols_by_file: &BTreeMap<&str, Vec<&ExtractedSymbol>>,
    sources: &BTreeMap<String, String>,
) -> Option<Resolution> {
    let mut targets: BTreeSet<(String, String)> = BTreeSet::new();
    for location in &answer.locations {
        let Some(symbols) = symbols_by_file.get(location.file.as_str()) else {
            continue;
        };
        let Some(source) = sources.get(location.file.as_str()) else {
            continue;
        };
        let start_byte = location_byte_in_source(source, location.line, location.character);
        // The innermost declaration holding the location: the first one held
        // it too when it was the file's own module symbol, or the type around
        // a method, and bound the call to that instead of the definition.
        let Some(symbol) = symbols
            .iter()
            .filter(|symbol| {
                symbol.span.start_byte <= start_byte && start_byte < symbol.span.end_byte
            })
            .min_by_key(|symbol| symbol.span.end_byte - symbol.span.start_byte)
        else {
            continue;
        };
        targets.insert((location.file.clone(), symbol.name.clone()));
    }
    match targets.len() {
        0 => None,
        1 => {
            let (file, symbol) = targets.into_iter().next().expect("len 1");
            Some(Resolution::LanguageServer {
                target_symbol: symbol,
                target_file: file,
                server: answer.server.clone(),
                server_version: answer.server_version.clone(),
            })
        }
        _ => Some(Resolution::LanguageServerDispatch {
            candidates: targets.into_iter().collect::<Vec<_>>().into(),
            server: answer.server.clone(),
            server_version: answer.server_version.clone(),
        }),
    }
}

/// When two servers name different targets for the same site, abstain.
/// Neither answer becomes a `UniqueGlobal`, and neither becomes a
/// `LanguageServer` edge — disagreement is not evidence of one winner.
pub fn reconcile_server_answers(answers: &[LspSiteAnswer]) -> Option<&LspSiteAnswer> {
    match answers {
        [] => None,
        [only] => Some(only),
        rest => {
            let first = &rest[0].locations;
            if rest.iter().all(|answer| answer.locations == *first) {
                Some(&rest[0])
            } else {
                None
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LspPassReport {
    pub servers: Vec<LspServerStatus>,
    pub edges_added: usize,
    pub sites_resolved: usize,
    pub sites_left_unresolved: usize,
}

impl LspPassReport {
    /// Status lines for the build readout. A missing server is "did not run",
    /// never "zero edges from a run".
    pub fn status_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for server in &self.servers {
            if let Some(reason) = &server.did_not_run {
                lines.push(format!(
                    "lsp: {} did not run ({}) — {} file(s) considered, {} finished",
                    server.binary,
                    reason.label(),
                    server.files_considered,
                    server.files_finished
                ));
                // Which candidate was refused, and why, is what the operator
                // needs to act on; the label alone does not say where it was.
                if let LspDidNotRun::RefusedBinary(detail) = reason {
                    lines.push(format!("lsp: {} refused: {detail}", server.binary));
                }
            } else if server.ran {
                lines.push(format!(
                    "lsp: {} {} — {} edge(s) from {} file(s)",
                    server.binary,
                    server.version.as_deref().unwrap_or("unknown"),
                    server.edges_added,
                    server.files_finished
                ));
            }
            // A run that added no edge because every answer was dropped must
            // not read like a run whose answers named nothing.
            if server.dropped_any() {
                lines.push(format!(
                    "lsp: {} dropped {} location(s) as undecodable and {} as outside the \
                     repository; {} site(s) had no position and {} file(s) no in-repository URI",
                    server.binary,
                    server.locations_undecodable,
                    server.locations_outside_repo,
                    server.sites_without_position,
                    server.files_unaddressable
                ));
            }
        }
        if self.servers.is_empty() {
            lines.push("lsp: pass requested but no covering server was asked".into());
        }
        lines.push(format!(
            "lsp: {} site(s) resolved, {} left in the unresolved ledger",
            self.sites_resolved, self.sites_left_unresolved
        ));
        lines
    }
}

/// Apply already-decided resolutions to the unresolved ledger.
///
/// Only sites that receive a `LanguageServer` or `LanguageServerDispatch`
/// resolution leave the ledger. Everything else stays.
pub fn apply_language_server_resolutions(
    resolution: &mut ResolutionResult,
    decisions: &[(usize, Resolution)],
) -> usize {
    if decisions.is_empty() {
        return 0;
    }
    let mut by_index: BTreeMap<usize, Resolution> = BTreeMap::new();
    for (index, decided) in decisions {
        by_index.insert(*index, decided.clone());
    }
    // Own the sites before mutating `resolution` through `emit_language_server_edges`.
    let pending: Vec<(usize, UnresolvedReference)> =
        resolution.unresolved.iter().cloned().enumerate().collect();
    let mut keep = Vec::with_capacity(pending.len());
    let mut added = 0usize;
    for (index, site) in pending {
        let Some(decided) = by_index.remove(&index) else {
            keep.push(site);
            continue;
        };
        added += emit_language_server_edges(resolution, &site, Arc::new(decided));
    }
    resolution.unresolved = keep;
    added
}

fn emit_language_server_edges(
    resolution: &mut ResolutionResult,
    site: &UnresolvedReference,
    decided: Arc<Resolution>,
) -> usize {
    let details = match decided.as_ref() {
        Resolution::LanguageServer {
            server,
            server_version,
            ..
        }
        | Resolution::LanguageServerDispatch {
            server,
            server_version,
            ..
        } => Some(lsp_edge_details(server, server_version)),
        _ => None,
    };
    match decided.as_ref() {
        Resolution::LanguageServer {
            target_symbol,
            target_file,
            ..
        } => {
            resolution.edges.push(ResolvedEdge::resolved(
                site.source_file.clone(),
                target_file.clone(),
                site.source_symbol.clone(),
                target_symbol.clone(),
                EdgeKind::Calls,
                Arc::clone(&decided),
                details,
            ));
            1
        }
        Resolution::LanguageServerDispatch { candidates, .. } => {
            let mut count = 0usize;
            for (target_file, target_symbol) in candidates.iter() {
                resolution.edges.push(ResolvedEdge::resolved(
                    site.source_file.clone(),
                    target_file.clone(),
                    site.source_symbol.clone(),
                    target_symbol.clone(),
                    EdgeKind::Calls,
                    Arc::clone(&decided),
                    details.clone(),
                ));
                count += 1;
            }
            count
        }
        _ => 0,
    }
}

/// Run the opt-in pass with the source text needed to convert spans into LSP
/// positions. Missing servers are recorded as did-not-run; the unresolved
/// ledger is never hidden.
pub fn enrich_with_language_servers_with_sources(
    repo_root: &Path,
    extractions: &[Extraction],
    sources: &BTreeMap<String, String>,
    resolution: &mut ResolutionResult,
    cancel: &AtomicBool,
) -> LspPassReport {
    // Read once: every server is resolved against the same PATH.
    let path_var = std::env::var_os("PATH");
    enrich_with_servers(
        LSP_SERVERS,
        path_var.as_deref(),
        repo_root,
        extractions,
        sources,
        resolution,
        cancel,
    )
}

/// The pass over an explicit server list and `PATH` value, which is what the
/// public entry point fixes to [`LSP_SERVERS`] and the process environment.
fn enrich_with_servers(
    servers: &[LspServerSpec],
    path_var: Option<&std::ffi::OsStr>,
    repo_root: &Path,
    extractions: &[Extraction],
    sources: &BTreeMap<String, String>,
    resolution: &mut ResolutionResult,
    cancel: &AtomicBool,
) -> LspPassReport {
    let deadline = Instant::now() + LSP_PASS_BUDGET;
    let mut report = LspPassReport {
        sites_left_unresolved: resolution.unresolved.len(),
        ..LspPassReport::default()
    };

    let symbols_by_file: BTreeMap<&str, Vec<&ExtractedSymbol>> = extractions
        .iter()
        .map(|extraction| {
            (
                extraction.file_path.as_str(),
                extraction.symbols.iter().collect(),
            )
        })
        .collect();

    let mut by_server: BTreeMap<&'static str, Vec<(usize, UnresolvedReference)>> = BTreeMap::new();
    for (index, site) in resolution.unresolved.iter().enumerate() {
        if !matches!(site.kind, UnresolvedKind::Call | UnresolvedKind::Reference) {
            continue;
        }
        let Some(spec) = servers
            .iter()
            .find(|spec| spec.covers_path(&site.source_file))
        else {
            continue;
        };
        by_server
            .entry(spec.binary)
            .or_default()
            .push((index, site.clone()));
    }

    let mut decisions: Vec<(usize, Resolution)> = Vec::new();
    // One per file, shared by every server: built on the first site asked
    // about in that file.
    let mut line_indexes: BTreeMap<&str, LineIndex> = BTreeMap::new();

    for spec in servers {
        let sites = by_server.remove(spec.binary).unwrap_or_default();
        if sites.is_empty() {
            continue;
        }

        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            report.servers.push(LspServerStatus {
                binary: spec.binary.to_string(),
                version: None,
                ran: false,
                did_not_run: Some(if cancel.load(Ordering::Relaxed) {
                    LspDidNotRun::Cancelled
                } else {
                    LspDidNotRun::TimedOut
                }),
                ..LspServerStatus::default()
            });
            continue;
        }

        let mut files: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        for (_, site) in &sites {
            if seen.insert(site.source_file.clone()) {
                files.push(site.source_file.clone());
            }
        }
        let considered = files.len();
        if files.len() > LSP_FILE_BUDGET {
            files.truncate(LSP_FILE_BUDGET);
        }
        let allowed: BTreeSet<&str> = files.iter().map(String::as_str).collect();

        let binary_path = match which_binary(spec.binary, path_var, repo_root) {
            Ok(path) => path,
            Err(reason) => {
                report.servers.push(LspServerStatus {
                    binary: spec.binary.to_string(),
                    did_not_run: Some(reason),
                    files_considered: considered,
                    ..LspServerStatus::default()
                });
                continue;
            }
        };

        let mut client = match LspClient::spawn(spec, &binary_path, repo_root, deadline) {
            Ok(client) => client,
            Err(reason) => {
                report.servers.push(LspServerStatus {
                    binary: spec.binary.to_string(),
                    did_not_run: Some(reason),
                    files_considered: considered,
                    ..LspServerStatus::default()
                });
                continue;
            }
        };

        let version = client.server_version.clone();
        let mut finished_files = BTreeSet::new();
        // Files skipped for a stated reason rather than left unfinished: no
        // site in them had a position, or no in-repository URI names them.
        let mut skipped_files: BTreeSet<&str> = BTreeSet::new();
        let mut unaddressable_files: BTreeSet<&str> = BTreeSet::new();
        let mut status = LspServerStatus {
            binary: spec.binary.to_string(),
            files_considered: considered,
            ..LspServerStatus::default()
        };
        let edges_before = decisions.len();
        let mut failed: Option<LspDidNotRun> = None;

        for (index, site) in &sites {
            if !allowed.contains(site.source_file.as_str()) {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                failed = Some(LspDidNotRun::Cancelled);
                break;
            }
            if Instant::now() >= deadline {
                failed = Some(LspDidNotRun::TimedOut);
                break;
            }
            let file = site.source_file.as_str();
            let Some((key, source, byte)) =
                sources.get_key_value(file).and_then(|(key, source)| {
                    query_byte(source, site).map(|byte| (key.as_str(), source, byte))
                })
            else {
                status.sites_without_position += 1;
                skipped_files.insert(file);
                continue;
            };
            let Ok(uri) = uri_for_repo_file(&client.repo_root, file) else {
                if unaddressable_files.insert(file) {
                    status.files_unaddressable += 1;
                }
                skipped_files.insert(file);
                continue;
            };
            let (line, character) = line_indexes
                .entry(key)
                .or_insert_with(|| LineIndex::new(source))
                .position(source, byte);
            match client.definition(file, &uri, source, line, character) {
                Ok((locations, drops)) => {
                    status.locations_undecodable += drops.undecodable;
                    status.locations_outside_repo += drops.outside_repo;
                    finished_files.insert(site.source_file.clone());
                    let answer = LspSiteAnswer {
                        server: spec.binary.to_string(),
                        server_version: version.clone().unwrap_or_else(|| "unknown".into()),
                        locations,
                    };
                    if let Some(decided) =
                        decide_language_server_resolution(&answer, &symbols_by_file, sources)
                    {
                        // Never write UniqueGlobal from this pass.
                        debug_assert!(!matches!(decided, Resolution::UniqueGlobal { .. }));
                        decisions.push((*index, decided));
                    }
                }
                Err(reason) => {
                    failed = Some(reason);
                    break;
                }
            }
        }

        let edges_for_server = decisions.len().saturating_sub(edges_before);
        let skipped = skipped_files
            .iter()
            .filter(|file| !finished_files.contains(**file))
            .count();
        let unfinished = considered.saturating_sub(finished_files.len() + skipped);
        let did_not_run = match failed {
            Some(reason) if unfinished > 0 => Some(reason),
            Some(reason @ LspDidNotRun::OversizedPayload { .. })
            | Some(reason @ LspDidNotRun::NonZeroExit { .. })
            | Some(reason @ LspDidNotRun::MissingBinary)
            | Some(reason @ LspDidNotRun::RefusedBinary(_))
            | Some(reason @ LspDidNotRun::Protocol(_))
            | Some(reason @ LspDidNotRun::TimedOut)
            | Some(reason @ LspDidNotRun::Cancelled) => Some(reason),
            None if unfinished > 0 => Some(LspDidNotRun::TimedOut),
            None => None,
        };
        report.servers.push(LspServerStatus {
            version,
            ran: did_not_run.is_none(),
            did_not_run,
            files_finished: finished_files.len(),
            edges_added: edges_for_server,
            ..status
        });
        let _ = client.shutdown();
    }

    let edges_added = apply_language_server_resolutions(resolution, &decisions);
    report.edges_added = edges_added;
    report.sites_resolved = decisions.len();
    report.sites_left_unresolved = resolution.unresolved.len();
    report
}

/// The language server `name` names on `path_var` (the `PATH` value), through
/// the one resolver every process boundary in the workspace uses,
/// `dc_proc::resolve_outside`.
///
/// The pass runs in the environment of whoever asked for `--lsp`, often an
/// agent's, where a PATH entry can name a directory inside the repository
/// being built. A server there is repository content, not a program to start,
/// so a candidate inside `repo_root`, a link chain through it, or one found
/// through an empty or relative entry (which names the directory the build
/// runs in) is refused. Not found is [`LspDidNotRun::MissingBinary`]; refused
/// is [`LspDidNotRun::RefusedBinary`], so a refusal never reads as an absent
/// install.
fn which_binary(
    name: &str,
    path_var: Option<&std::ffi::OsStr>,
    repo_root: &Path,
) -> Result<PathBuf, LspDidNotRun> {
    match subprocess::resolve_outside(std::ffi::OsStr::new(name), path_var, repo_root) {
        Ok(path) => Ok(path),
        Err(subprocess::Failure::Spawn { error, .. })
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Err(LspDidNotRun::MissingBinary)
        }
        Err(failure) => Err(LspDidNotRun::RefusedBinary(failure.to_string())),
    }
}

/// What the reader thread hands back: a decoded frame, or why it stopped.
enum Incoming {
    Message(Value),
    Failed(LspDidNotRun),
    Eof,
}

/// A language server session.
///
/// Every wait here is bounded. Reads and writes happen on threads of their
/// own, and the caller only ever waits on a channel with `recv_timeout`: a
/// blocking `read_line` on the calling thread never returns from a server that
/// has gone quiet, which is how the per-request and per-pass budgets used to be
/// checked only between reads and never during one. On expiry the session is
/// killed, which closes the pipes and ends both threads.
struct LspClient {
    session: Session,
    incoming: Receiver<Incoming>,
    outgoing: Option<SyncSender<Vec<u8>>>,
    next_id: u64,
    server_version: Option<String>,
    deadline: Instant,
    open_files: BTreeSet<String>,
    repo_root: PathBuf,
}

/// One frame from `reader`: headers, then a body of at most
/// [`LSP_PAYLOAD_CAP`] bytes.
fn read_frame(reader: &mut impl BufRead) -> Incoming {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = Vec::new();
        let read = match reader
            .by_ref()
            .take(LSP_HEADER_LINE_CAP)
            .read_until(b'\n', &mut line)
        {
            Ok(read) => read,
            Err(error) => {
                return Incoming::Failed(LspDidNotRun::Protocol(format!(
                    "read header failed: {error}"
                )))
            }
        };
        if read == 0 {
            return Incoming::Eof;
        }
        if !line.ends_with(b"\n") {
            return Incoming::Failed(LspDidNotRun::Protocol(format!(
                "header line longer than {LSP_HEADER_LINE_CAP} bytes"
            )));
        }
        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }
    let Some(length) = content_length else {
        return Incoming::Failed(LspDidNotRun::Protocol("missing Content-Length".into()));
    };
    if length > LSP_PAYLOAD_CAP {
        return Incoming::Failed(LspDidNotRun::OversizedPayload { bytes: length });
    }
    let mut body = vec![0u8; length];
    if let Err(error) = reader.read_exact(&mut body) {
        return Incoming::Failed(LspDidNotRun::Protocol(format!("read body failed: {error}")));
    }
    match serde_json::from_slice(&body) {
        Ok(value) => Incoming::Message(value),
        Err(error) => Incoming::Failed(LspDidNotRun::Protocol(format!("json failed: {error}"))),
    }
}

impl LspClient {
    fn spawn(
        spec: &LspServerSpec,
        binary: &Path,
        repo_root: &Path,
        deadline: Instant,
    ) -> Result<Self, LspDidNotRun> {
        let mut command = subprocess::session_command(binary);
        command.args(spec.args).current_dir(repo_root);
        let spawn_budget = deadline.saturating_duration_since(Instant::now());
        let mut session = subprocess::spawn_session(&mut command, spawn_budget)
            .map_err(|error| LspDidNotRun::Protocol(format!("spawn {}: {error}", spec.binary)))?;
        let stdout = session
            .take_stdout()
            .ok_or_else(|| LspDidNotRun::Protocol("missing stdout".into()))?;
        let mut stdin = session
            .take_stdin()
            .ok_or_else(|| LspDidNotRun::Protocol("missing stdin".into()))?;

        // Bounded to a few frames: a server that stops reading backs the
        // writer up, and the caller's recv_timeout on the reply is what ends it.
        let (incoming_tx, incoming) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = read_frame(&mut reader);
                let last = !matches!(frame, Incoming::Message(_));
                if incoming_tx.send(frame).is_err() || last {
                    return;
                }
            }
        });
        let (outgoing, outgoing_rx) = mpsc::sync_channel::<Vec<u8>>(16);
        thread::spawn(move || {
            for frame in outgoing_rx {
                if stdin
                    .write_all(&frame)
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    return;
                }
            }
        });

        // Absolute, and canonical where it exists, so every URI sent names the
        // same spelling of the root a server reports back.
        let repo_root = match repo_root.canonicalize() {
            Ok(root) => root,
            Err(_) => std::path::absolute(repo_root).map_err(|error| {
                LspDidNotRun::Protocol(format!("repository root {}: {error}", repo_root.display()))
            })?,
        };
        let root_uri = file_uri(&repo_root).map_err(LspDidNotRun::Protocol)?;
        let mut client = Self {
            session,
            incoming,
            outgoing: Some(outgoing),
            next_id: 1,
            server_version: None,
            deadline,
            open_files: BTreeSet::new(),
            repo_root,
        };
        let init = client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "definition": { "linkSupport": false }
                    }
                }
            }),
        )?;
        if let Some(server_info) = init.get("serverInfo") {
            let name = server_info
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(spec.binary);
            let version = server_info
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            client.server_version = Some(format!("{name} {version}"));
        } else {
            client.server_version = Some("unknown".into());
        }
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    /// Ask where the symbol at `line`/`character` of `relative` is defined.
    /// `uri` is `relative`'s [`uri_for_repo_file`], built by the caller so a
    /// file no URI can name is skipped there rather than failing the session.
    fn definition(
        &mut self,
        relative: &str,
        uri: &str,
        source: &str,
        line: u32,
        character: u32,
    ) -> Result<(Vec<LspLocation>, LocationDrops), LspDidNotRun> {
        if !self.open_files.contains(relative) {
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id_for(relative),
                        "version": 1,
                        "text": source,
                    }
                }),
            )?;
            self.open_files.insert(relative.to_string());
        }
        let result = self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character }
            }),
        )?;
        Ok(parse_locations(&result, &self.repo_root))
    }

    fn shutdown(&mut self) -> Result<(), LspDidNotRun> {
        let _ = self.request("shutdown", json!(null));
        let _ = self.notify("exit", json!(null));
        // A polite exit gets a grace period, never an unbounded wait: a server
        // that ignores `exit` is killed with the rest of its process group.
        let grace = Instant::now() + LSP_EXIT_GRACE;
        while self.session.try_status().is_none() && Instant::now() < grace {
            thread::sleep(Duration::from_millis(10));
        }
        self.kill();
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, LspDidNotRun> {
        if Instant::now() >= self.deadline {
            self.kill();
            return Err(LspDidNotRun::TimedOut);
        }
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&payload)?;
        let request_deadline = Instant::now()
            .checked_add(LSP_REQUEST_BUDGET)
            .unwrap_or(self.deadline)
            .min(self.deadline);
        loop {
            if Instant::now() >= request_deadline {
                self.kill();
                return Err(LspDidNotRun::TimedOut);
            }
            let message = self.read_message(request_deadline)?;
            let message_id = message
                .get("id")
                .and_then(|value| value.as_u64().or_else(|| value.as_i64().map(|v| v as u64)));
            if message_id == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(LspDidNotRun::Protocol(error.to_string()));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), LspDidNotRun> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&mut self, payload: &Value) -> Result<(), LspDidNotRun> {
        let body = serde_json::to_vec(payload)
            .map_err(|error| LspDidNotRun::Protocol(format!("serialize failed: {error}")))?;
        if body.len() > LSP_PAYLOAD_CAP {
            return Err(LspDidNotRun::OversizedPayload { bytes: body.len() });
        }
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        let Some(outgoing) = &self.outgoing else {
            return Err(LspDidNotRun::Protocol("the session is closed".into()));
        };
        // try_send, not send: a writer backed up behind a server that stopped
        // reading must not block this thread past the deadline.
        match outgoing.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                self.kill();
                Err(LspDidNotRun::TimedOut)
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(LspDidNotRun::Protocol("write to server failed".into()))
            }
        }
    }

    fn read_message(&mut self, request_deadline: Instant) -> Result<Value, LspDidNotRun> {
        let wait = request_deadline.saturating_duration_since(Instant::now());
        match self.incoming.recv_timeout(wait) {
            Ok(Incoming::Message(value)) => Ok(value),
            Ok(Incoming::Failed(reason)) => {
                self.kill();
                Err(reason)
            }
            Ok(Incoming::Eof) | Err(RecvTimeoutError::Disconnected) => {
                let code = self.session.try_status().and_then(|status| status.code());
                self.kill();
                Err(LspDidNotRun::NonZeroExit { code })
            }
            Err(RecvTimeoutError::Timeout) => {
                self.kill();
                Err(LspDidNotRun::TimedOut)
            }
        }
    }

    fn kill(&mut self) {
        // Dropping the sender ends the writer thread once it is unblocked; the
        // kill closes the pipes, which unblocks both threads.
        self.outgoing = None;
        self.session.kill();
    }
}

fn language_id_for(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".py") || lower.ends_with(".pyi") {
        "python"
    } else if lower.ends_with(".tsx") {
        "typescriptreact"
    } else if lower.ends_with(".jsx") {
        "javascriptreact"
    } else if lower.ends_with(".ts") || lower.ends_with(".mts") || lower.ends_with(".cts") {
        "typescript"
    } else {
        "javascript"
    }
}

/// RFC 3986 percent-encoding of a path's bytes: every byte outside the
/// unreserved set and `/` becomes `%XX`. A space, `#`, `?` or `%` written raw
/// would end the path or start an escape in the server's parser, and a
/// non-ASCII name is not a valid URI character at all.
fn percent_encode_path(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    out
}

/// The inverse of [`percent_encode_path`], for any encoder's output: `%XX` in
/// either case becomes its byte and every other byte is kept. A `%` not
/// followed by two hex digits is `None`, never a guess.
fn percent_decode(text: &str) -> Option<Vec<u8>> {
    fn hex(digit: u8) -> Option<u8> {
        match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            b'A'..=b'F' => Some(digit - b'A' + 10),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let high = hex(*bytes.get(at + 1)?)?;
            let low = hex(*bytes.get(at + 2)?)?;
            out.push(high << 4 | low);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    Some(out)
}

/// The `file:` URI of an absolute path. `Err` says why the path has none.
#[cfg(unix)]
fn file_uri(path: &Path) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;
    if !path.is_absolute() {
        return Err(format!("{} is not absolute", path.display()));
    }
    Ok(format!(
        "file://{}",
        percent_encode_path(path.as_os_str().as_bytes())
    ))
}

/// The `file:` URI of an absolute drive path: `C:\a b\x.rs` is
/// `file:///C:/a%20b/x.rs`. A UNC or device path is refused rather than
/// spelled as something a server would read as another file.
#[cfg(windows)]
fn file_uri(path: &Path) -> Result<String, String> {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let mut out = String::from("file:///");
    match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                out.push(char::from(letter));
                out.push(':');
            }
            _ => return Err(format!("{} has no drive letter", path.display())),
        },
        _ => return Err(format!("{} is not an absolute drive path", path.display())),
    }
    let mut wrote_segment = false;
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| format!("{} is not Unicode", path.display()))?;
                out.push('/');
                out.push_str(&percent_encode_path(part.as_bytes()));
                wrote_segment = true;
            }
            _ => return Err(format!("{} is not a plain absolute path", path.display())),
        }
    }
    if !wrote_segment {
        out.push('/');
    }
    Ok(out)
}

#[cfg(not(any(unix, windows)))]
fn file_uri(path: &Path) -> Result<String, String> {
    Err(format!(
        "{}: no file URI form is defined for this host",
        path.display()
    ))
}

/// The URI a server is told for the repository file `relative`.
///
/// `relative` must name a file *inside* `repo_root`: one or more `/`-separated
/// plain components. `..`, `.`, an empty component (which an absolute path
/// starts with), `\` and `:` (a drive or stream on Windows) are refused, so no
/// spelling of a repository-relative name opens a file outside the root.
fn uri_for_repo_file(repo_root: &Path, relative: &str) -> Result<String, String> {
    let plain =
        |part: &str| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':']);
    if !relative.split('/').all(plain) {
        return Err(format!(
            "{relative:?} is not a plain repository-relative path"
        ));
    }
    file_uri(&repo_root.join(relative))
}

/// The path a `file:` URI names, decoded. An authority other than empty or
/// `localhost` is another machine's file, so outside the repository; a query,
/// a fragment, a bad escape or a NUL is not a path at all.
fn path_from_file_uri(uri: &str) -> Result<PathBuf, LocationDrop> {
    const SCHEME: &str = "file://";
    let rest = match uri.get(..SCHEME.len()) {
        Some(scheme) if scheme.eq_ignore_ascii_case(SCHEME) => &uri[SCHEME.len()..],
        _ => return Err(LocationDrop::Undecodable),
    };
    let path_start = rest.find('/').ok_or(LocationDrop::Undecodable)?;
    let authority = &rest[..path_start];
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return Err(LocationDrop::OutsideRepo);
    }
    let encoded = &rest[path_start..];
    if encoded.contains(['?', '#']) {
        return Err(LocationDrop::Undecodable);
    }
    let bytes = percent_decode(encoded).ok_or(LocationDrop::Undecodable)?;
    if bytes.contains(&0) {
        return Err(LocationDrop::Undecodable);
    }
    native_path(bytes)
}

#[cfg(unix)]
fn native_path(bytes: Vec<u8>) -> Result<PathBuf, LocationDrop> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// `/C:/a/b` is `C:\a\b`. A path with no drive letter names no file this
/// host can open by it.
#[cfg(windows)]
fn native_path(bytes: Vec<u8>) -> Result<PathBuf, LocationDrop> {
    let text = String::from_utf8(bytes).map_err(|_| LocationDrop::Undecodable)?;
    let drive = text.as_bytes();
    if drive.len() < 3 || drive[0] != b'/' || !drive[1].is_ascii_alphabetic() || drive[2] != b':' {
        return Err(LocationDrop::Undecodable);
    }
    Ok(PathBuf::from(text[1..].replace('/', "\\")))
}

#[cfg(not(any(unix, windows)))]
fn native_path(_bytes: Vec<u8>) -> Result<PathBuf, LocationDrop> {
    Err(LocationDrop::Undecodable)
}

/// The repository-relative name of the file a server's URI names.
///
/// The URI is percent-decoded first: a server answers with an encoded URI, so
/// `file:///…/my%20code/a.rs` used to fail both the canonicalise and the
/// prefix strip, and every definition under a root with a space was dropped
/// while the pass reported it ran. A file that exists is compared by its
/// canonical path against the canonical root, so a link out of the repository
/// stays outside it; one that does not is compared lexically, and a `..` that
/// survives the strip is outside.
fn repo_relative_from_uri(repo_root: &Path, uri: &str) -> Result<String, LocationDrop> {
    let absolute = path_from_file_uri(uri)?;
    if !absolute.is_absolute() {
        return Err(LocationDrop::Undecodable);
    }
    let canonical_root = repo_root.canonicalize().ok();
    let relative = match absolute.canonicalize() {
        Ok(canonical) => {
            let root = canonical_root.as_deref().unwrap_or(repo_root);
            canonical.strip_prefix(root).map(Path::to_path_buf)
        }
        Err(_) => absolute
            .strip_prefix(repo_root)
            .or_else(|error| match canonical_root.as_deref() {
                Some(root) => absolute.strip_prefix(root),
                None => Err(error),
            })
            .map(Path::to_path_buf),
    }
    .map_err(|_| LocationDrop::OutsideRepo)?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => {
                parts.push(part.to_str().ok_or(LocationDrop::Undecodable)?)
            }
            _ => return Err(LocationDrop::OutsideRepo),
        }
    }
    if parts.is_empty() {
        return Err(LocationDrop::OutsideRepo);
    }
    Ok(parts.join("/"))
}

/// The in-repository locations of a definition answer, and a count of every
/// location dropped and why.
fn parse_locations(value: &Value, repo_root: &Path) -> (Vec<LspLocation>, LocationDrops) {
    let items: &[Value] = match value {
        Value::Object(_) => std::slice::from_ref(value),
        Value::Array(items) => items,
        _ => &[],
    };
    let mut out = Vec::new();
    let mut drops = LocationDrops::default();
    for item in items {
        match location_from_value(item, repo_root) {
            Ok(location) => out.push(location),
            Err(drop) => drops.count(drop),
        }
    }
    (out, drops)
}

fn location_from_value(value: &Value, repo_root: &Path) -> Result<LspLocation, LocationDrop> {
    let uri = value
        .get("targetUri")
        .or_else(|| value.get("uri"))
        .and_then(Value::as_str)
        .ok_or(LocationDrop::Undecodable)?;
    // Targets outside the repository are dropped — never a UniqueGlobal.
    let file = repo_relative_from_uri(repo_root, uri)?;
    let start = value
        .get("targetSelectionRange")
        .or_else(|| value.get("targetRange"))
        .or_else(|| value.get("range"))
        .and_then(|range| range.get("start"))
        .ok_or(LocationDrop::Undecodable)?;
    let coordinate = |key: &str| {
        start
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(LocationDrop::Undecodable)
    };
    Ok(LspLocation {
        file,
        line: coordinate("line")?,
        character: coordinate("character")?,
    })
}

/// URI and position handling, on every host: none of these start a server.
#[cfg(test)]
mod uris_and_positions {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A fresh directory under the system temp dir, removed on drop.
    pub(super) struct Scratch(pub(super) PathBuf);

    impl Scratch {
        pub(super) fn new(label: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "devmap-lsp-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn site(callee: &str, receiver: Option<&str>, start: usize, end: usize) -> UnresolvedReference {
        UnresolvedReference {
            source_file: "main.rs".into(),
            source_symbol: "main".into(),
            callee_name: callee.into(),
            kind: UnresolvedKind::Call,
            resolution: Resolution::Unresolved {
                reason: String::new(),
            },
            class: crate::model::UnresolvedClass::Unresolved,
            receiver: receiver.map(str::to_string),
            site: Some(crate::model::SiteBytes { start, end }),
        }
    }

    #[test]
    fn every_byte_survives_percent_encoding() {
        let all: Vec<u8> = (0..=255).collect();
        let encoded = percent_encode_path(&all);
        assert!(
            encoded.bytes().all(|byte| byte.is_ascii_graphic()),
            "{encoded}"
        );
        assert!(!encoded.contains([' ', '#', '?']), "{encoded}");
        assert_eq!(percent_decode(&encoded).as_deref(), Some(all.as_slice()));
        assert_eq!(percent_decode("a%2fb%2Fc").as_deref(), Some(&b"a/b/c"[..]));
    }

    #[test]
    fn a_broken_escape_is_refused_not_guessed() {
        for text in ["%", "a%2", "%zz", "%2g", "%%41"] {
            assert_eq!(percent_decode(text), None, "{text:?}");
        }
    }

    /// Acceptance: outbound URIs percent-encode the path and stay inside the
    /// repository. A space and a `#` are encoded; `..` and every other
    /// spelling that leaves the root is refused. No server is started.
    #[test]
    fn outbound_uris_encode_space_and_hash_and_refuse_dot_dot() {
        let scratch = Scratch::new("outbound");
        let root = scratch.0.join("my code #1");
        let root_uri = file_uri(&root).unwrap();
        let uri = uri_for_repo_file(&root, "src/a b#c.rs").unwrap();
        assert!(!uri.contains([' ', '#']), "unencoded: {uri}");
        assert!(uri.contains("my%20code%20%231/src/a%20b%23c.rs"), "{uri}");
        assert!(
            uri.starts_with(&format!("{root_uri}/")),
            "{uri} leaves {root_uri}"
        );
        for escaping in [
            "..",
            "../x.rs",
            "src/../../x.rs",
            "src/./x.rs",
            "/etc/passwd",
            "src//x.rs",
            "",
            "C:/x.rs",
            "src\\..\\x.rs",
        ] {
            assert!(
                uri_for_repo_file(&root, escaping).is_err(),
                "{escaping:?} was given a URI"
            );
        }
    }

    /// Acceptance: `repo_relative_from_uri(root, uri_for_repo_file(root,
    /// rel)) == rel` with a space, `#`, `%` and non-ASCII in both the root and
    /// the name, for files that exist (canonical comparison) and files that
    /// do not (lexical comparison).
    #[test]
    fn a_repository_file_round_trips_through_its_uri() {
        let scratch = Scratch::new("round-trip");
        let root = scratch.0.join("my code #1 100% café");
        let names = [
            "src/a b.rs",
            "src/hash#tag.rs",
            "src/50%.rs",
            "src/%41.rs",
            "src/naïve/日本.rs",
            "plain.rs",
        ];
        for name in names {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "fn x() {}\n").unwrap();
        }
        let canonical = root.canonicalize().unwrap();
        for name in names.iter().copied().chain(["src/not written.rs"]) {
            for root in [&root, &canonical] {
                let uri = uri_for_repo_file(root, name).unwrap();
                assert_eq!(
                    repo_relative_from_uri(root, &uri).as_deref(),
                    Ok(name),
                    "{uri}"
                );
            }
        }
    }

    /// Acceptance: a server's `%20` answer resolves inside a root with a
    /// space. Spelled the way a server would, lower-case escapes included,
    /// rather than through this module's own encoder.
    #[cfg(unix)]
    #[test]
    fn a_server_answer_with_percent_20_resolves_inside_a_root_with_a_space() {
        let scratch = Scratch::new("percent-20");
        let root = scratch.0.join("my code");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a b.rs"), "fn x() {}\n").unwrap();
        let spelled = format!("file://{}", root.join("src/a b.rs").display())
            .replace(' ', "%20")
            .replace("src", "%73rc");
        assert_eq!(
            repo_relative_from_uri(&root, &spelled).as_deref(),
            Ok("src/a b.rs")
        );
        let localhost = spelled.replacen("file://", "file://localhost", 1);
        assert_eq!(
            repo_relative_from_uri(&root, &localhost).as_deref(),
            Ok("src/a b.rs")
        );
    }

    /// The report must be able to tell an answer it could not read from one
    /// that pointed outside the repository.
    #[cfg(unix)]
    #[test]
    fn a_dropped_location_says_whether_it_was_undecodable_or_outside() {
        let scratch = Scratch::new("drops");
        let root = scratch.0.join("my code");
        std::fs::create_dir_all(&root).unwrap();
        let root_text = root.display().to_string().replace(' ', "%20");
        for (uri, expected) in [
            ("file:///etc/hosts".to_string(), LocationDrop::OutsideRepo),
            (
                "file://elsewhere/x.rs".to_string(),
                LocationDrop::OutsideRepo,
            ),
            (
                format!("file://{root_text}/../x.rs"),
                LocationDrop::OutsideRepo,
            ),
            (format!("file://{root_text}"), LocationDrop::OutsideRepo),
            (
                format!("file://{root_text}/a%zz.rs"),
                LocationDrop::Undecodable,
            ),
            (
                format!("file://{root_text}/a.rs#L3"),
                LocationDrop::Undecodable,
            ),
            (
                format!("file://{root_text}/a%00.rs"),
                LocationDrop::Undecodable,
            ),
            (
                "https://example.com/a.rs".to_string(),
                LocationDrop::Undecodable,
            ),
            ("file:relative.rs".to_string(), LocationDrop::Undecodable),
        ] {
            assert_eq!(repo_relative_from_uri(&root, &uri), Err(expected), "{uri}");
        }
        let answer = json!([
            { "uri": "file:///etc/hosts", "range": { "start": { "line": 0, "character": 0 } } },
            { "uri": "file:///a%zz", "range": { "start": { "line": 0, "character": 0 } } },
            { "uri": format!("file://{root_text}/b.rs") },
            { "uri": format!("file://{root_text}/b.rs"),
              "range": { "start": { "line": 2, "character": 4 } } },
        ]);
        let (locations, drops) = parse_locations(&answer, &root);
        assert_eq!(
            locations,
            vec![LspLocation {
                file: "b.rs".into(),
                line: 2,
                character: 4
            }]
        );
        assert_eq!(
            drops,
            LocationDrops {
                undecodable: 2,
                outside_repo: 1
            }
        );
        let status = LspServerStatus {
            binary: "x".into(),
            ran: true,
            locations_undecodable: drops.undecodable,
            locations_outside_repo: drops.outside_repo,
            ..LspServerStatus::default()
        };
        let lines = LspPassReport {
            servers: vec![status],
            ..LspPassReport::default()
        }
        .status_lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("2 location(s) as undecodable and 1 as outside")),
            "{lines:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_path_is_a_three_slash_uri_and_back() {
        let path = Path::new(r"C:\my code\a#b.rs");
        assert_eq!(file_uri(path).unwrap(), "file:///C:/my%20code/a%23b.rs");
        assert_eq!(
            path_from_file_uri("file:///C:/my%20code/a%23b.rs").unwrap(),
            PathBuf::from(r"C:\my code\a#b.rs")
        );
        assert_eq!(
            path_from_file_uri("file:///c%3A/my%20code/x.rs").unwrap(),
            PathBuf::from(r"c:\my code\x.rs")
        );
        // The old spelling was not a URI; it is refused, not misread.
        assert!(path_from_file_uri(r"file://C:\my code\x.rs").is_err());
    }

    /// The index answers what counting every byte before the site answered,
    /// at every byte of a source with multi-byte and astral characters.
    #[test]
    fn the_line_index_matches_a_prefix_scan() {
        let source = "fn a() {}\n// é 日本 🎉 x\n\n  b.run();\nlast";
        let index = LineIndex::new(source);
        for byte in 0..=source.len() + 2 {
            let mut at = byte.min(source.len());
            while !source.is_char_boundary(at) {
                at -= 1;
            }
            let prefix = &source[..at];
            let line = prefix.bytes().filter(|&b| b == b'\n').count() as u32;
            let start = prefix.rfind('\n').map_or(0, |found| found + 1);
            let character = source[start..at].encode_utf16().count() as u32;
            assert_eq!(
                index.position(source, byte),
                (line, character),
                "byte {byte}"
            );
        }
    }

    /// Acceptance (engine-02, pure half): two same-name calls with different
    /// receivers in one function are asked about at their own positions, and
    /// at the callee's name rather than at the receiver.
    #[test]
    fn same_name_calls_are_asked_about_at_their_own_callee() {
        let source = "fn main() { a.run(); b.run(); run.run(); }";
        let a = source.find("a.run()").unwrap();
        let b = source.find("b.run()").unwrap();
        let r = source.find("run.run()").unwrap();
        let byte_a = query_byte(source, &site("run", Some("a"), a, a + 7)).unwrap();
        let byte_b = query_byte(source, &site("run", Some("b"), b, b + 7)).unwrap();
        let byte_r = query_byte(source, &site("run", Some("run"), r, r + 9)).unwrap();
        assert_eq!(byte_a, a + 2);
        assert_eq!(byte_b, b + 2);
        assert_eq!(byte_r, r + 4);
        // Not spelled in the span: its start. No locator, or one past the
        // source: no position at all.
        let with = "with Engine() as e:";
        assert_eq!(
            query_byte(with, &site("__enter__", Some("Engine"), 5, 13)),
            Some(5)
        );
        let mut bare = site("run", None, 0, 3);
        bare.site = None;
        assert_eq!(query_byte(source, &bare), None);
        assert_eq!(query_byte(source, &site("run", None, 500, 510)), None);
    }
}

#[cfg(all(test, unix))]
mod session_bounds {
    use super::*;

    fn spec(script: String) -> LspServerSpec {
        LspServerSpec {
            binary: "sh",
            args: Box::leak(
                vec!["-c", Box::leak(script.into_boxed_str()) as &str].into_boxed_slice(),
            ),
            extensions: &[".rs"],
        }
    }

    fn alive(pid: i32) -> bool {
        let mut probe = subprocess::session_command("kill");
        probe.args(["-0", &pid.to_string()]);
        let bounds = subprocess::Bounds {
            deadline: Duration::from_secs(5),
            stdout_cap: 1024,
            stderr_cap: 1024,
        };
        subprocess::run_bounded(&mut probe, bounds).is_ok_and(|done| done.status.success())
    }

    /// A server that reads everything and never answers. The request budget
    /// used to be checked only between blocking reads, so this hung the pass
    /// forever; it must now end at the deadline, with the server's own child
    /// killed along with it.
    #[test]
    fn a_silent_server_times_out_at_the_deadline_and_takes_its_children_with_it() {
        let dir = std::env::temp_dir().join(format!("lsp-silent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("child.pid");
        let script = format!(
            "sleep 1000 & echo $! > {}; cat > /dev/null",
            pidfile.display()
        );
        let started = Instant::now();
        let deadline = started + Duration::from_secs(2);
        let result = LspClient::spawn(&spec(script), Path::new("sh"), &dir, deadline);
        let elapsed = started.elapsed();
        assert!(
            matches!(result, Err(LspDidNotRun::TimedOut)),
            "a silent server must time out, got {:?}",
            result.err()
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "took {elapsed:?} against a 2s deadline"
        );
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let reaped_by = Instant::now() + Duration::from_secs(2);
        while alive(pid) && Instant::now() < reaped_by {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the server's child {pid} outlived the kill");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bytes with no newline used to grow one header String without bound.
    #[test]
    fn an_endless_header_line_is_refused_not_buffered() {
        let script = "while :; do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'; done".to_string();
        let started = Instant::now();
        let result = LspClient::spawn(
            &spec(script),
            Path::new("sh"),
            &std::env::temp_dir(),
            started + Duration::from_secs(20),
        );
        match result {
            Err(LspDidNotRun::Protocol(reason)) => {
                assert!(reason.contains("header line longer"), "{reason}")
            }
            other => panic!("expected a header-cap refusal, got {:?}", other.err()),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A header promising a body, then one byte and silence. The read of the
    /// body must end at the deadline, not wait for the other 99 bytes.
    #[test]
    fn a_server_that_stalls_mid_body_times_out_at_the_deadline() {
        let script = "printf 'Content-Length: 100\\r\\n\\r\\n{'; cat > /dev/null".to_string();
        let started = Instant::now();
        let result = LspClient::spawn(
            &spec(script),
            Path::new("sh"),
            &std::env::temp_dir(),
            started + Duration::from_secs(1),
        );
        assert!(
            matches!(result, Err(LspDidNotRun::TimedOut)),
            "a stalled body must time out, got {:?}",
            result.err()
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?} against a 1s deadline",
            started.elapsed()
        );
    }

    /// Answers `initialize`, then ignores `shutdown` and `exit`, with a
    /// background child holding its pipes. Shutdown must still return, and
    /// the background child must die with the group.
    #[test]
    fn a_server_that_ignores_exit_is_killed_with_its_children() {
        let dir = std::env::temp_dir().join(format!("lsp-ignores-exit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("child.pid");
        let script = format!(
            "sleep 1000 & echo $! > {}; \
             printf 'Content-Length: 36\\r\\n\\r\\n{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}'; \
             cat > /dev/null",
            pidfile.display()
        );
        let mut client = LspClient::spawn(
            &spec(script),
            Path::new("sh"),
            &dir,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_or_else(|e| panic!("initialize was answered: {e:?}"));
        let started = Instant::now();
        client.shutdown().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "shutdown took {:?}",
            started.elapsed()
        );
        drop(client);
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let reaped_by = Instant::now() + Duration::from_secs(2);
        while alive(pid) && Instant::now() < reaped_by {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !alive(pid),
            "the server's background child {pid} outlived it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A language server in `sh`: answers `initialize` and `shutdown` with
    /// `null`, appends `line:character` of every definition request to `$1`,
    /// and answers each with one location in the URI `$2`, at line 0
    /// character 3. Bodies are read with `dd bs=1`, which never reads past
    /// the frame the way a buffered `head -c` can.
    const FAKE_SERVER: &str = r#"#!/bin/sh
log="$1"
answer="$2"
while :; do
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=$(printf '%s' "${line#Content-Length:}" | tr -d ' ') ;;
    esac
  done
  [ -z "$len" ] && exit 0
  body=$(dd bs=1 count="$len" 2>/dev/null)
  case "$body" in
    *'"method":"exit"'*) exit 0 ;;
  esac
  id=$(printf '%s' "$body" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$body" in
    *'"method":"textDocument/definition"'*)
      l=$(printf '%s' "$body" | sed -n 's/.*"line":\([0-9][0-9]*\).*/\1/p')
      c=$(printf '%s' "$body" | sed -n 's/.*"character":\([0-9][0-9]*\).*/\1/p')
      printf '%s:%s\n' "$l" "$c" >> "$log"
      result='[{"uri":"'"$answer"'","range":{"start":{"line":0,"character":3},"end":{"line":0,"character":9}}}]' ;;
    *) result='null' ;;
  esac
  resp='{"jsonrpc":"2.0","id":'"$id"',"result":'"$result"'}'
  printf 'Content-Length: %s\r\n\r\n%s' "$(printf '%s' "$resp" | wc -c | tr -d ' ')" "$resp"
done
"#;

    /// Acceptance, end to end through the pass with a fake server: a
    /// repository in a directory named `my code`; two `run` calls on
    /// different receivers in one function are asked about at their own
    /// `run`, and the server's percent-encoded answer becomes an edge rather
    /// than a silent drop.
    #[cfg(feature = "parse")]
    #[test]
    fn a_fake_server_session_in_my_code_asks_each_site_and_reads_encoded_answers() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = super::uris_and_positions::Scratch::new("session");
        let root = scratch.0.join("my code");
        let bin = scratch.0.join("bin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let server = bin.join("fake-lsp");
        std::fs::write(&server, FAKE_SERVER).unwrap();
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755)).unwrap();

        let source = "fn target() {}\n\nfn main() {\n    a.run();\n    b.run();\n}\n";
        std::fs::write(root.join("main.rs"), source).unwrap();
        let log = scratch.0.join("positions.log");
        let answer = file_uri(&root.canonicalize().unwrap().join("main.rs")).unwrap();
        assert!(answer.contains("my%20code"), "{answer}");

        let extraction = devmap_extract::extract_file("main.rs", source);
        let extractions = [extraction];
        let mut resolver = crate::resolver::Resolver::new();
        resolver.index_extractions(&extractions);
        let mut resolution = resolver.resolve_all(&extractions).unwrap();
        let runs: Vec<&UnresolvedReference> = resolution
            .unresolved
            .iter()
            .filter(|row| row.callee_name == "run")
            .collect();
        assert_eq!(runs.len(), 2, "{:?}", resolution.unresolved);

        let args: &'static [&'static str] = Box::leak(
            vec![
                Box::leak(log.display().to_string().into_boxed_str()) as &str,
                Box::leak(answer.into_boxed_str()) as &str,
            ]
            .into_boxed_slice(),
        );
        let spec = LspServerSpec {
            binary: "fake-lsp",
            args,
            extensions: &[".rs"],
        };
        let sources = BTreeMap::from([("main.rs".to_string(), source.to_string())]);
        let report = enrich_with_servers(
            &[spec],
            Some(bin.as_os_str()),
            &root,
            &extractions,
            &sources,
            &mut resolution,
            &AtomicBool::new(false),
        );

        let status = &report.servers[0];
        assert!(status.ran, "{:?}", report.status_lines());
        assert_eq!(status.locations_undecodable, 0, "{status:?}");
        assert_eq!(status.locations_outside_repo, 0, "{status:?}");
        let asked: BTreeSet<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        // `a.run` is line 3, `b.run` line 4; `run` is at character 6 on both.
        for expected in ["3:6", "4:6"] {
            assert!(asked.contains(expected), "{expected} not asked: {asked:?}");
        }
        assert!(
            !resolution
                .unresolved
                .iter()
                .any(|row| row.callee_name == "run"),
            "both run sites should have left the ledger: {:?}",
            resolution.unresolved
        );
        let lsp_edges: Vec<&ResolvedEdge> = resolution
            .edges
            .iter()
            .filter(|edge| {
                edge.details
                    .as_deref()
                    .is_some_and(|details| details.starts_with("lsp:"))
            })
            .collect();
        assert!(
            lsp_edges
                .iter()
                .any(|edge| edge.target_file == "main.rs" && edge.target_symbol == "target"),
            "{lsp_edges:?}"
        );
    }
}

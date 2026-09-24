//! Opt-in language-server edges for `devmap build --lsp`.
//!
//! The syntax resolver remains the owner of every deterministic and
//! unique-global claim. This pass may only *add* [`Resolution::LanguageServer`]
//! or [`Resolution::LanguageServerDispatch`] edges for sites that are still in
//! the unresolved ledger, and only when a server on `PATH` actually names a
//! target inside the repository. A missing, hung, oversized, or disagreeing
//! answer never falls through to a bare-name [`Resolution::UniqueGlobal`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use devmap_extract::model::{EdgeKind, ExtractedSymbol, Extraction, Span};
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

/// Servers this pass will start, in order, and only when the binary is on
/// `PATH`. A missing binary is a status line that the pass did not run — never
/// "zero new edges from a run".
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspDidNotRun {
    MissingBinary,
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
            LspDidNotRun::NonZeroExit { .. } => "non_zero_exit",
            LspDidNotRun::TimedOut => "timed_out",
            LspDidNotRun::OversizedPayload { .. } => "oversized_payload",
            LspDidNotRun::Cancelled => "cancelled",
            LspDidNotRun::Protocol(_) => "protocol",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerStatus {
    pub binary: String,
    pub version: Option<String>,
    pub ran: bool,
    pub did_not_run: Option<LspDidNotRun>,
    pub files_considered: usize,
    pub files_finished: usize,
    pub edges_added: usize,
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

fn byte_to_lsp_position(source: &str, byte: usize) -> (u32, u32) {
    let byte = byte.min(source.len());
    let prefix = &source[..byte];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map(|at| at + 1).unwrap_or(0);
    let character = source[line_start..byte].encode_utf16().count() as u32;
    (line, character)
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
        let Some(symbol) = symbols.iter().find(|symbol| {
            symbol.span.start_byte <= start_byte && start_byte < symbol.span.end_byte
        }) else {
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
            } else if server.ran {
                lines.push(format!(
                    "lsp: {} {} — {} edge(s) from {} file(s)",
                    server.binary,
                    server.version.as_deref().unwrap_or("unknown"),
                    server.edges_added,
                    server.files_finished
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

fn site_span<'a>(extractions: &'a [Extraction], site: &UnresolvedReference) -> Option<&'a Span> {
    let extraction = extractions
        .iter()
        .find(|extraction| extraction.file_path == site.source_file)?;
    match site.kind {
        UnresolvedKind::Call => extraction.calls.iter().find_map(|call| {
            let caller = call.caller_symbol.as_deref().unwrap_or("");
            if caller == site.source_symbol && call.callee_name == site.callee_name {
                Some(&call.span)
            } else {
                None
            }
        }),
        UnresolvedKind::Reference => extraction.references.iter().find_map(|reference| {
            let owner = reference.enclosing_symbol.as_deref().unwrap_or("");
            if owner == site.source_symbol && reference.name == site.callee_name {
                Some(&reference.span)
            } else {
                None
            }
        }),
        UnresolvedKind::Route | UnresolvedKind::Import => None,
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
        let Some(spec) = LSP_SERVERS
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

    for spec in LSP_SERVERS {
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
                files_considered: 0,
                files_finished: 0,
                edges_added: 0,
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

        let Some(binary_path) = which_binary(spec.binary) else {
            report.servers.push(LspServerStatus {
                binary: spec.binary.to_string(),
                version: None,
                ran: false,
                did_not_run: Some(LspDidNotRun::MissingBinary),
                files_considered: considered,
                files_finished: 0,
                edges_added: 0,
            });
            continue;
        };

        let mut client = match LspClient::spawn(spec, &binary_path, repo_root, deadline) {
            Ok(client) => client,
            Err(reason) => {
                report.servers.push(LspServerStatus {
                    binary: spec.binary.to_string(),
                    version: None,
                    ran: false,
                    did_not_run: Some(reason),
                    files_considered: considered,
                    files_finished: 0,
                    edges_added: 0,
                });
                continue;
            }
        };

        let version = client.server_version.clone();
        let mut finished_files = BTreeSet::new();
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
            let Some(source) = sources.get(site.source_file.as_str()) else {
                continue;
            };
            let Some(span) = site_span(extractions, site) else {
                continue;
            };
            let (line, character) = byte_to_lsp_position(source, span.start_byte);
            match client.definition(&site.source_file, source, line, character) {
                Ok(locations) => {
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
        let unfinished = considered.saturating_sub(finished_files.len());
        let did_not_run = match failed {
            Some(reason) if unfinished > 0 => Some(reason),
            Some(reason @ LspDidNotRun::OversizedPayload { .. })
            | Some(reason @ LspDidNotRun::NonZeroExit { .. })
            | Some(reason @ LspDidNotRun::MissingBinary)
            | Some(reason @ LspDidNotRun::Protocol(_))
            | Some(reason @ LspDidNotRun::TimedOut)
            | Some(reason @ LspDidNotRun::Cancelled) => Some(reason),
            None if unfinished > 0 => Some(LspDidNotRun::TimedOut),
            None => None,
        };
        report.servers.push(LspServerStatus {
            binary: spec.binary.to_string(),
            version,
            ran: did_not_run.is_none(),
            did_not_run,
            files_considered: considered,
            files_finished: finished_files.len(),
            edges_added: edges_for_server,
        });
        let _ = client.shutdown();
    }

    let edges_added = apply_language_server_resolutions(resolution, &decisions);
    report.edges_added = edges_added;
    report.sites_resolved = decisions.len();
    report.sites_left_unresolved = resolution.unresolved.len();
    report
}

fn which_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_exe = dir.join(format!("{name}.exe"));
            if with_exe.is_file() {
                return Some(with_exe);
            }
        }
    }
    None
}

struct LspClient {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    writer: std::process::ChildStdin,
    next_id: u64,
    server_version: Option<String>,
    deadline: Instant,
    open_files: BTreeSet<String>,
    repo_root: PathBuf,
}

impl LspClient {
    fn spawn(
        spec: &LspServerSpec,
        binary: &Path,
        repo_root: &Path,
        deadline: Instant,
    ) -> Result<Self, LspDidNotRun> {
        let mut command = Command::new(binary);
        command
            .args(spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .current_dir(repo_root);
        let mut child = command
            .spawn()
            .map_err(|error| LspDidNotRun::Protocol(format!("spawn {}: {error}", spec.binary)))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspDidNotRun::Protocol("missing stdout".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspDidNotRun::Protocol("missing stdin".into()))?;
        let mut client = Self {
            child,
            reader: BufReader::new(stdout),
            writer: stdin,
            next_id: 1,
            server_version: None,
            deadline,
            open_files: BTreeSet::new(),
            repo_root: repo_root.to_path_buf(),
        };
        let root_uri = format!("file://{}", repo_root.display());
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

    fn definition(
        &mut self,
        relative: &str,
        source: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspLocation>, LspDidNotRun> {
        if !self.open_files.contains(relative) {
            let uri = uri_for_repo_file(&self.repo_root, relative);
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
        let uri = uri_for_repo_file(&self.repo_root, relative);
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
        let _ = self.child.wait();
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
        write!(self.writer, "Content-Length: {}\r\n\r\n", body.len())
            .map_err(|error| LspDidNotRun::Protocol(format!("write header failed: {error}")))?;
        self.writer
            .write_all(&body)
            .map_err(|error| LspDidNotRun::Protocol(format!("write body failed: {error}")))?;
        self.writer
            .flush()
            .map_err(|error| LspDidNotRun::Protocol(format!("flush failed: {error}")))?;
        Ok(())
    }

    fn read_message(&mut self, request_deadline: Instant) -> Result<Value, LspDidNotRun> {
        let mut content_length: Option<usize> = None;
        loop {
            if Instant::now() >= request_deadline {
                self.kill();
                return Err(LspDidNotRun::TimedOut);
            }
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .map_err(|error| LspDidNotRun::Protocol(format!("read header failed: {error}")))?;
            if read == 0 {
                let status = self.child.try_wait().ok().flatten();
                return Err(LspDidNotRun::NonZeroExit {
                    code: status.and_then(|status| status.code()),
                });
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().ok();
            }
        }
        let length = content_length
            .ok_or_else(|| LspDidNotRun::Protocol("missing Content-Length".into()))?;
        if length > LSP_PAYLOAD_CAP {
            self.kill();
            return Err(LspDidNotRun::OversizedPayload { bytes: length });
        }
        let mut body = vec![0u8; length];
        self.reader
            .read_exact(&mut body)
            .map_err(|error| LspDidNotRun::Protocol(format!("read body failed: {error}")))?;
        serde_json::from_slice(&body)
            .map_err(|error| LspDidNotRun::Protocol(format!("json failed: {error}")))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

fn uri_for_repo_file(repo_root: &Path, relative: &str) -> String {
    format!("file://{}", repo_root.join(relative).display())
}

fn repo_relative_from_uri(repo_root: &Path, uri: &str) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let absolute = PathBuf::from(path);
    let root = repo_root.canonicalize().ok()?;
    let absolute = absolute.canonicalize().ok().unwrap_or(absolute);
    let relative = absolute.strip_prefix(&root).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn parse_locations(value: &Value, repo_root: &Path) -> Vec<LspLocation> {
    let mut out = Vec::new();
    match value {
        Value::Null => {}
        Value::Object(_) => {
            if let Some(location) = location_from_value(value, repo_root) {
                out.push(location);
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Some(location) = location_from_value(item, repo_root) {
                    out.push(location);
                }
            }
        }
        _ => {}
    }
    out
}

fn location_from_value(value: &Value, repo_root: &Path) -> Option<LspLocation> {
    let uri = value
        .get("targetUri")
        .or_else(|| value.get("uri"))
        .and_then(Value::as_str)?;
    // Targets outside the repository are dropped — never a UniqueGlobal.
    let file = repo_relative_from_uri(repo_root, uri)?;
    let range = value
        .get("targetSelectionRange")
        .or_else(|| value.get("targetRange"))
        .or_else(|| value.get("range"))?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()? as u32;
    let character = start.get("character")?.as_u64()? as u32;
    Some(LspLocation {
        file,
        line,
        character,
    })
}

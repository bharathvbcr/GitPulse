//! Jupyter notebook (`.ipynb`) extraction.
//!
//! **The problem a notebook poses.** The file is JSON; the code is inside it,
//! split across cells, each cell's source held as an array of strings with
//! every newline, quote and backslash escaped. Handing the raw bytes to a
//! Python grammar yields a parse error and no symbols. Handing it the
//! *reconstructed* code yields symbols whose spans index a string that exists
//! only in memory — and a span is a byte range into the file on disk. Every
//! consumer that renders a symbol slices the file with it, so a span into a
//! phantom buffer is not a smaller kind of correct, it is a wrong answer that
//! reads like a right one.
//!
//! **What this does instead.** Code cells are reconstructed and parsed, which
//! is the only way to get real symbols. Each resulting symbol is then
//! *relocated* through a structural map from decoded cell bytes to raw JSON
//! bytes. The same map relocates calls, references, imports, exports, lexical
//! bindings and parser diagnostics; duplicate text in prose or output cells
//! cannot steal a source location. A mapping failure refuses the extraction.
//!
//! Markdown cells are not scanned, for the reason `fallback` does not scan
//! Markdown: a type described in prose is not a type the notebook declares.

use crate::model::{Extraction, ExtractionEngine, ParseOutcome, Span};

/// Cells beyond this are not read.
///
/// Generated and checkpointed notebooks reach tens of thousands of cells.
/// Source mapping is linear in input size; exceeding this ingestion bound is
/// reported as a diagnostic, never silently truncated.
const MAX_CELLS: usize = 5_000;

/// A code cell's source, and where that source sits in the raw file.
struct Cell {
    code: String,
    /// Byte range in the raw `.ipynb` covering this cell's `source` value.
    raw_span: Span,
}

struct NotebookKernel {
    names: &'static [&'static str],
    grammar: &'static str,
    extension: &'static str,
}

// Dispatch and cache invalidation read the same table: a newly supported
// kernel must not leave notebook payloads keyed on an unavailable grammar.
const KERNELS: &[NotebookKernel] = &[
    NotebookKernel {
        names: &["python"],
        grammar: "python",
        extension: "py",
    },
    NotebookKernel {
        names: &["r"],
        grammar: "r",
        extension: "R",
    },
    NotebookKernel {
        names: &["julia"],
        grammar: "julia",
        extension: "jl",
    },
    NotebookKernel {
        names: &["javascript", "typescript"],
        grammar: "typescript",
        extension: "ts",
    },
    NotebookKernel {
        names: &["rust"],
        grammar: "rust",
        extension: "rs",
    },
    NotebookKernel {
        names: &["scala"],
        grammar: "scala",
        extension: "scala",
    },
];

pub(crate) fn kernel_grammars() -> impl Iterator<Item = &'static str> {
    KERNELS.iter().map(|kernel| kernel.grammar)
}

/// The language a notebook's kernel declares, mapped to an extractor language.
///
/// `language_info.name` is authoritative when present; `kernelspec.language`
/// is the fallback older notebooks carry. Anything unrecognised returns `None`
/// and the notebook is reported as unparsed rather than guessed at — a
/// notebook is not always Python, and assuming so would attribute R or Julia
/// declarations to a Python grammar.
fn kernel_language(doc: &serde_json::Value) -> Option<&'static str> {
    let declared = doc
        .get("metadata")
        .and_then(|meta| {
            meta.get("language_info")
                .and_then(|info| info.get("name"))
                .or_else(|| meta.get("kernelspec").and_then(|ks| ks.get("language")))
        })
        .and_then(|name| name.as_str())?
        .to_ascii_lowercase();

    KERNELS
        .iter()
        .find(|kernel| kernel.names.contains(&declared.as_str()))
        .map(|kernel| kernel.grammar)
}

/// The file extension a reconstructed cell buffer should be named with, so
/// `detect_language` routes it to the same grammar the kernel declares.
fn synthetic_extension(language: &str) -> &'static str {
    KERNELS
        .iter()
        .find(|kernel| kernel.grammar == language)
        .map(|kernel| kernel.extension)
        .unwrap_or("txt")
}

/// A cell's `source` is either a string or an array of strings.
fn cell_source(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(lines) => lines
            .iter()
            .map(|line| line.as_str())
            .collect::<Option<Vec<_>>>()
            .map(|lines| lines.concat()),
        _ => None,
    }
}

/// Read the immediate children of a validated JSON object or array with their
/// original byte offsets. Serde owns token parsing, escapes and nesting limits.
/// Repeated object keys remain ordered here; callers select the last one just
/// as serde_json::Value does.
fn json_children(raw: &str, span: &Span) -> Option<Vec<(Option<String>, Span)>> {
    let mut at = span.start_byte;
    let skip_space = |at: &mut usize| {
        while raw.as_bytes().get(*at).is_some_and(u8::is_ascii_whitespace) {
            *at += 1;
        }
    };
    skip_space(&mut at);
    let object = match raw.as_bytes().get(at)? {
        b'{' => true,
        b'[' => false,
        _ => return None,
    };
    at += 1;
    let mut values = Vec::new();
    loop {
        skip_space(&mut at);
        if raw.as_bytes().get(at) == Some(if object { &b'}' } else { &b']' }) {
            return Some(values);
        }
        let key = if object {
            let mut stream = serde_json::Deserializer::from_str(raw.get(at..span.end_byte)?)
                .into_iter::<String>();
            let key = stream.next()?.ok()?;
            at += stream.byte_offset();
            skip_space(&mut at);
            if raw.as_bytes().get(at) != Some(&b':') {
                return None;
            }
            at += 1;
            skip_space(&mut at);
            Some(key)
        } else {
            None
        };
        let start_byte = at;
        let mut stream = serde_json::Deserializer::from_str(raw.get(at..span.end_byte)?)
            .into_iter::<serde::de::IgnoredAny>();
        stream.next()?.ok()?;
        at += stream.byte_offset();
        values.push((
            key,
            Span {
                start_byte,
                end_byte: at,
            },
        ));
        skip_space(&mut at);
        if raw.as_bytes().get(at) == Some(&b',') {
            at += 1;
        } else if raw.as_bytes().get(at) != Some(if object { &b'}' } else { &b']' }) {
            return None;
        }
    }
}

fn json_field(raw: &str, span: &Span, name: &str) -> Option<Span> {
    json_children(raw, span)?
        .into_iter()
        .rev()
        .find(|(key, _)| key.as_deref() == Some(name))
        .map(|(_, span)| span)
}

struct SourceSegment {
    decoded: Span,
    raw: Span,
    escaped: bool,
}

#[derive(Default)]
struct SourceMap(Vec<SourceSegment>);

impl SourceMap {
    fn push(&mut self, decoded: Span, raw: Span, escaped: bool) {
        if !escaped {
            if let Some(last) = self.0.last_mut() {
                if !last.escaped
                    && last.decoded.end_byte == decoded.start_byte
                    && last.raw.end_byte == raw.start_byte
                {
                    last.decoded.end_byte = decoded.end_byte;
                    last.raw.end_byte = raw.end_byte;
                    return;
                }
            }
        }
        self.0.push(SourceSegment {
            decoded,
            raw,
            escaped,
        });
    }

    /// Serde validates and decodes each JSON string. This only tracks how many
    /// raw bytes represented each decoded character; it never interprets JSON
    /// independently. Adjacent unescaped characters share one map segment.
    fn append_string(&mut self, raw: &str, span: &Span, buffer: &mut String) -> Option<()> {
        let text: String = serde_json::from_str(raw.get(span.start_byte..span.end_byte)?).ok()?;
        let mut at = span.start_byte + 1;
        let base = buffer.len();
        for (offset, ch) in text.char_indices() {
            let escaped = raw.as_bytes().get(at) == Some(&b'\\');
            let width = if escaped && raw.as_bytes().get(at + 1) == Some(&b'u') {
                if ch.len_utf8() == 4 {
                    12
                } else {
                    6
                }
            } else if escaped {
                2
            } else {
                ch.len_utf8()
            };
            self.push(
                Span {
                    start_byte: base + offset,
                    end_byte: base + offset + ch.len_utf8(),
                },
                Span {
                    start_byte: at,
                    end_byte: at + width,
                },
                escaped,
            );
            at += width;
        }
        if at != span.end_byte.checked_sub(1)? {
            return None;
        }
        buffer.push_str(&text);
        Some(())
    }

    fn append_cell(&mut self, raw: &str, cell: &Cell, buffer: &mut String) -> Option<()> {
        let before = buffer.len();
        if raw.as_bytes().get(cell.raw_span.start_byte) == Some(&b'[') {
            for (_, span) in json_children(raw, &cell.raw_span)? {
                self.append_string(raw, &span, buffer)?;
            }
        } else {
            self.append_string(raw, &cell.raw_span, buffer)?;
        }
        if buffer.get(before..)? != cell.code {
            return None;
        }
        if !cell.code.ends_with('\n') {
            // The separator is synthesized, so it maps to the source value's
            // end boundary rather than to any code from the following cell.
            self.push(
                Span {
                    start_byte: buffer.len(),
                    end_byte: buffer.len() + 1,
                },
                Span {
                    start_byte: cell.raw_span.end_byte,
                    end_byte: cell.raw_span.end_byte,
                },
                true,
            );
            buffer.push('\n');
        }
        Some(())
    }

    fn span(&self, span: &Span) -> Option<Span> {
        if span.start_byte > span.end_byte {
            return None;
        }
        let first = self
            .0
            .partition_point(|part| part.decoded.end_byte <= span.start_byte);
        if span.start_byte == span.end_byte {
            if let Some(part) = self.0.get(first) {
                let at = if part.escaped {
                    part.raw.start_byte
                } else {
                    part.raw.start_byte + span.start_byte - part.decoded.start_byte
                };
                return Some(Span {
                    start_byte: at,
                    end_byte: at,
                });
            }
            let last = self.0.last()?;
            return (span.start_byte == last.decoded.end_byte).then_some(Span {
                start_byte: last.raw.end_byte,
                end_byte: last.raw.end_byte,
            });
        }
        let last = self
            .0
            .partition_point(|part| part.decoded.start_byte < span.end_byte)
            .checked_sub(1)?;
        let start = self.0.get(first)?;
        let end = self.0.get(last)?;
        if span.end_byte > end.decoded.end_byte {
            return None;
        }
        Some(Span {
            start_byte: if start.escaped {
                start.raw.start_byte
            } else {
                start.raw.start_byte + span.start_byte - start.decoded.start_byte
            },
            end_byte: if end.escaped {
                end.raw.end_byte
            } else {
                end.raw.start_byte + span.end_byte - end.decoded.start_byte
            },
        })
    }
}

/// Whether `path` is a notebook this module handles.
pub fn is_notebook(path: &str) -> bool {
    path.rsplit('.').next().is_some_and(|ext| ext == "ipynb")
}

/// Extract independent notebook code cells and relocate their semantic payloads.
///
/// Compatibility adapter for custom parsers. The callback must bound its own
/// execution time: an opaque synchronous callback cannot be cancelled here.
/// The shared deadline is checked before and after every callback; production
/// uses `extract_notebook_bounded` to pass the remaining budget to tree-sitter.
pub fn extract_notebook(
    path: &str,
    raw: &str,
    parse: impl Fn(&str, &str, &str) -> Extraction,
) -> Extraction {
    extract_notebook_with_parser(
        path,
        raw,
        crate::treesitter::DEFAULT_PARSE_BUDGET,
        |path, language, source, _remaining| parse(path, language, source),
    )
}

/// Production entry: every cell receives only the remaining shared budget.
pub(crate) fn extract_notebook_bounded(path: &str, raw: &str) -> Extraction {
    extract_notebook_with_parser(
        path,
        raw,
        crate::treesitter::DEFAULT_PARSE_BUDGET,
        crate::treesitter::extract_treesitter_with_budget,
    )
}

fn extract_notebook_with_parser(
    path: &str,
    raw: &str,
    budget: std::time::Duration,
    parse: impl Fn(&str, &str, &str, std::time::Duration) -> Extraction,
) -> Extraction {
    let started = std::time::Instant::now();
    if raw.len() as u64 > crate::MAX_SOURCE_BYTES {
        return crate::treesitter::refused_extraction(
            path,
            "notebook",
            raw,
            format!(
                "source has {} bytes, over the {} byte input limit",
                raw.len(),
                crate::MAX_SOURCE_BYTES
            ),
        );
    }
    let mut diagnostics: Vec<String> = Vec::new();

    // The grammarless path already produces a correctly shaped `Extraction`
    // for this file — content hash, the `File` node (K1), and every field this
    // struct has grown — so it is the base rather than a hand-built literal
    // that would silently miss whatever field is added next. `"notebook"` is in
    // `fallback::NON_DECLARATIVE_LANGUAGES`, so no line scan runs over the raw
    // JSON on the way through.
    let base = |outcome: ParseOutcome, engine: ExtractionEngine, diagnostics: Vec<String>| {
        let mut extraction = crate::extract_treesitter(path, "notebook", raw);
        extraction.parse_outcome = outcome;
        extraction.engine = engine;
        extraction.diagnostics = diagnostics;
        extraction
    };

    let Some(deadline) = started.checked_add(budget) else {
        return base(
            ParseOutcome::Failed {
                reason: "notebook extraction budget is outside the supported clock range"
                    .to_string(),
            },
            ExtractionEngine::Unavailable {
                requested_language: "notebook".to_string(),
            },
            diagnostics,
        );
    };
    if std::time::Instant::now() >= deadline {
        return base(
            ParseOutcome::Failed {
                reason: format!(
                    "notebook extraction exceeded the {budget:?} budget before parsing"
                ),
            },
            ExtractionEngine::Unavailable {
                requested_language: "notebook".to_string(),
            },
            diagnostics,
        );
    }

    let doc: serde_json::Value = match serde_json::from_str(raw) {
        Ok(doc) => doc,
        Err(error) => {
            return base(
                ParseOutcome::Failed {
                    reason: format!("notebook is not valid JSON: {error}"),
                },
                ExtractionEngine::Unavailable {
                    requested_language: "notebook".to_string(),
                },
                diagnostics,
            );
        }
    };

    let Some(language) = kernel_language(&doc) else {
        // Not a guess. A notebook with no declared kernel, or one this build
        // has no grammar for, is reported as unhandled rather than parsed as
        // Python — which would attribute R or Julia declarations to a Python
        // grammar and put symbols in the graph that the notebook never declared.
        return base(
            ParseOutcome::Failed {
                reason: "notebook declares no kernel language this build can parse".to_string(),
            },
            ExtractionEngine::Unavailable {
                requested_language: "notebook".to_string(),
            },
            diagnostics,
        );
    };

    let all_cells = doc.get("cells").and_then(|cells| cells.as_array());
    let Some(all_cells) = all_cells else {
        return base(
            ParseOutcome::Failed {
                reason: "notebook has no `cells` array".to_string(),
            },
            ExtractionEngine::Unavailable {
                requested_language: "notebook".to_string(),
            },
            diagnostics,
        );
    };
    // Counted here and carried all the way to `parse_outcome` below. It used
    // to live only in `diagnostics`, which `for_durable_store()` clears before
    // the payload reaches `generation_files.extraction_json` and the extraction
    // cache — so the stored record of a 6,000-cell notebook read `Clean`, was
    // cache-admitted under a real content hash, and a caller could not tell it
    // from a notebook read in full. Same defect, same fix, as the fallback
    // scanner's truncation count in schema v29.
    let dropped_cells = all_cells.len().saturating_sub(MAX_CELLS);
    if dropped_cells > 0 {
        diagnostics.push(format!(
            "notebook has {} cells; only the first {MAX_CELLS} were read",
            all_cells.len()
        ));
    }

    let raw_cell_spans = json_field(
        raw,
        &Span {
            start_byte: 0,
            end_byte: raw.len(),
        },
        "cells",
    )
    .and_then(|span| json_children(raw, &span));
    let Some(raw_cell_spans) = raw_cell_spans else {
        return base(
            ParseOutcome::Failed {
                reason: "notebook cell source positions could not be decoded".to_string(),
            },
            ExtractionEngine::Notebook {
                kernel_language: language.to_string(),
            },
            diagnostics,
        );
    };
    let mut invalid_cells = 0usize;
    let cells: Vec<Cell> = all_cells
        .iter()
        .take(MAX_CELLS)
        .enumerate()
        .filter_map(|(index, cell)| {
            match cell.get("cell_type").and_then(|kind| kind.as_str()) {
                Some("markdown" | "raw") => return None,
                Some("code") => {}
                _ => {
                    invalid_cells += 1;
                    return None;
                }
            }
            let Some(code) = cell.get("source").and_then(cell_source) else {
                invalid_cells += 1;
                return None;
            };
            let Some(raw_span) = raw_cell_spans
                .get(index)
                .and_then(|(_, cell_span)| json_field(raw, cell_span, "source"))
            else {
                invalid_cells += 1;
                return None;
            };
            Some(Cell { code, raw_span })
        })
        .collect();

    // A malformed code cell has unknown contents. Failing the container is
    // safer than parsing a concatenation that silently removed part of it.
    if invalid_cells > 0 {
        return base(
            ParseOutcome::Failed {
                reason: format!("notebook has {invalid_cells} malformed cell(s): each cell needs a known cell_type and code source must be a string or an array of strings"),
            },
            ExtractionEngine::Notebook {
                kernel_language: language.to_string(),
            },
            diagnostics,
        );
    }

    // A notebook shares names across cells, not parser state. Independent
    // parsing prevents a dangling function/string/bracket in one cell from
    // borrowing the next cell's source and being published as Clean.
    let synthetic = format!("{path}.{}", synthetic_extension(language));
    let mut extraction = base(
        ParseOutcome::Clean,
        ExtractionEngine::Notebook {
            kernel_language: language.to_string(),
        },
        diagnostics,
    );
    let mut partial_ranges = Vec::new();
    let mut incomplete = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let fail = |reason: String| {
            base(
                ParseOutcome::Failed {
                    reason: format!("notebook code cell {}: {reason}", index + 1),
                },
                ExtractionEngine::Notebook {
                    kernel_language: language.to_string(),
                },
                Vec::new(),
            )
        };
        let mut buffer = String::new();
        let mut source_map = SourceMap::default();
        if source_map.append_cell(raw, cell, &mut buffer).is_none() {
            return fail("source map could not be decoded".to_string());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return fail(format!("extraction exceeded the shared {budget:?} budget"));
        }
        let parsed = parse(&synthetic, language, &buffer, remaining);
        if std::time::Instant::now() >= deadline {
            return fail(format!("extraction exceeded the shared {budget:?} budget"));
        }
        if let ParseOutcome::Failed { reason } = &parsed.parse_outcome {
            return fail(reason.clone());
        }
        let Some(parsed) = relocate_cell(parsed, &source_map, path, &synthetic) else {
            return fail("source span falls outside this cell".to_string());
        };
        match parsed.parse_outcome {
            ParseOutcome::Clean => {}
            ParseOutcome::Partial { error_ranges } => partial_ranges.extend(error_ranges),
            ParseOutcome::Fallback { reason } | ParseOutcome::Skipped { reason } => {
                incomplete.push(format!("cell {}: {reason}", index + 1));
            }
            ParseOutcome::Failed { reason } => return fail(reason),
        }
        extraction.symbols.extend(parsed.symbols);
        extraction.imports.extend(parsed.imports);
        extraction.calls.extend(parsed.calls);
        extraction.exports.extend(parsed.exports);
        extraction.references.extend(parsed.references);
        extraction.routes.extend(parsed.routes);
        extraction.wiring.extend(parsed.wiring);
        extraction.diagnostics.extend(parsed.diagnostics);
        extraction.scope_locals.extend(parsed.scope_locals);
        extraction.local_bindings.extend(parsed.local_bindings);
        // These are empty for currently supported kernels, but remain part of
        // the merge contract if a kernel with Go metadata is added later.
        extraction.go_package = parsed.go_package.or(extraction.go_package);
        extraction.go_build_constrained |= parsed.go_build_constrained;
        extraction
            .go_interface_methods
            .extend(parsed.go_interface_methods);
        extraction.go_method_params.extend(parsed.go_method_params);
    }
    if dropped_cells > 0 {
        incomplete.push(format!("notebook has {} cells; only the first {MAX_CELLS} were read and {dropped_cells} were not", all_cells.len()));
    }
    if !incomplete.is_empty() {
        if !partial_ranges.is_empty() {
            incomplete.push("parsed cells also contain syntax errors".to_string());
        }
        extraction.parse_outcome = ParseOutcome::Fallback {
            reason: incomplete.join("; "),
        };
    } else if !partial_ranges.is_empty() {
        partial_ranges.sort_by_key(|range| (range.start_byte, range.end_byte));
        partial_ranges.dedup();
        extraction.parse_outcome = ParseOutcome::Partial {
            error_ranges: partial_ranges,
        };
    }
    extraction.local_bindings.sort();
    extraction.local_bindings.dedup();
    extraction.scope_locals.sort();
    extraction.scope_locals.dedup();
    if std::time::Instant::now() >= deadline {
        return base(
            ParseOutcome::Failed {
                reason: format!("notebook finalization exceeded the shared {budget:?} budget"),
            },
            ExtractionEngine::Notebook {
                kernel_language: language.to_string(),
            },
            Vec::new(),
        );
    }
    extraction
}

/// One owner for translating all fields from a single execution unit.
fn relocate_cell(
    mut parsed: Extraction,
    source_map: &SourceMap,
    path: &str,
    synthetic: &str,
) -> Option<Extraction> {
    let synthetic_prefix = format!("{synthetic}::");
    let real_prefix = format!("{path}::");
    let rewrite_owner = |owner: &mut String| {
        if owner.as_str() == synthetic {
            *owner = path.to_string();
        } else if let Some(rest) = owner.strip_prefix(&synthetic_prefix) {
            *owner = format!("{real_prefix}{rest}");
        }
    };
    parsed
        .symbols
        .retain(|symbol| symbol.kind != crate::model::SymbolKind::File);
    let mut unmapped = 0usize;
    let mut relocate = |span: &mut Span| {
        if let Some(mapped) = source_map.span(span) {
            *span = mapped;
        } else {
            unmapped += 1;
        }
    };
    for symbol in &mut parsed.symbols {
        relocate(&mut symbol.span);
        rewrite_owner(&mut symbol.qualified_name);
        if let Some(owner) = &mut symbol.parent_symbol {
            rewrite_owner(owner);
        }
    }
    for import in &mut parsed.imports {
        relocate(&mut import.span);
    }
    for call in &mut parsed.calls {
        relocate(&mut call.span);
        if let Some(owner) = &mut call.caller_symbol {
            rewrite_owner(owner);
        }
    }
    for reference in &mut parsed.references {
        relocate(&mut reference.span);
        if let Some(owner) = &mut reference.enclosing_symbol {
            rewrite_owner(owner);
        }
    }
    for export in &mut parsed.exports {
        relocate(&mut export.span);
        rewrite_owner(&mut export.exported_name);
        if let Some(owner) = &mut export.local_name {
            rewrite_owner(owner);
        }
    }
    for route in &mut parsed.routes {
        relocate(&mut route.span);
    }
    for binding in &mut parsed.local_bindings {
        let mut point = Span {
            start_byte: binding.start_byte,
            end_byte: binding.start_byte,
        };
        relocate(&mut point);
        binding.start_byte = point.start_byte;
        if let Some(owner) = &mut binding.scope {
            rewrite_owner(owner);
        }
    }
    if let ParseOutcome::Partial { error_ranges } = &mut parsed.parse_outcome {
        for range in error_ranges {
            let mut span = Span {
                start_byte: range.start_byte,
                end_byte: range.end_byte,
            };
            relocate(&mut span);
            range.start_byte = span.start_byte;
            range.end_byte = span.end_byte;
        }
    }
    for (owner, _) in &mut parsed.scope_locals {
        rewrite_owner(owner);
    }
    // Synthetic filename-based file annotations do not describe the notebook;
    // keep the raw file's annotations and preserve real symbol-scoped wiring.
    parsed
        .wiring
        .retain(|annotation| annotation.target_symbol != synthetic);
    for annotation in &mut parsed.wiring {
        rewrite_owner(&mut annotation.target_symbol);
    }
    for method in &mut parsed.go_method_params {
        rewrite_owner(&mut method.qualified_name);
    }
    (unmapped == 0).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExtractedSymbol, SymbolKind};

    #[test]
    fn every_cell_receives_only_the_remaining_notebook_budget() {
        let raw = notebook(
            "python",
            &[
                ("code", "def first():\n    pass"),
                ("code", "def other():\n    pass"),
            ],
        );
        let seen = std::cell::RefCell::new(Vec::new());
        let budget = std::time::Duration::from_secs(2);
        let result = extract_notebook_with_parser(
            "budget.ipynb",
            &raw,
            budget,
            |path, language, source, remaining| {
                seen.borrow_mut().push(remaining);
                let result = crate::treesitter::extract_treesitter_with_budget(
                    path, language, source, remaining,
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
                result
            },
        );
        assert!(matches!(result.parse_outcome, ParseOutcome::Clean));
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert!(
            seen[0] < budget && seen[1] < seen[0],
            "budgets were reset per cell: {seen:?}"
        );
    }

    #[test]
    fn exhausted_notebook_budget_discards_the_prefix_and_stops_parsing() {
        let raw = notebook(
            "python",
            &[
                ("code", "def first():\n    pass"),
                ("code", "def other():\n    pass"),
            ],
        );
        let calls = std::cell::Cell::new(0);
        let result = extract_notebook_with_parser(
            "budget.ipynb",
            &raw,
            std::time::Duration::from_millis(200),
            |path, language, source, remaining| {
                calls.set(calls.get() + 1);
                let result = crate::extract_treesitter(path, language, source);
                std::thread::sleep(remaining + std::time::Duration::from_millis(2));
                result
            },
        );
        assert_eq!(calls.get(), 1);
        assert!(matches!(result.parse_outcome, ParseOutcome::Failed { .. }));
        assert!(result
            .symbols
            .iter()
            .all(|symbol| symbol.kind == SymbolKind::File));
    }

    #[test]
    fn zero_or_unrepresentable_notebook_budgets_refuse_before_callback() {
        let raw = notebook("python", &[("code", "def first():\n    pass")]);
        for budget in [std::time::Duration::ZERO, std::time::Duration::MAX] {
            let result =
                extract_notebook_with_parser("budget.ipynb", &raw, budget, |_, _, _, _| {
                    panic!("invalid budget reached callback")
                });
            assert!(matches!(result.parse_outcome, ParseOutcome::Failed { .. }));
        }
    }

    fn notebook(language: &str, cells: &[(&str, &str)]) -> String {
        let cells: Vec<serde_json::Value> = cells
            .iter()
            .map(|(kind, code)| {
                serde_json::json!({
                    "cell_type": kind,
                    "source": code.lines().map(|l| format!("{l}\n")).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "metadata": {"language_info": {"name": language}},
            "cells": cells,
        })
        .to_string()
    }

    fn extract(raw: &str) -> Extraction {
        extract_notebook("analysis.ipynb", raw, crate::extract_treesitter)
    }

    #[test]
    fn code_cells_yield_real_declarations() {
        let raw = notebook(
            "python",
            &[
                ("markdown", "# Analysis"),
                ("code", "def load(path):\n    return open(path)"),
                ("code", "class Model:\n    pass"),
            ],
        );
        let extraction = extract(&raw);
        let names: Vec<&str> = extraction
            .symbols
            .iter()
            .filter(|s| s.kind != SymbolKind::File)
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"load"), "{names:?}");
        assert!(names.contains(&"Model"), "{names:?}");
    }

    /// **The property this module exists to get right.**
    ///
    /// A span is a byte range into the file on disk. Symbols here are parsed
    /// from a reconstructed buffer that exists only in memory, so an
    /// unrelocated span would index a phantom and render as whatever bytes sit
    /// at that offset in the JSON — a wrong answer that reads like a right one.
    #[test]
    fn spans_index_the_raw_notebook_not_the_reconstructed_buffer() {
        let raw = notebook(
            "python",
            &[
                ("markdown", "# padding to push the offsets apart"),
                ("code", "def load(path):\n    return open(path)"),
            ],
        );
        let extraction = extract(&raw);
        for symbol in extraction
            .symbols
            .iter()
            .filter(|s| s.kind != SymbolKind::File)
        {
            let slice = raw
                .get(symbol.span.start_byte..symbol.span.end_byte)
                .expect("every span must be a valid range into the raw file");
            assert!(
                slice.contains(&symbol.name),
                "the span for {} must cover its declaration in the raw \
                 notebook; got {slice:?}",
                symbol.name
            );
        }
    }

    /// Prose cells declare nothing — the K3 rule, applied to notebooks.
    #[test]
    fn markdown_cells_are_not_scanned_for_declarations() {
        let raw = notebook(
            "python",
            &[(
                "markdown",
                "We will add:\n\n```go\ntype Invented struct {}\n```",
            )],
        );
        let extraction = extract(&raw);
        let declared: Vec<&str> = extraction
            .symbols
            .iter()
            .filter(|s| s.kind != SymbolKind::File)
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            declared.is_empty(),
            "a type described in a markdown cell is not declared by the \
             notebook: {declared:?}"
        );
    }

    /// Exactly one `File` node, named for the notebook. (K1)
    ///
    /// The reconstructed buffer is a file to the extractor, so it emits a
    /// `File` node of its own for the synthetic path. That node describes a
    /// buffer that exists only in memory; shipping it put two File nodes in the
    /// graph for one file, the second addressed as `analysis.ipynb.py`.
    #[test]
    fn exactly_one_file_node_named_for_the_notebook() {
        let raw = notebook(
            "python",
            &[("code", "def load(path):\n    return open(path)")],
        );
        let extraction = extract(&raw);
        let files: Vec<&ExtractedSymbol> = extraction
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::File)
            .collect();
        assert_eq!(
            files.len(),
            1,
            "one file, one File node; got {:?}",
            files.iter().map(|f| &f.qualified_name).collect::<Vec<_>>()
        );
        assert_eq!(
            files[0].qualified_name, "analysis.ipynb",
            "the node must address the notebook, not the reconstructed buffer"
        );
    }

    /// A notebook is addressable even when it declares nothing. (K1)
    #[test]
    fn a_notebook_always_gets_a_file_node() {
        for raw in [
            notebook("python", &[]),
            notebook("python", &[("markdown", "just prose")]),
            "{not json at all".to_string(),
        ] {
            let extraction = extract(&raw);
            assert!(
                extraction
                    .symbols
                    .iter()
                    .any(|s| s.kind == SymbolKind::File),
                "every notebook must be an addressable node"
            );
        }
    }

    /// An unknown kernel is reported, never assumed to be Python.
    ///
    /// Guessing would attribute R or Julia declarations to a Python grammar,
    /// putting symbols in the graph the notebook never declared.
    #[test]
    fn an_unrecognised_kernel_is_not_guessed_at() {
        let raw = serde_json::json!({
            "metadata": {"language_info": {"name": "haskell"}},
            "cells": [{"cell_type": "code", "source": ["main :: IO ()\n"]}],
        })
        .to_string();
        let extraction = extract(&raw);
        assert!(
            matches!(extraction.parse_outcome, ParseOutcome::Failed { .. }),
            "an unparseable kernel must report Failed, got {:?}",
            extraction.parse_outcome
        );
        assert!(matches!(
            extraction.engine,
            ExtractionEngine::Unavailable { .. }
        ));
    }

    /// Malformed JSON fails loudly rather than yielding a partial notebook.
    #[test]
    fn invalid_json_is_reported_not_salvaged() {
        let extraction = extract("{\"cells\": [ truncated");
        match &extraction.parse_outcome {
            ParseOutcome::Failed { reason } => {
                assert!(reason.contains("valid JSON"), "{reason}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Cells share notebook identities, so cross-cell calls remain resolvable
    /// while each code cell remains an independent parse unit.
    /// The synthetic filename must not survive anywhere in the extraction.
    ///
    /// It leaked three times before this test existed: into `qualified_name`
    /// (fixed at relocation), into `caller_symbol` on every call, and into
    /// `parent_symbol` on every top-level declaration. The last one was the
    /// quiet one — the resolver only emits a second `Contains` edge when a
    /// symbol's parent differs from its file, and a parent naming
    /// `analysis.ipynb.py` differs from `analysis.ipynb`, so every notebook
    /// symbol got a containment edge from a file that does not exist.
    ///
    /// Asserting on the *substring* rather than on the three known fields is
    /// deliberate: the next field added to `ExtractedSymbol` gets the same
    /// guarantee without anyone remembering to extend this.
    #[test]
    fn no_synthetic_filename_survives_into_the_extraction() {
        let raw = notebook(
            "python",
            &[
                ("code", "def outer():\n    return 1\n"),
                (
                    "code",
                    "class Holder:\n    def method(self):\n        return outer()\n",
                ),
            ],
        );
        let extraction = extract_notebook("nb.ipynb", &raw, crate::extract_treesitter);

        let rendered = format!("{extraction:?}");
        assert!(
            !rendered.contains("nb.ipynb.py"),
            "the synthetic parse filename leaked into the extraction: {rendered}"
        );
        // Non-vacuity: the fixture has to actually produce the fields that
        // carried the leak, or this passes for the wrong reason.
        assert!(
            extraction.symbols.iter().any(|s| s.parent_symbol.is_some()),
            "fixture produced no parented symbol"
        );
        assert!(
            extraction.calls.iter().any(|c| c.caller_symbol.is_some()),
            "fixture produced no attributed call"
        );

        // Every attributed caller must name a symbol this extraction declares.
        // The prefix rewrite alone did not give this: rebuilding a method's
        // qualified name from `symbol.name` dropped the type, so the call from
        // `Holder.method` referred to a symbol recorded as plain `method`.
        let declared: Vec<&str> = extraction
            .symbols
            .iter()
            .map(|s| s.qualified_name.as_str())
            .collect();
        for call in &extraction.calls {
            if let Some(caller) = call.caller_symbol.as_deref() {
                assert!(
                    declared.contains(&caller),
                    "call attributed to {caller}, which this notebook does not declare: {declared:?}"
                );
            }
        }
        assert!(
            declared.iter().any(|name| name.contains("Holder.method")),
            "the method kept its type qualifier: {declared:?}"
        );
    }

    #[test]
    fn a_call_across_cells_is_seen() {
        let raw = notebook(
            "python",
            &[
                ("code", "def helper():\n    return 1"),
                ("code", "def run():\n    return helper()"),
            ],
        );
        let extraction = extract(&raw);
        assert!(
            extraction.calls.iter().any(|c| c.callee_name == "helper"),
            "a cross-cell call must resolve: {:?}",
            extraction.calls
        );
    }

    /// `source` may be a bare string rather than an array of lines.
    #[test]
    fn a_string_valued_source_is_accepted() {
        let raw = serde_json::json!({
            "metadata": {"kernelspec": {"language": "python"}},
            "cells": [{"cell_type": "code", "source": "def only(x):\n    return x\n"}],
        })
        .to_string();
        let extraction = extract(&raw);
        assert!(extraction.symbols.iter().any(|s| s.name == "only"));
    }
}

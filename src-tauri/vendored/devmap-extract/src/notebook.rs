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
//! *relocated*: its declaration is found in the raw file bytes, and the span it
//! carries is that range. A symbol whose declaration cannot be located keeps
//! the span of the cell it came from, which is coarse but true. One that cannot
//! even be attributed to a cell is dropped and counted — never emitted with a
//! guessed span.
//!
//! Markdown cells are not scanned, for the reason `fallback` does not scan
//! Markdown: a type described in prose is not a type the notebook declares.

use crate::model::{ExtractedSymbol, Extraction, ExtractionEngine, ParseOutcome, Span};

/// Cells beyond this are not read.
///
/// Generated and checkpointed notebooks reach tens of thousands of cells, and
/// the relocation pass below is linear in cells × symbols. Exceeding it is
/// reported as a diagnostic, never silently truncated.
const MAX_CELLS: usize = 5_000;

/// A code cell's source, and where that source sits in the raw file.
struct Cell {
    code: String,
    /// Byte range in the raw `.ipynb` covering this cell's `source` value.
    raw_span: Option<Span>,
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

    Some(match declared.as_str() {
        "python" => "python",
        "r" => "r",
        "julia" => "julia",
        "javascript" | "typescript" => "typescript",
        "rust" => "rust",
        "scala" => "scala",
        _ => return None,
    })
}

/// The file extension a reconstructed cell buffer should be named with, so
/// `detect_language` routes it to the same grammar the kernel declares.
fn synthetic_extension(language: &str) -> &'static str {
    match language {
        "python" => "py",
        "r" => "R",
        "julia" => "jl",
        "typescript" => "ts",
        "rust" => "rs",
        "scala" => "scala",
        _ => "txt",
    }
}

/// A cell's `source` is either a string or an array of strings.
fn cell_source(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(lines) => Some(
            lines
                .iter()
                .filter_map(|line| line.as_str())
                .collect::<Vec<_>>()
                .concat(),
        ),
        _ => None,
    }
}

/// Locate `needle` in `raw`, returning the byte range of the line containing it.
///
/// Used to relocate a reconstructed symbol into the file on disk. The search is
/// over the *escaped* form, because that is what the raw bytes hold: a
/// declaration written `def load(path):` appears in the JSON as
/// `"def load(path):\n"`, so the unescaped needle would never be found.
fn locate_line(raw: &str, needle: &str) -> Option<Span> {
    if needle.is_empty() {
        return None;
    }
    // `serde_json::to_string` of a string yields the quoted, escaped form;
    // trimming the quotes leaves exactly the bytes the file contains.
    let escaped = serde_json::to_string(needle).ok()?;
    let escaped = escaped.get(1..escaped.len().saturating_sub(1))?;
    let at = raw.find(escaped)?;
    Some(Span {
        start_byte: at,
        end_byte: at + escaped.len(),
    })
}

/// Whether `path` is a notebook this module handles.
pub fn is_notebook(path: &str) -> bool {
    path.rsplit('.').next().is_some_and(|ext| ext == "ipynb")
}

/// Extract a notebook by reconstructing its code cells and relocating the
/// symbols back into the raw file.
pub fn extract_notebook(
    path: &str,
    raw: &str,
    parse: impl Fn(&str, &str, &str) -> Extraction,
) -> Extraction {
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

    let cells: Vec<Cell> = all_cells
        .iter()
        .take(MAX_CELLS)
        .filter(|cell| {
            cell.get("cell_type")
                .and_then(|kind| kind.as_str())
                .is_some_and(|kind| kind == "code")
        })
        .filter_map(|cell| {
            let code = cell_source(cell.get("source")?)?;
            let raw_span = code
                .lines()
                .next()
                .and_then(|first| locate_line(raw, first));
            Some(Cell { code, raw_span })
        })
        .collect();

    if cells.is_empty() {
        // A notebook of prose and outputs declares nothing. That is a complete
        // answer, not a failure to parse one.
        return base(
            ParseOutcome::Clean,
            ExtractionEngine::Notebook {
                kernel_language: language.to_string(),
            },
            diagnostics,
        );
    }

    // One buffer, so a symbol defined in cell 3 and called in cell 7 resolves
    // the way the notebook actually behaves when run top to bottom.
    let mut buffer = String::new();
    for cell in &cells {
        buffer.push_str(&cell.code);
        if !cell.code.ends_with('\n') {
            buffer.push('\n');
        }
    }

    let synthetic = format!("{path}.{}", synthetic_extension(language));
    let parsed = parse(&synthetic, language, &buffer);
    // Everything the parse produced is named for `synthetic`. That name must
    // not survive into the extraction in any field: the resolver joins symbols,
    // parents and callers by these strings, and one that names a file which does
    // not exist produces edges nothing can match.
    let synthetic_prefix = format!("{synthetic}::");
    let real_prefix = format!("{path}::");

    let mut relocated: Vec<ExtractedSymbol> = Vec::new();
    let mut unlocatable = 0usize;
    for mut symbol in parsed.symbols {
        // The reconstructed buffer is a file to the extractor, so it emits its
        // own `File` node named for the synthetic path. That node describes a
        // buffer that does not exist; the base already carries the real one for
        // the `.ipynb` itself, and letting this one through put two File nodes
        // in the graph for one file.
        if symbol.kind == crate::model::SymbolKind::File {
            continue;
        }
        // The declaration line as it appeared in the reconstructed buffer.
        let declaration = buffer
            .get(symbol.span.start_byte..symbol.span.end_byte)
            .and_then(|text| text.lines().next())
            .unwrap_or_default()
            .trim_end();

        let span = locate_line(raw, declaration).or_else(|| {
            // Coarse but true: the cell this symbol came from.
            cells
                .iter()
                // `contains("")` is unconditionally true, so an empty
                // declaration silently took cell 0's span and was reported as
                // located — a guessed span presented as a found one, which the
                // module doc explicitly forbids. The parallel call path already
                // guards this; the symbol path did not.
                .find(|cell| !declaration.is_empty() && cell.code.contains(declaration))
                .and_then(|cell| cell.raw_span.clone())
        });

        match span {
            Some(span) => {
                symbol.span = span;
                // Rewrite the *prefix*, do not rebuild the name from
                // `symbol.name`. A method's qualified name is
                // `<file>::Holder.method`, and rebuilding it flattened that to
                // `<file>::method` while the call attributed to it kept the
                // dotted form — so every method declared in a notebook was
                // recorded under a name nothing referred to.
                symbol.qualified_name = match symbol
                    .qualified_name
                    .strip_prefix(synthetic_prefix.as_str())
                {
                    Some(rest) => format!("{real_prefix}{rest}"),
                    // No synthetic prefix to strip: fall back to the plain
                    // file-qualified form rather than leaving a name that
                    // points at the parse buffer.
                    None => format!("{real_prefix}{}", symbol.name),
                };
                // The parent too. A top-level declaration's parent is the file
                // it was parsed from, and left as the synthetic name it no
                // longer equals `file_path` — so the resolver's "only emit a
                // second Contains when the parent is a real type" guard stopped
                // firing, and every symbol in a notebook got two containment
                // edges, the second from a file that does not exist.
                if let Some(parent) = symbol.parent_symbol.as_mut() {
                    if parent.as_str() == synthetic.as_str() {
                        *parent = path.to_string();
                    } else if let Some(rest) = parent.strip_prefix(synthetic_prefix.as_str()) {
                        *parent = format!("{real_prefix}{rest}");
                    }
                }
                relocated.push(symbol);
            }
            None => unlocatable += 1,
        }
    }
    if unlocatable > 0 {
        // Dropped, not guessed. A span that does not index this file renders as
        // whatever bytes happen to sit at the offset.
        diagnostics.push(format!(
            "{unlocatable} symbol(s) could not be located in the raw notebook and were dropped"
        ));
    }

    // A prefix read is part of the result, not a note about it.
    let outcome = match (&parsed.parse_outcome, dropped_cells, unlocatable) {
        (ParseOutcome::Failed { .. }, _, _) => parsed.parse_outcome,
        (_, 0, 0) => parsed.parse_outcome,
        (other, _, _) => {
            let mut losses = Vec::new();
            if dropped_cells > 0 {
                losses.push(format!(
                    "{MAX_CELLS} of {} cells were read and {dropped_cells} were not",
                    MAX_CELLS + dropped_cells
                ));
            }
            if unlocatable > 0 {
                losses.push(format!(
                    "{unlocatable} symbol(s) could not be located in the raw notebook \
                     and were dropped"
                ));
            }
            if matches!(other, ParseOutcome::Partial { .. }) {
                losses.push("the parsed cells also carried syntax errors".to_string());
            }
            ParseOutcome::Fallback {
                reason: format!(
                    "notebook symbol list is a prefix, not a set: {} — absence of a symbol \
                     is not evidence it is not declared",
                    losses.join("; ")
                ),
            }
        }
    };

    let mut extraction = base(
        outcome,
        ExtractionEngine::Notebook {
            kernel_language: language.to_string(),
        },
        diagnostics,
    );
    // The base carries the `File` node; the relocated declarations join it
    // rather than replacing it, so a notebook is addressable even when every
    // symbol in it proved unlocatable.
    extraction.symbols.extend(relocated);
    extraction.imports = parsed.imports;
    // Calls need the same relocation the symbols got, for the same reason and
    // in two places.
    //
    // `caller_symbol` is qualified with the *synthetic* path the parse ran
    // against, so a call from `summarize` arrived as
    // `analysis.ipynb.py::summarize` while its own node was recorded as
    // `analysis.ipynb::summarize`. The resolver joins those by name: the edge
    // pointed at a symbol no node row declared, and `devmap impact` answered
    // with a caller that does not exist. It also invented a second containing
    // file, so every symbol got two `Contains` edges.
    //
    // The span is relocated to the cell the call sits in — coarse, but a real
    // range in the real file, which is the rule the symbols already follow.
    // Nothing persists a call span today; leaving one that indexes a buffer
    // which no longer exists is a trap for whoever first does.
    // One relocation, applied to both edge kinds.
    //
    // `references` were not carried forward at all, which the capability matrix
    // could not see until the corpus gained an `.ipynb`: a notebook declaring
    // `class Widget(BaseWidget)` produced no `ReferenceKind::Heritage`, so W1.2's
    // `Extends`/`Implements` edges never fired for a notebook and every
    // `Capability::References` consumer saw an empty vector for a file whose
    // grammar had read it cleanly. That is the over-claim direction — a bit the
    // kernel declares and does not deliver — which is the one failure the
    // capability registry exists to make impossible.
    let relocate_span = |span: &mut Span| {
        let line = buffer
            .get(span.start_byte..span.end_byte)
            .and_then(|text| text.lines().next())
            .unwrap_or_default();
        if let Some(cell) = cells
            .iter()
            .find(|cell| !line.is_empty() && cell.code.contains(line))
        {
            if let Some(raw_span) = cell.raw_span.clone() {
                *span = raw_span;
            }
        }
    };
    let relocate_owner = |owner: &mut String| {
        if let Some(name) = owner.strip_prefix(synthetic_prefix.as_str()) {
            *owner = format!("{real_prefix}{name}");
        }
    };

    extraction.calls = parsed
        .calls
        .into_iter()
        .map(|mut call| {
            if let Some(caller) = call.caller_symbol.as_mut() {
                relocate_owner(caller);
            }
            relocate_span(&mut call.span);
            call
        })
        .collect();
    extraction.references = parsed
        .references
        .into_iter()
        .map(|mut reference| {
            if let Some(enclosing) = reference.enclosing_symbol.as_mut() {
                relocate_owner(enclosing);
            }
            relocate_span(&mut reference.span);
            reference
        })
        .collect();
    extraction
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;

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

    /// Cells share one buffer, so a call across cells resolves the way the
    /// notebook behaves when run top to bottom.
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

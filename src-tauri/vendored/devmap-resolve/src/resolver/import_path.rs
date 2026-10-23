use super::*;

impl Resolver {
    /// Every indexed file an absolute Python module path may name, in the order
    /// [`Self::resolve_import_path`] tries them: the manifest or package source
    /// roots, then the repository root, `src/`, and the importer's own
    /// directory. The first is the answer the import walk gives; a caller that
    /// must not guess between roots asks for all of them.
    pub(super) fn python_absolute_module_files(
        &self,
        current_file: &str,
        specifier: &str,
    ) -> Vec<String> {
        let dir = Self::parent_dir(current_file);
        let py_path = specifier.replace('.', "/");
        let mut files: Vec<String> = self.python_root_candidates(current_file, &py_path);
        files.extend([
            format!("{}.py", py_path),
            format!("{}/__init__.py", py_path),
            format!("src/{}.py", py_path),
            format!("src/{}/__init__.py", py_path),
            format!("{}/{}.py", dir, py_path),
            format!("{}/{}/__init__.py", dir, py_path),
        ]);
        let mut seen = BTreeSet::new();
        files.retain(|cand| self.file_symbols.contains_key(cand) && seen.insert(cand.clone()));
        files
    }

    pub(super) fn resolve_import_path(
        &self,
        current_file: &str,
        lang: &str,
        specifier: &str,
    ) -> Option<String> {
        let clean_spec = specifier.trim_matches(|c| c == '\'' || c == '"');
        let dir = Self::parent_dir(current_file);

        // The family, not a hand-listed set of grammar keys. The list this
        // replaces omitted `jsx` (which `from_lang` has always treated as
        // JS/TS) and every embedded-script host, so a `.svelte` file's
        // `import { helper } from "./helpers"` was classified as an *external*
        // import — the corpus was told a local file came from outside it.
        if LangFamily::from_lang(lang) == LangFamily::JsTs && clean_spec.starts_with('.') {
            let base = Self::normalize_rel(&dir, clean_spec)?;
            // NodeNext writes the extension the *emitted* file will have:
            // `'./arrows.js'` names `arrows.ts`. TypeScript tries the source
            // sibling first, so for a TS importer it goes before the literal
            // path; a JS importer names the file it wrote and only falls back.
            let source_sibling = |this: &Self| {
                Self::nodenext_source_siblings(&base)
                    .into_iter()
                    .find(|candidate| this.file_symbols.contains_key(candidate))
            };
            let from_ts = matches!(lang, "typescript" | "tsx");
            if from_ts {
                if let Some(found) = source_sibling(self) {
                    return Some(found);
                }
            }
            for cand in Self::js_module_candidates(&base) {
                if self.file_symbols.contains_key(&cand) {
                    return Some(cand);
                }
            }
            if !from_ts {
                if let Some(found) = source_sibling(self) {
                    return Some(found);
                }
            }
        }
        // A non-relative specifier names a file in this tree only through the
        // config that governs the importer: `paths`, then `baseUrl`.
        if LangFamily::from_lang(lang) == LangFamily::JsTs && !clean_spec.starts_with('.') {
            if let Some(mapped) = self.resolve_ts_mapped(current_file, clean_spec) {
                return Some(mapped);
            }
            if let Some(package) = self.resolve_workspace_package(clean_spec) {
                return Some(package);
            }
        }

        // Shell `source` / `.` names a **path**, not a module, and it is the one
        // import form whose base directory is the runtime working directory
        // rather than anything written down.
        //
        // There was no arm here at all, so every specifier
        // `langimports::shell` extracted resolved to nothing. `shell` still
        // declares `Capability::Imports`, which is what makes that silent: SQL
        // declines the capability and is charged `import_blind`, so a `.sql`
        // file nothing imports is excluded from the finding with a reason,
        // while a `.sh` file nothing *could* import was reported as unwired on
        // the strength of a lookup that never had an arm to run.
        //
        // Measured on this repository: `rust/verify.sh` and
        // `rust/tools/memory_model_probe.sh` both `. tools/peak_rss.sh`, and
        // the sourced file was an unwired candidate. Worse than the missing
        // edge, the `peak_rss_bytes` calls it defines fanned out through the
        // global tier onto **six archived copies** under
        // `benchmarks/results/competition/*/peak_rss.sh` — the stale artifacts
        // absorbed the edges the live file should have had.
        //
        // The specifier is tried against each ancestor directory of the
        // sourcing script, nearest first. A script that sources a relative path
        // has almost always `cd`-ed somewhere inside its own tree first
        // (`verify.sh` opens with `cd "$(dirname "$0")"`), and the ancestor walk
        // is the bounded way to say that without guessing at a cwd: it cannot
        // reach outside the repository, it cannot invent a file — the indexed
        // universe decides — and nearest-first makes the most specific match
        // win. An absolute or `$`-bearing specifier never arrives: the
        // extractor refuses those rather than emitting a path the author did
        // not write.
        if lang == "shell" {
            let mut probe = dir.clone();
            loop {
                if let Some(candidate) = Self::normalize_rel(&probe, clean_spec) {
                    if self.file_symbols.contains_key(&candidate) {
                        return Some(candidate);
                    }
                }
                match probe.rsplit_once('/') {
                    Some((parent, _)) if !parent.is_empty() => probe = parent.to_string(),
                    _ => break,
                }
            }
        }

        if lang == "python" {
            let dots = clean_spec.chars().take_while(|c| *c == '.').count();
            if dots > 0 {
                // A relative import names a module of the importer's own
                // package, climbing one package level per extra dot. The base
                // goes through `normalize_rel`, the single owner of `.`/`..`
                // handling: a climb past the repository root is `None`, and
                // the root directory is the empty base, not `"."`. Only the
                // two forms a module can take are probed, and a miss ends the
                // lookup: a missing sibling must not fall through into
                // `src/<pkg>/` or `<dir>/<dir>/`, which name other modules.
                let tail = clean_spec[dots..].replace('.', "/");
                let climb = "../".repeat(dots - 1);
                let base = Self::normalize_rel(&dir, &format!("{climb}{tail}"))?;
                // A bare `.`/`..` names the package itself, so only its
                // `__init__.py` can be the answer: a stray `pkg.py` beside
                // the `pkg/` directory is a different module.
                let candidates = if base.is_empty() {
                    vec!["__init__.py".to_string()]
                } else if tail.is_empty() {
                    vec![format!("{base}/__init__.py")]
                } else {
                    vec![format!("{base}.py"), format!("{base}/__init__.py")]
                };
                return candidates
                    .into_iter()
                    .find(|cand| self.file_symbols.contains_key(cand));
            }
            if let Some(found) = self
                .python_absolute_module_files(current_file, clean_spec)
                .into_iter()
                .next()
            {
                return Some(found);
            }
        }

        // X41. `use` now arrives here as a real module path, so the three
        // module roots it can name have to be answerable **bare** as well as
        // prefixed: `use super::*;` inside `mod tests { … }` is rewritten by
        // the extractor to the bare `self`, naming the file it is written in.
        if lang == "rust" {
            if clean_spec == "self" {
                return self
                    .file_symbols
                    .contains_key(current_file)
                    .then(|| current_file.to_string());
            }
            if clean_spec == "crate" {
                let mut roots = Vec::new();
                self.consider_crate_root_files(&mut roots, current_file);
                if let Some(first) = roots.into_iter().next() {
                    return Some(first);
                }
            }
            // The parent module *as a file*: `src/deep/leaf.rs` is `deep::leaf`,
            // so its `super` is `deep`, which lives in `src/deep/mod.rs` or
            // `src/deep.rs`.
            if clean_spec == "super" {
                for module_dir in self
                    .rust_module_dirs(current_file)
                    .iter()
                    .map(|module_dir| Self::parent_dir(module_dir))
                {
                    for candidate in [
                        format!("{module_dir}/mod.rs"),
                        format!("{module_dir}.rs"),
                        format!("{module_dir}/lib.rs"),
                        format!("{module_dir}/main.rs"),
                    ] {
                        if self.file_symbols.contains_key(&candidate) {
                            return Some(candidate);
                        }
                    }
                }
            }
        }

        if lang == "rust" && clean_spec.starts_with("crate::") {
            let crate_tail = clean_spec.strip_prefix("crate::")?;
            // The **crate's** source root, not the repository's. `crate::` is
            // relative to the crate the file belongs to, and a workspace puts
            // that at `crates/<name>/src`, so probing a literal `src/…` from
            // the tree root answered nothing for every workspace member. That
            // is why this repository produced no `Imports` edge at all for the
            // hundreds of `use crate::…` lines in its own kernel.
            let root = self.rust_crate_src_root(current_file);
            let mut parts: Vec<&str> = crate_tail.split("::").collect();
            self.trim_to_indexed_depth(&mut parts);
            while !parts.is_empty() {
                let rust_path = parts.join("/");
                let candidates = [
                    Self::rel_join(&root, &format!("{rust_path}.rs")),
                    Self::rel_join(&root, &format!("{rust_path}/mod.rs")),
                ];
                for cand in &candidates {
                    if self.file_symbols.contains_key(cand) {
                        return Some(cand.clone());
                    }
                }
                parts.pop();
            }
        }

        // Rust 2018 *uniform paths*: `use schema::stamp;` written beside
        // `pub mod schema;` names this file's own child module, with no `self::`
        // on it. That is how every crate root in this workspace is written —
        // `pub use model::*;`, `pub use resolver::Resolver;` — and there was no
        // arm for it. A bare specifier fell through to the language table,
        // which carries no Rust rule at all, so it resolved to nothing and the
        // crate surface was invisible to the re-export chain built above.
        //
        // Read as `self::`-relative and accepted only when the candidate is an
        // indexed file, so an external crate name binds to nothing rather than
        // to a coincidence: the corpus decides, not the guess. `use serde::…`
        // probes a sibling `serde.rs`, finds none, and stays unresolved exactly
        // as before.
        let rust_uniform_path = lang == "rust"
            && !clean_spec.is_empty()
            && !clean_spec.starts_with("crate::")
            && !matches!(clean_spec, "self" | "super" | "crate");
        if lang == "rust"
            && (clean_spec.starts_with("self::")
                || clean_spec.starts_with("super::")
                || rust_uniform_path)
        {
            let mut tail = clean_spec;
            let mut hops = 0usize;
            if let Some(stripped) = tail.strip_prefix("self::") {
                tail = stripped;
            } else {
                while let Some(stripped) = tail.strip_prefix("super::") {
                    hops += 1;
                    tail = stripped;
                }
            }
            let mut parts: Vec<&str> = tail.split("::").collect();
            self.trim_to_indexed_depth(&mut parts);
            // Both readings of "where does this module keep its children", best
            // first — see `rust_module_dirs`. Each `super::` walks one directory
            // up from whichever base is being tried.
            let bases: Vec<String> = self
                .rust_module_dirs(current_file)
                .into_iter()
                .map(|mut module_dir| {
                    for _ in 0..hops {
                        module_dir = Self::parent_dir(&module_dir);
                    }
                    module_dir
                })
                .collect();
            while !parts.is_empty() {
                let module_path = parts.join("/");
                for module_dir in &bases {
                    let Some(base) = Self::normalize_rel(module_dir, &module_path) else {
                        continue;
                    };
                    // `base` itself, before the two conventional forms: a
                    // `#[path = "generated/tables.rs"] mod tables;` reaches this
                    // rung as `self::generated/tables.rs`, and the extension is
                    // already on it. Appending `.rs` to a path that has one probes
                    // `tables.rs.rs` and finds nothing — which is precisely the
                    // case the attribute exists to declare, so failing it would
                    // leave the real file reported as imported by nothing.
                    for candidate in [base.clone(), format!("{base}.rs"), format!("{base}/mod.rs")]
                    {
                        if self.file_symbols.contains_key(&candidate) {
                            return Some(candidate);
                        }
                    }
                }
                parts.pop();
            }
        }

        // One Rust crate naming another. Last of the Rust rungs on purpose: a
        // path that resolves inside this crate is what the author wrote, and
        // only a specifier no local module answers can be a sibling crate.
        //
        // `use devmap_store::schema::CURRENT_SCHEMA_VERSION;` walks the tail
        // the same way `crate::` does, so a deep path lands on the module that
        // declares the name; a bare `use devmap_store::X;` lands on the root and
        // the re-export chain takes it the rest of the way.
        if lang == "rust" {
            let (head, tail) = clean_spec.split_once("::").unwrap_or((clean_spec, ""));
            if let Some(Some(root)) = self.rust_crate_roots.get(head) {
                let root = root.clone();
                let mut parts: Vec<&str> = if tail.is_empty() {
                    Vec::new()
                } else {
                    tail.split("::").collect()
                };
                self.trim_to_indexed_depth(&mut parts);
                while !parts.is_empty() {
                    let module_path = parts.join("/");
                    for candidate in [
                        Self::rel_join(&root, &format!("{module_path}.rs")),
                        Self::rel_join(&root, &format!("{module_path}/mod.rs")),
                    ] {
                        if self.file_symbols.contains_key(&candidate) {
                            return Some(candidate);
                        }
                    }
                    parts.pop();
                }
                for candidate in [
                    self.rust_library_file(&root),
                    Self::rel_join(&root, "main.rs"),
                ] {
                    if self.file_symbols.contains_key(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }

        // W0.3 move 2: the thirteen languages whose import extraction landed
        // with this rung. Table-driven rather than thirteen more blocks above,
        // because they differ only in separator, extensions and build roots —
        // see `importpath`, which owns the table and the candidate order.
        if let Some(rule) = crate::importpath::rule_for(lang) {
            let spec = crate::importpath::strip_dart_package_prefix(clean_spec)
                .filter(|_| lang == "dart")
                .unwrap_or(clean_spec);
            let mut candidates = crate::importpath::candidates(rule, &dir, spec);
            // A Gradle module's own source roots: `core/src/main/java/com/..`.
            // The list is the settings script's `include` statements, so a
            // module nothing includes is not probed. Ahead of all of them,
            // the importer's own source set and its module's `main`: a
            // product flavor sees itself and `main`, never a sibling flavor.
            if matches!(lang, "kotlin" | "java") {
                let own = crate::importpath::source_set_candidates(rule, current_file, spec);
                candidates.splice(0..0, own);
                candidates.extend(crate::importpath::module_candidates(
                    rule,
                    &self.gradle_module_dirs,
                    spec,
                ));
            }
            for candidate in &candidates {
                if self.file_symbols.contains_key(candidate) {
                    return Some(candidate.clone());
                }
            }
            // Last rung: a candidate's *basename* naming exactly one indexed
            // file. This is what makes `#include "util.h"` from a target built
            // with `-Isrc/core` resolve without the resolver knowing the build
            // system's include path, and `-include_lib("kernel/include/x.hrl")`
            // resolve without knowing where the application was unpacked.
            //
            // Guarded on uniqueness, not on plausibility. Two files named
            // `util.h` mean this rung abstains — which is the same answer the
            // ambiguity ladder gives elsewhere in this resolver, and the reason
            // it can be trusted at all: it never picks a winner.
            if !crate::importpath::is_relative_specifier(spec) {
                for candidate in &candidates {
                    let basename = candidate.rsplit('/').next().unwrap_or(candidate);
                    if let Some(Some(unique)) = self.unique_basename.get(basename) {
                        return Some(unique.clone());
                    }
                }
            }
        }

        None
    }
}

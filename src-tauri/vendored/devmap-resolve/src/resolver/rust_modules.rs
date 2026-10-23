use super::*;

impl Resolver {
    /// Delegates to `importpath::normalize_rel`, which is the single owner.
    ///
    /// The two implementations were identical when the table-driven ladder
    /// landed, and two identical copies of path normalisation is how a resolver
    /// starts answering two different questions about one `..`.
    pub(super) fn normalize_rel(base_dir: &str, spec: &str) -> Option<String> {
        crate::importpath::normalize_rel(base_dir, spec)
    }

    /// The directories a Rust module's children can live in, best first.
    ///
    /// `src/lib.rs` and `src/deep/mod.rs` keep their children in their own
    /// directory; `src/deep/leaf.rs` keeps them in `src/deep/leaf/`. The second
    /// is the rule the module system states and the one this resolver never
    /// applied — it used the file's directory for both, so `super::sibling`
    /// written in `src/deep/leaf.rs` probed `src/sibling.rs` when the statement
    /// names `src/deep/sibling.rs`.
    ///
    /// Both readings are returned rather than one chosen, because "is this file
    /// a crate root" is not decidable from its path: `lib.rs` and `main.rs` are,
    /// and so is every file directly under `tests/`, `benches/`, `examples/` and
    /// `src/bin/` — a list that goes stale against Cargo's auto-discovery and
    /// against a hand-written `[[test]] path = …`. The module-system reading is
    /// tried first and the indexed file universe decides; neither can invent a
    /// file that is not there, so the worst case is the answer this rung gave
    /// before.
    pub(super) fn rust_module_dirs(&self, file: &str) -> Vec<String> {
        let dir = Self::parent_dir(file);
        let stem = file
            .rsplit('/')
            .next()
            .unwrap_or(file)
            .strip_suffix(".rs")
            .unwrap_or_default();
        // A directory module's file: its children are its siblings, not its
        // descendants. A crate root a manifest names (`[lib] path =
        // "lib/core.rs"`) is one whatever its stem, and that is decidable.
        if stem.is_empty()
            || matches!(stem, "mod" | "lib" | "main")
            || self
                .rust_declared_roots
                .get(&dir)
                .is_some_and(|roots| roots.contains(file))
        {
            return vec![dir];
        }
        let nested = if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{dir}/{stem}")
        };
        if nested == dir {
            vec![dir]
        } else {
            vec![nested, dir]
        }
    }

    /// The source root of the crate `file` belongs to — what `crate::` is
    /// relative to.
    ///
    /// The longest ancestor path whose last component is `src`. Cargo requires
    /// a crate's root to be `src/lib.rs` or `src/main.rs` (or a path named in
    /// the manifest), so the innermost `src` above a file is its crate's root
    /// in every layout this resolver can be pointed at, single-crate and
    /// workspace alike.
    ///
    /// A manifest's own word beats the convention: the directory of a root
    /// file `[lib] path` / `[[bin]] path` names is the root of every file
    /// under it, unless a `src` nearer the file says otherwise (a deeper crate
    /// nested inside it).
    ///
    /// Falls back to the literal `src`, which is what this rung probed
    /// unconditionally before: a file with no `src` ancestor and no declared
    /// root above it — `build.rs`, a `tests/` integration crate, a bare
    /// script — keeps exactly the behaviour it had rather than gaining a guess.
    pub(super) fn rust_crate_src_root(&self, file: &str) -> String {
        let mut components: Vec<&str> = file.split('/').collect();
        components.pop();
        let mut by_convention = None;
        let mut declared = None;
        while !(by_convention.is_some() && declared.is_some()) {
            let dir = components.join("/");
            if declared.is_none() && self.rust_declared_roots.contains_key(&dir) {
                declared = Some(dir.clone());
            }
            let Some(last) = components.last() else {
                break;
            };
            if by_convention.is_none() && *last == "src" {
                by_convention = Some(dir);
            }
            components.pop();
        }
        match (by_convention, declared) {
            (Some(src), Some(declared)) if declared.len() > src.len() => declared,
            (Some(src), _) => src,
            (None, Some(declared)) => declared,
            (None, None) => "src".to_string(),
        }
    }

    /// `rest` under the repo-relative directory `dir`; the root is `""`.
    pub(super) fn rel_join(dir: &str, rest: &str) -> String {
        if dir.is_empty() {
            rest.to_string()
        } else {
            format!("{dir}/{rest}")
        }
    }

    /// The files a crate rooted at `src_root` may be entered through, library
    /// first: the `[lib] path` a manifest declares there, else `lib.rs`, then
    /// `main.rs`, then any binary root a manifest declares there.
    pub(super) fn rust_root_file_candidates(&self, src_root: &str) -> Vec<String> {
        let mut files = Vec::new();
        match self.rust_declared_libs.get(src_root) {
            Some(lib) => files.push(lib.clone()),
            None => files.push(Self::rel_join(src_root, "lib.rs")),
        }
        files.push(Self::rel_join(src_root, "main.rs"));
        for declared in self.rust_declared_roots.get(src_root).into_iter().flatten() {
            if !files.contains(declared) {
                files.push(declared.clone());
            }
        }
        files
    }

    /// The library root of the crate at `src_root`: what a bare crate name is.
    pub(super) fn rust_library_file(&self, src_root: &str) -> String {
        self.rust_declared_libs
            .get(src_root)
            .cloned()
            .unwrap_or_else(|| Self::rel_join(src_root, "lib.rs"))
    }

    /// The directory this file's own child modules live in.
    ///
    /// The first reading of [`Self::rust_module_dirs`]: a `mod.rs` / `lib.rs` /
    /// `main.rs` keeps children beside itself, and a leaf keeps them in its
    /// nested directory. The second reading is the parent directory, which the
    /// import rung still tries so an old `use` does not go missing. A *call*
    /// must not use it. `helper::run()` in `ledger/bindings.rs` is not the
    /// sibling `ledger/helper.rs`; that path is `super::helper`.
    pub(super) fn rust_call_base(&self, file: &str) -> String {
        self.rust_module_dirs(file)
            .into_iter()
            .next()
            .unwrap_or_else(|| Self::parent_dir(file))
    }

    pub(super) fn rust_path_within_index(&self, path: &str) -> bool {
        let segments = path.split("::").count();
        segments > 0 && segments <= self.max_indexed_path_depth.saturating_add(2)
    }

    pub(super) fn rust_child_shapes(&self, file: &str, name: &str) -> usize {
        if !Self::is_plain_ident(name) {
            return 0;
        }
        let Some(joined) = Self::normalize_rel(&self.rust_call_base(file), name) else {
            return 0;
        };
        let mut shapes = 0usize;
        if self.file_symbols.contains_key(&format!("{joined}.rs")) {
            shapes += 1;
        }
        if self.file_symbols.contains_key(&format!("{joined}/mod.rs")) {
            shapes += 1;
        }
        shapes
    }

    pub(super) fn rust_path_root(&self, file: &str, path: &str) -> RustPathRoot {
        let root = path.split("::").next().unwrap_or("");
        if matches!(root, "crate" | "self" | "super") {
            return RustPathRoot::Keyword;
        }
        let child = self.rust_child_shapes(file, root) > 0;
        match self.rust_crate_roots.get(root) {
            Some(None) => RustPathRoot::Ambiguous,
            Some(Some(_)) if child => RustPathRoot::Ambiguous,
            Some(Some(src)) => RustPathRoot::Crate(src.clone()),
            None if child => RustPathRoot::Child,
            None => RustPathRoot::Unknown,
        }
    }

    pub(super) fn consider_indexed(&self, found: &mut Vec<String>, candidate: String) {
        if self.file_symbols.contains_key(&candidate) {
            found.push(candidate);
        }
    }

    /// The root file(s) `crate` names from `file`. A crate root a manifest
    /// declares is its own `crate`, which matters where two binaries share a
    /// directory; anything else is every root its crate directory holds.
    pub(super) fn consider_crate_root_files(&self, found: &mut Vec<String>, file: &str) {
        let src_root = self.rust_crate_src_root(file);
        if self
            .rust_declared_roots
            .get(&src_root)
            .is_some_and(|roots| roots.contains(file))
        {
            self.consider_indexed(found, file.to_string());
            return;
        }
        for candidate in self.rust_root_file_candidates(&src_root) {
            self.consider_indexed(found, candidate);
        }
    }

    /// Every conventional file of the module whose directory is `dir`.
    ///
    /// More than one hit is ambiguity, not a preference: `work.rs` and
    /// `work/mod.rs` are both the module `work`, and picking either would
    /// invent the call's target.
    pub(super) fn consider_module_dir(&self, found: &mut Vec<String>, dir: &str) {
        let dir = dir.trim_end_matches('/');
        for candidate in [
            format!("{dir}/mod.rs"),
            format!("{dir}.rs"),
            format!("{dir}/lib.rs"),
            format!("{dir}/main.rs"),
        ] {
            self.consider_indexed(found, candidate);
        }
    }

    pub(super) fn consider_module_rel(&self, found: &mut Vec<String>, base: &str, tail: &str) {
        if tail.is_empty() || !tail.split("::").all(Self::is_plain_ident) {
            return;
        }
        let rel = tail.replace("::", "/");
        let Some(joined) = Self::normalize_rel(base, &rel) else {
            return;
        };
        self.consider_indexed(found, format!("{joined}.rs"));
        self.consider_indexed(found, format!("{joined}/mod.rs"));
    }

    /// The module file `tail` names below the one crate root in `roots`, when
    /// a segment is not a child module of the file before it but a module that
    /// file's glob re-export publishes: `pub use geo::*;` in `lib.rs` makes
    /// `crate::point` mean `geo`'s `mod point`. Each segment must name exactly
    /// one file, so a module two globs bring in is no answer. Empty on any
    /// doubt.
    pub(super) fn rust_module_through_globs(&self, roots: &[String], tail: &str) -> Vec<String> {
        let [root] = roots else {
            return Vec::new();
        };
        let mut current = root.clone();
        for segment in tail.split("::") {
            let mut next = Vec::new();
            self.consider_module_rel(&mut next, &self.rust_call_base(&current), segment);
            // A name the file itself declares shadows anything a glob brings.
            if next.is_empty() && self.symbol_kind_in(&current, segment).is_none() {
                for glob in self.rust_module_globs.get(&current).into_iter().flatten() {
                    self.consider_module_rel(&mut next, &self.rust_call_base(glob), segment);
                }
            }
            next.sort();
            next.dedup();
            let [only] = next.as_slice() else {
                return Vec::new();
            };
            current = only.clone();
        }
        vec![current]
    }

    /// Indexed files the whole path names, with no segment popped.
    ///
    /// `resolve_import_path` walks a `use` by dropping a trailing type name
    /// until a file appears. A call must not: `crate::resolver::Resolver`
    /// would land on `resolver.rs` and then look the callee up there, which
    /// is a different path from the one the author wrote.
    pub(super) fn rust_module_files(&self, file: &str, path: &str) -> Vec<String> {
        let mut found = Vec::new();
        if path == "self" {
            self.consider_indexed(&mut found, file.to_string());
            return found;
        }
        if path == "crate" {
            self.consider_crate_root_files(&mut found, file);
            return found;
        }
        if path == "super" {
            let parent = Self::parent_dir(&self.rust_call_base(file));
            self.consider_module_dir(&mut found, &parent);
            return found;
        }
        if let Some(tail) = path.strip_prefix("crate::") {
            self.consider_module_rel(&mut found, &self.rust_crate_src_root(file), tail);
            if found.is_empty() {
                let mut roots = Vec::new();
                self.consider_crate_root_files(&mut roots, file);
                found = self.rust_module_through_globs(&roots, tail);
            }
            return found;
        }
        if let Some(tail) = path.strip_prefix("self::") {
            self.consider_module_rel(&mut found, &self.rust_call_base(file), tail);
            return found;
        }
        if let Some(rest) = path.strip_prefix("super::") {
            let mut hops = 1usize;
            let mut tail = rest;
            while let Some(more) = tail.strip_prefix("super::") {
                hops += 1;
                tail = more;
            }
            let mut base = self.rust_call_base(file);
            for _ in 0..hops {
                base = Self::parent_dir(&base);
            }
            if tail.is_empty() {
                self.consider_module_dir(&mut found, &base);
            } else {
                self.consider_module_rel(&mut found, &base, tail);
            }
            return found;
        }
        match self.rust_path_root(file, path) {
            RustPathRoot::Crate(src_root) => {
                if let Some((_, tail)) = path.split_once("::") {
                    self.consider_module_rel(&mut found, &src_root, tail);
                    if found.is_empty() {
                        let mut roots = Vec::new();
                        self.consider_indexed(&mut roots, self.rust_library_file(&src_root));
                        found = self.rust_module_through_globs(&roots, tail);
                    }
                } else {
                    // A crate *name* is the library target. A package with
                    // both `lib.rs` and `main.rs` is not ambiguous to an
                    // importer — the binary is not addressable by name — so
                    // `main.rs` answers only when there is no library.
                    self.consider_indexed(&mut found, self.rust_library_file(&src_root));
                    if found.is_empty() {
                        self.consider_indexed(&mut found, Self::rel_join(&src_root, "main.rs"));
                    }
                }
            }
            RustPathRoot::Child => {
                self.consider_module_rel(&mut found, &self.rust_call_base(file), path);
            }
            RustPathRoot::Keyword | RustPathRoot::Ambiguous | RustPathRoot::Unknown => {}
        }
        found
    }

    pub(super) fn rust_exact_module(&self, file: &str, path: &str) -> RustModulePlace {
        if !self.rust_path_within_index(path) || !path.split("::").all(Self::is_plain_ident) {
            return RustModulePlace::Absent;
        }
        match self.rust_path_root(file, path) {
            RustPathRoot::Unknown => RustModulePlace::Absent,
            RustPathRoot::Ambiguous => RustModulePlace::Ambiguous,
            RustPathRoot::Keyword | RustPathRoot::Crate(_) | RustPathRoot::Child => {
                let mut hits = self.rust_module_files(file, path);
                hits.sort();
                hits.dedup();
                match hits.len() {
                    0 => RustModulePlace::Absent,
                    1 => RustModulePlace::Unique(hits.remove(0)),
                    _ => RustModulePlace::Ambiguous,
                }
            }
        }
    }

    pub(super) fn rust_type_kind(kind: SymbolKind) -> bool {
        matches!(
            kind,
            SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Class
                | SymbolKind::Trait
                | SymbolKind::Interface
        )
    }

    pub(super) fn rust_module_resolution(
        caller: &str,
        target_file: &str,
        target_symbol: &str,
        path: &str,
    ) -> Resolution {
        if caller == target_file {
            Resolution::SameFile {
                target_symbol: target_symbol.to_string(),
                target_file: target_file.to_string(),
            }
        } else {
            Resolution::ImportScoped {
                target_symbol: target_symbol.to_string(),
                target_file: target_file.to_string(),
                imported_from: path.to_string(),
            }
        }
    }

    /// A Rust call whose receiver is a module path: `crate::work::helper()`,
    /// `super::helper()`, `other_crate::Resolver::resolve_all()`.
    ///
    /// Two shapes, and the first one wins so they cannot both fire. The whole
    /// receiver is a module and the callee is a free function in that file, or
    /// the receiver's last segment is a type declared in the module the prefix
    /// names and the callee is a method of that type in that file. Either
    /// shape binds only when exactly one indexed file is the module. A second
    /// file, a second method, or a prefix that had to be popped is no target.
    ///
    /// A receiver with no `::` left — `dc_glob::matches()` reduces to `dc_glob`
    /// — is syntactically indistinguishable from a value, so it is a path only
    /// when the caller has established `bare_root_is_free` (no local, no import
    /// of that name) and the name is an indexed crate. A bare child module is
    /// not taken here: `mod work;` already binds `work` through its import.
    ///
    /// A crate root that states `pub use index::build_index;` declares nothing
    /// itself, so both shapes follow the root's re-export to the declaring
    /// file — the statement says where the name comes from.
    pub(super) fn rust_qualified_call(
        &self,
        file: &str,
        receiver: &str,
        callee: &str,
        bare_root_is_free: bool,
    ) -> Option<Resolution> {
        if !Self::is_plain_ident(callee) {
            return None;
        }
        // `fast_impl::gated()` beside `mod fast_impl { pub fn gated() }`: the
        // module is this file, and its items are filed under the file's own
        // namespace, so the path names a function this file declares. Only a
        // module the extractor saw declared inline qualifies — a crate or a
        // sibling file spelled the same way is not this file's function.
        if (bare_root_is_free || !Self::is_plain_ident(receiver))
            && self.names_inline_modules_only(file, receiver)
        {
            if self.symbol_kind_in(file, callee) != Some(SymbolKind::Function)
                || !self.declared_at_file_level(file, callee)
            {
                return None;
            }
            return Some(Resolution::SameFile {
                target_symbol: self.qualified_for(file, callee),
                target_file: file.to_string(),
            });
        }
        let bare_crate = bare_root_is_free
            && Self::is_plain_ident(receiver)
            && matches!(self.rust_path_root(file, receiver), RustPathRoot::Crate(_));
        if !Self::receiver_is_module_path(receiver)
            && !matches!(receiver, "crate" | "self" | "super")
            && !bare_crate
        {
            return None;
        }
        match self.rust_exact_module(file, receiver) {
            RustModulePlace::Ambiguous => return None,
            RustModulePlace::Unique(module_file) => {
                let (decl_file, decl_name) = self.declaring_site(&module_file, callee);
                let (target_file, target_symbol) =
                    self.lookup_in_package(&decl_file, &decl_name)?;
                if self.symbol_kind_in(&target_file, &decl_name) != Some(SymbolKind::Function) {
                    return None;
                }
                return Some(Self::rust_module_resolution(
                    file,
                    &target_file,
                    &target_symbol,
                    receiver,
                ));
            }
            RustModulePlace::Absent => {}
        }
        let (module_path, type_name) = receiver.rsplit_once("::")?;
        if !Self::is_plain_ident(type_name) {
            return None;
        }
        let module_file = match self.rust_exact_module(file, module_path) {
            RustModulePlace::Unique(module_file) => module_file,
            RustModulePlace::Ambiguous | RustModulePlace::Absent => return None,
        };
        let (type_file, type_name) = self.declaring_site(&module_file, type_name);
        if !self
            .symbol_kind_in(&type_file, &type_name)
            .is_some_and(Self::rust_type_kind)
        {
            return None;
        }
        let hits =
            self.type_methods
                .get(&(LangFamily::Rust, type_name.clone(), callee.to_string()))?;
        let mut matched: Vec<&(String, String)> =
            hits.iter().filter(|(path, _)| path == &type_file).collect();
        matched.sort();
        matched.dedup();
        if matched.len() != 1 {
            return None;
        }
        let (target_file, target_symbol) = matched[0];
        Some(Resolution::ReceiverType {
            target_symbol: target_symbol.clone(),
            target_file: target_file.clone(),
            receiver_type: type_name,
        })
    }

    /// Whether every segment of the Rust path `receiver` (a leading `self::`
    /// allowed) is an inline module `file` declares.
    pub(super) fn names_inline_modules_only(&self, file: &str, receiver: &str) -> bool {
        let Some(modules) = self.rust_inline_modules.get(file) else {
            return false;
        };
        let path = receiver.strip_prefix("self::").unwrap_or(receiver);
        !path.is_empty()
            && path
                .split("::")
                .all(|segment| Self::is_plain_ident(segment) && modules.contains(segment))
    }

    /// The file and name that actually declare `name` as `file` exposes it.
    ///
    /// `file` itself when it declares the name; otherwise the terminal of the
    /// re-export `file` states for it (`pub use index::build_index;`), which
    /// `compute_reexport_chains` has already followed through nested barrels
    /// and refused for a cycle. With neither, `file` and `name` unchanged, so a
    /// caller's own lookup fails exactly as it did before.
    pub(super) fn declaring_site(&self, file: &str, name: &str) -> (String, String) {
        if self.symbol_kind_in(file, name).is_none() {
            if let Some((terminal_file, terminal_name)) = self
                .reexport_chains
                .get(&format!("{file}::{name}"))
                .and_then(|terminal| terminal.rsplit_once("::"))
            {
                return (terminal_file.to_string(), terminal_name.to_string());
            }
        }
        (file.to_string(), name.to_string())
    }
}

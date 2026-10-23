use super::*;

impl Resolver {
    pub(super) fn go_import_edge_targets(&self, files: &[String]) -> Vec<String> {
        let mut nodes = BTreeSet::new();
        for file in files {
            match self.go_package_by_file.get(file) {
                Some(pkg) if pkg != "main" && !pkg.is_empty() => {
                    nodes.insert(format!("package:{}/{}", Self::parent_dir(file), pkg));
                }
                _ => {
                    nodes.insert(file.clone());
                }
            }
        }
        nodes.into_iter().collect()
    }

    /// A `.go` file that is part of the importable package.
    ///
    /// `_test.go` files are compiled only for the package's own test binary, so
    /// an importer never sees them. One owner for the predicate because
    /// `go_files_in_dir` and the package-suffix fallback below must agree on
    /// what a package contains — they answer the same import.
    pub(super) fn is_go_package_file(path: &str) -> bool {
        path.ends_with(".go") && !path.ends_with("_test.go")
    }

    /// Every directory holding at least one importable Go file, ascending.
    ///
    /// The keys of `files_by_dir` are parent directories already, so this is
    /// the same set the previous `parent_dir`-per-file scan collected, without
    /// the scan.
    pub(super) fn go_package_dirs(&self) -> impl Iterator<Item = String> + '_ {
        self.files_by_dir
            .iter()
            .filter(|(_, files)| files.iter().any(|path| Self::is_go_package_file(path)))
            .map(|(dir, _)| dir.clone())
    }

    pub(super) fn go_files_in_dir(&self, dir: &str) -> Vec<String> {
        let dir = dir.trim_end_matches('/');
        self.files_by_dir
            .get(dir)
            .map(|files| {
                files
                    .iter()
                    .filter(|path| Self::is_go_package_file(path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn apply_go_replace(&self, spec: &str) -> Option<(String, Option<String>)> {
        let mut best: Option<(&GoModule, &(String, String))> = None;
        for module in &self.go_modules {
            for replace in &module.replaces {
                if spec == replace.0 || spec.starts_with(&format!("{}/", replace.0)) {
                    let better = best.is_none_or(|(_, current)| replace.0.len() > current.0.len());
                    if better {
                        best = Some((module, replace));
                    }
                }
            }
        }
        let Some((module, (from, to))) = best else {
            return Some((spec.to_string(), None));
        };
        let suffix = spec[from.len()..].trim_start_matches('/');
        let joined = if to.starts_with('.') || to.starts_with('/') {
            let base = if module.dir.is_empty() {
                "."
            } else {
                module.dir.as_str()
            };
            let replaced = Self::normalize_rel(base, to)?;
            if suffix.is_empty() {
                replaced
            } else {
                format!("{replaced}/{suffix}")
            }
        } else if suffix.is_empty() {
            to.clone()
        } else {
            format!("{to}/{suffix}")
        };
        Some((spec.to_string(), Some(joined)))
    }

    pub(super) fn resolve_go_import(&self, specifier: &str) -> Vec<String> {
        let spec = specifier.trim_matches('"').trim();
        if spec.is_empty() || spec == "C" {
            return Vec::new();
        }
        let Some((original, replaced)) = self.apply_go_replace(spec) else {
            return Vec::new();
        };
        if let Some(replaced_dir) = replaced {
            let files = self.go_files_in_dir(&replaced_dir);
            if !files.is_empty() {
                return files;
            }
        }

        let mut best_module: Option<&GoModule> = None;
        for module in &self.go_modules {
            if original == module.prefix || original.starts_with(&format!("{}/", module.prefix)) {
                let better =
                    best_module.is_none_or(|current| module.prefix.len() > current.prefix.len());
                if better {
                    best_module = Some(module);
                }
            }
        }
        if let Some(module) = best_module {
            let rel = original[module.prefix.len()..].trim_start_matches('/');
            let target_dir = if rel.is_empty() {
                module.dir.clone()
            } else if module.dir.is_empty() {
                rel.to_string()
            } else {
                format!("{}/{}", module.dir, rel)
            };
            let files = self.go_files_in_dir(&target_dir);
            if !files.is_empty() {
                return files;
            }
        }

        let vendor = format!("vendor/{original}");
        let vendor_files = self.go_files_in_dir(&vendor);
        if !vendor_files.is_empty() {
            return vendor_files;
        }

        if !original.contains('/') {
            return Vec::new();
        }
        let mut matches: Vec<(usize, String)> = self
            .go_package_dirs()
            .filter(|dir| {
                let components = dir.split('/').filter(|part| !part.is_empty()).count();
                components >= 2
                    && (original == dir.as_str() || original.ends_with(&format!("/{dir}")))
            })
            .map(|dir| (dir.len(), dir))
            .collect();
        matches.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        let Some(&(best_len, _)) = matches.first() else {
            return Vec::new();
        };
        matches.retain(|(len, _)| *len == best_len);
        if matches.len() != 1 {
            return Vec::new();
        }
        self.go_files_in_dir(&matches[0].1)
    }

    pub(super) fn resolve_import_targets(
        &self,
        current_file: &str,
        lang: &str,
        specifier: &str,
    ) -> Vec<String> {
        if lang == "go" {
            return self.resolve_go_import(specifier);
        }
        // A Swift specifier names a **module**. Falling through to file-path
        // resolution would look for a file called `Foundation` and either miss
        // (leaving the import unexplained) or bind a coincidental
        // `Foundation.swift`. Empty here means the module is outside the
        // corpus, which `classify_unresolved` records as External.
        if lang == "swift" {
            return self.resolve_swift_import(specifier);
        }
        // A JVM wildcard import names a package, and a package is every file in
        // one directory. `import com.foo.*;` and `import foo.bar._` really do
        // depend on all of them, so all of them get an edge.
        //
        // Not capped, deliberately. An ambiguous bare-name *guess* — N
        // candidates of which at most one is right — is not emitted as edges at
        // all, because emitting N is how a resolver launders uncertainty into
        // volume. These N are all correct, and the consumer is `unwired_candidates`: truncating the
        // list would leave the files past the cut falsely reported as imported
        // by nothing, turning a bound into a false finding. The bound here is
        // the repository's own size, which already bounds everything else.
        if let Some(package) = specifier.strip_suffix(".*") {
            return self.resolve_package_wildcard(lang, package);
        }
        // A Terraform `module` source names a directory of `.tf` files, for the
        // same reason and with the same answer.
        if lang == "hcl" {
            return self.resolve_terraform_module(current_file, specifier);
        }
        let by_path = self.resolve_import_path(current_file, lang, specifier);
        if by_path.is_none() && matches!(lang, "kotlin" | "java") {
            return self.resolve_jvm_import(current_file, specifier);
        }
        by_path.into_iter().collect()
    }

    /// The file(s) a Kotlin or Java import names, read from the `package`
    /// headers the corpus declares rather than from a directory layout.
    ///
    /// `import a.b.C` and `import a.b.helper` both name `helper`/`C` in package
    /// `a.b`; a top-level function or an extension lives in whatever file the
    /// author chose, and a module's source root (`core/src/main/java`) is not a
    /// prefix any fixed root list can know. Splitting from the right tries
    /// `a.b.C.member` as member `member` of type `C` in `a.b` as well. The file
    /// must declare the name; a package alone is not an answer. Never the
    /// importing file itself.
    /// The member `type_name.member` a Kotlin or Java file reaches through a
    /// type the corpus declares: written fully qualified (`com.fx.app.Registry`,
    /// with Kotlin's `.INSTANCE`/`.Companion` accessor dropped), imported, or
    /// declared in the caller's own package.
    pub(super) fn jvm_qualified_member(
        &self,
        file: &str,
        receiver: &str,
        member: &str,
    ) -> Option<(String, String)> {
        let receiver = receiver
            .strip_suffix(".INSTANCE")
            .or_else(|| receiver.strip_suffix(".Companion"))
            .unwrap_or(receiver);
        let parts: Vec<&str> = receiver.split('.').collect();
        if parts
            .iter()
            .any(|part| part.is_empty() || !Self::is_plain_ident(part))
        {
            return None;
        }
        let (type_name, type_file) = if let [only] = parts.as_slice() {
            let imported = self
                .import_bindings
                .get(file)
                .and_then(|bindings| bindings.get(*only))
                .map(|(target, _)| target.clone())
                .filter(|target| target.ends_with(".java") || target.ends_with(".kt"));
            let same_package = || {
                let package = self.jvm_package_by_file.get(file)?;
                let files = self
                    .jvm_package_symbols
                    .get(&(package.clone(), (*only).to_string()))?;
                let mut others = files.iter().filter(|candidate| candidate.as_str() != file);
                let first = others.next()?;
                others.next().is_none().then(|| first.clone())
            };
            ((*only).to_string(), imported.or_else(same_package)?)
        } else {
            let (type_name, package_parts) = parts.split_last()?;
            let files = self
                .jvm_package_symbols
                .get(&(package_parts.join("."), (*type_name).to_string()))?;
            let mut others = files.iter().filter(|candidate| candidate.as_str() != file);
            let first = others.next()?;
            if others.next().is_some() {
                return None;
            }
            ((*type_name).to_string(), first.clone())
        };
        self.jvm_member_target(&type_file, &type_name, member)
            .map(|identity| (type_file, identity))
    }

    /// The one nested Kotlin type `receiver.name` names, when this file can see
    /// `receiver`: declared in it, imported by it, or in its package.
    pub(super) fn nested_type_target(
        &self,
        file: &str,
        receiver: &str,
        name: &str,
    ) -> Option<(String, String)> {
        let hits = self.symbol_index.get(name)?;
        let own = format!("{receiver}.{name}");
        let nested = format!(".{own}");
        let scoped = format!("::{own}");
        let root = Self::path_root(receiver);
        let imported = self
            .import_bindings
            .get(file)
            .and_then(|bindings| bindings.get(root))
            .map(|(target, _)| target.as_str());
        let package = self.jvm_package_by_file.get(file);
        let mut found: Vec<(&str, &str)> = hits
            .iter()
            .filter(|(path, kind, family, identity)| {
                *family == LangFamily::Kotlin
                    && matches!(
                        kind,
                        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum
                    )
                    && (identity.as_ref() == own
                        || identity.ends_with(&nested)
                        || identity.ends_with(&scoped))
                    && (path == file
                        || imported == Some(path.as_str())
                        || (package.is_some() && self.jvm_package_by_file.get(path) == package))
            })
            .map(|(path, _, _, identity)| (path.as_str(), identity.as_ref()))
            .collect();
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [(path, identity)] => Some(((*path).to_string(), (*identity).to_string())),
            _ => None,
        }
    }

    /// The identity of the one method `type_name.member` a file declares.
    pub(super) fn jvm_member_target(
        &self,
        file: &str,
        type_name: &str,
        member: &str,
    ) -> Option<String> {
        let hits = self.symbol_index.get(member)?;
        let own = format!("{type_name}.{member}");
        let nested = format!(".{own}");
        let scoped = format!("::{own}");
        let mut found: Vec<&str> = hits
            .iter()
            .filter(|(path, kind, _, identity)| {
                path == file
                    && matches!(kind, SymbolKind::Method | SymbolKind::Function)
                    && (identity.as_ref() == own
                        || identity.ends_with(&nested)
                        || identity.ends_with(&scoped))
            })
            .map(|(_, _, _, identity)| identity.as_ref())
            .collect();
        found.sort_unstable();
        found.dedup();
        (found.len() == 1).then(|| found[0].to_string())
    }

    pub(super) fn resolve_jvm_import(&self, current_file: &str, specifier: &str) -> Vec<String> {
        let parts: Vec<&str> = specifier.split('.').collect();
        for split in (1..parts.len()).rev() {
            let package = parts[..split].join(".");
            let name = parts[split];
            if let Some(files) = self.jvm_package_symbols.get(&(package, name.to_string())) {
                let others: Vec<String> = files
                    .iter()
                    .filter(|file| file.as_str() != current_file)
                    .cloned()
                    .collect();
                if !others.is_empty() {
                    return others;
                }
            }
        }
        Vec::new()
    }

    /// The one indexed file a Python module handle's load names: by path
    /// ([`Self::resolve_path_load`]), or — for `importlib.import_module` with a
    /// literal — by module name, exactly as `import <name>` here would resolve
    /// it, and only when that names one file.
    pub(super) fn resolve_loaded_module(
        &self,
        ext: &Extraction,
        imp: &ExtractedImport,
        load: &devmap_extract::model::PathLoad,
    ) -> Option<String> {
        if load.kind == PathLoadKind::ModuleName {
            let mut targets = self.resolve_import_targets_or_search_dirs(ext, imp);
            targets.sort();
            targets.dedup();
            return match targets.as_slice() {
                [only] => Some(only.clone()),
                _ => None,
            };
        }
        self.resolve_path_load(&ext.file_path, &imp.module_specifier, load.anchor_up)
    }

    /// The indexed file a Python path load names, or `None`.
    ///
    /// `specifier` is the literal path tail the extractor read
    /// (`scripts/build_supplement.py`); `anchor_up` says what it is relative
    /// to (see `devmap_extract::model::PathLoad`).
    ///
    /// * **Anchored** (`Path(__file__).parents[1] / "x.py"`): exactly one
    ///   candidate, the loader's directory climbed `anchor_up` levels. It is
    ///   indexed or the load abstains; nothing else is tried, because the
    ///   source said where the file is.
    /// * **Unanchored** (`ROOT / "x.py"` with `ROOT` opaque, or a bare
    ///   literal, which Python reads against the working directory): (a) the
    ///   loader's own directory and (b) the repository root, and if both name
    ///   different indexed files that is "one of several" and abstains; then
    ///   (c) the one indexed `.py` file whose path ends with the specifier.
    ///   Two such files abstain. A specifier that climbs (`..`) is only
    ///   meaningful against (a) and never suffix-matched.
    pub(super) fn resolve_path_load(
        &self,
        current_file: &str,
        specifier: &str,
        anchor_up: Option<u32>,
    ) -> Option<String> {
        let indexed = |path: &str| self.file_symbols.contains_key(path);
        let dir = Self::parent_dir(current_file);
        if let Some(up) = anchor_up {
            let mut base = dir;
            for _ in 0..up {
                if base.is_empty() {
                    // Above the repository root: not a file this corpus has.
                    return None;
                }
                base = base
                    .rsplit_once('/')
                    .map(|(parent, _)| parent.to_string())
                    .unwrap_or_default();
            }
            let candidate = Self::normalize_rel(&base, specifier)?;
            return indexed(&candidate).then_some(candidate);
        }
        let climbs = specifier.split('/').any(|part| part == "..");
        let mut hits: BTreeSet<String> = BTreeSet::new();
        for base in [dir.as_str(), ""] {
            if climbs && base.is_empty() {
                continue;
            }
            if let Some(candidate) = Self::normalize_rel(base, specifier) {
                if indexed(&candidate) {
                    hits.insert(candidate);
                }
            }
        }
        match hits.len() {
            0 => {}
            1 => return hits.pop_first(),
            _ => return None,
        }
        if climbs {
            return None;
        }
        let name = specifier.rsplit('/').next()?;
        let suffix = format!("/{specifier}");
        let mut matches = self
            .py_files_by_name
            .get(name)?
            .iter()
            .filter(|path| path.as_str() == specifier || path.ends_with(&suffix));
        let only = matches.next()?;
        matches.next().is_none().then(|| only.clone())
    }

    /// This file's `sys.path` as repo-relative directories: module-level
    /// entries with the offset of each insert, and the vetoes of inserts made
    /// inside functions. An entry that climbs above the repository root, or
    /// a veto with no readable directory, names a directory outside what the
    /// corpus can check and vetoes every `sys.path`-derived link.
    pub(super) fn search_dirs_of(&self, ext: &Extraction) -> PySearchPath {
        let mut out = PySearchPath::default();
        if ext.language != "python" {
            return out;
        }
        let dir = Self::parent_dir(&ext.file_path);
        let mut count = 0usize;
        for imp in &ext.imports {
            let Some(load) = imp
                .path_load
                .as_ref()
                .filter(|load| load.kind == PathLoadKind::SearchDirectory)
            else {
                continue;
            };
            count += 1;
            let directory = load.anchor_up.and_then(|up| {
                let mut base = Some(dir.clone());
                for _ in 0..up {
                    base = base.and_then(|base| {
                        (!base.is_empty()).then(|| {
                            base.rsplit_once('/')
                                .map(|(parent, _)| parent.to_string())
                                .unwrap_or_default()
                        })
                    });
                }
                Self::normalize_rel(&base?, &imp.module_specifier)
            });
            match (directory, load.scope.is_some()) {
                (Some(directory), false) => out.entries.push((imp.span.start_byte, directory)),
                (Some(directory), true) => out.vetoes.push(directory),
                (None, _) => out.veto_all = true,
            }
        }
        // The extractor emits at most 64 per file; a payload that carries
        // more — hand-built, or from elsewhere — is refused here too, so each
        // import's cost stays bounded whatever arrives.
        if count > 64 {
            return PySearchPath::default();
        }
        out
    }

    /// The one indexed file a Python module name reaches through this file's
    /// own `sys.path` entries: `<dir>/X.py` or `<dir>/X/__init__.py`, over the
    /// entries inserted **before** `at`. Asked only after ordinary resolution
    /// found nothing, so an import the ordinary rules answer keeps that answer.
    /// Two candidates — two entries, or a module and a package of one name in
    /// one entry — are one of several, and abstain. A relative import names
    /// its own package, never a `sys.path` entry.
    ///
    /// An insert inside a function may have run before this import, whatever
    /// their order in the file, so a directory it inserted that also holds
    /// the module vetoes the link unless it names the same file; and an
    /// insert whose directory the source does not fix vetoes every link.
    pub(super) fn resolve_via_search_dirs(
        &self,
        ext: &Extraction,
        at: usize,
        module: &str,
    ) -> Option<String> {
        let search = self.py_search_dirs.get(&ext.file_path)?;
        if search.veto_all {
            return None;
        }
        let module = module.trim_matches(|c| c == '\'' || c == '"');
        if module.is_empty() || module.starts_with('.') {
            return None;
        }
        let relative = module.replace('.', "/");
        let candidates = [format!("{relative}.py"), format!("{relative}/__init__.py")];
        let found = |directory: &str| {
            candidates
                .iter()
                .filter_map(|candidate| Self::normalize_rel(directory, candidate))
                .filter(|path| self.file_symbols.contains_key(path))
                .collect::<Vec<_>>()
        };
        let mut hits: BTreeSet<String> = BTreeSet::new();
        for (inserted, directory) in &search.entries {
            if *inserted < at {
                hits.extend(found(directory));
            }
        }
        if hits.len() != 1 {
            return None;
        }
        let target = hits.pop_first()?;
        let vetoed = search
            .vetoes
            .iter()
            .any(|directory| found(directory).iter().any(|path| *path != target));
        (!vetoed).then_some(target)
    }

    /// Whether a directory inserted into `sys.path` before `at` holds a file for
    /// `module` other than the ones ordinary resolution found (`found`).
    pub(super) fn sys_path_shadows(
        &self,
        ext: &Extraction,
        at: usize,
        module: &str,
        found: &[String],
    ) -> bool {
        let Some(search) = self.py_search_dirs.get(&ext.file_path) else {
            return false;
        };
        let module = module.trim_matches(|c| c == '\'' || c == '"');
        if module.is_empty() || module.starts_with('.') {
            return false;
        }
        let relative = module.replace('.', "/");
        [format!("{relative}.py"), format!("{relative}/__init__.py")]
            .iter()
            .any(|candidate| {
                search
                    .entries
                    .iter()
                    .filter(|(inserted, _)| *inserted < at)
                    .filter_map(|(_, directory)| Self::normalize_rel(directory, candidate))
                    .any(|path| self.file_symbols.contains_key(&path) && !found.contains(&path))
            })
    }

    /// Ordinary import resolution, then — for Python only, and only when that
    /// found nothing — this file's `sys.path` entries.
    pub(super) fn resolve_import_targets_or_search_dirs(
        &self,
        ext: &Extraction,
        imp: &ExtractedImport,
    ) -> Vec<String> {
        let targets =
            self.resolve_import_targets(&ext.file_path, &ext.language, &imp.module_specifier);
        if ext.language != "python" {
            return targets;
        }
        if !targets.is_empty() {
            // A directory this file put on `sys.path` before the import that
            // holds a *different* file for the module outranks the ordinary
            // rule at run time, so the ordinary answer may be the wrong
            // module. Abstain rather than bind it confidently.
            return if self.sys_path_shadows(
                ext,
                imp.span.start_byte,
                &imp.module_specifier,
                &targets,
            ) {
                Vec::new()
            } else {
                targets
            };
        }
        // `from .x import m` where `x/` is a namespace package (no
        // `__init__.py`): the module names no file, but each imported name
        // can be a submodule of it, `x/m.py`. Only a relative module is
        // probed, and only for names that are real files, so a name that is
        // an attribute of a missing module still yields no edge.
        if imp.module_specifier.starts_with('.') {
            let mut submodules: Vec<String> = imp
                .imported_names
                .iter()
                .filter(|name| !name.is_empty() && name.as_str() != "*")
                .filter_map(|name| {
                    self.resolve_import_path(
                        &ext.file_path,
                        &ext.language,
                        &Self::python_submodule_spec(&imp.module_specifier, name),
                    )
                })
                .collect();
            submodules.sort();
            submodules.dedup();
            if !submodules.is_empty() {
                return submodules;
            }
        }
        self.resolve_via_search_dirs(ext, imp.span.start_byte, &imp.module_specifier)
            .into_iter()
            .collect()
    }

    /// The dotted module `name` names inside the Python module `module`:
    /// `.x` + `m` is `.x.m`, while bare dots take the name directly, so `.` +
    /// `b` is `.b` (not `..b`, which would climb a package).
    pub(super) fn python_submodule_spec(module: &str, name: &str) -> String {
        if module.chars().all(|c| c == '.') {
            format!("{module}{name}")
        } else {
            format!("{module}.{name}")
        }
    }

    /// The file a path-loaded Python module handle names, read at a use site.
    ///
    /// The key is the use site's own binding scope: inside the function that
    /// bound the handle (or a closure over it) the extractor's
    /// `LocalBinding::scope` names that function; with no site binding the
    /// name is a module global. A binding of an anonymous callable has no
    /// scope identity to join on and abstains. `load().fn()` — a call through
    /// a same-file loader function — records the receiver identity `load`
    /// (the inner call's callee, arguments dropped), and the extractor keys
    /// the loader at module level under that same name.
    pub(super) fn path_module_for(
        &self,
        ext: &Extraction,
        site: usize,
        receiver: &str,
    ) -> Option<&String> {
        let table = self.path_module_bindings.get(&ext.file_path)?;
        let scope = match ext.local_binding_at(site, receiver) {
            Some(binding) => Some(binding.scope.clone()?),
            None => None,
        };
        table.get(&(scope, receiver.to_string()))
    }

    /// Every indexed file in the directory a JVM package name maps to.
    pub(super) fn resolve_package_wildcard(&self, lang: &str, package: &str) -> Vec<String> {
        let Some(rule) = crate::importpath::rule_for(lang) else {
            return Vec::new();
        };
        let relative = package.replace(rule.separator, "/");
        if relative.is_empty() {
            return Vec::new();
        }
        let mut directories: Vec<String> = Vec::new();
        for root in rule.roots {
            let Some(dir) = crate::importpath::normalize_rel(root, &relative) else {
                continue;
            };
            // The repository root is never a package. Without this an
            // unqualified wildcard would link its importer to every file in the
            // corpus — the one shape of this expansion that is not merely large
            // but wrong.
            if dir.is_empty() || directories.contains(&dir) {
                continue;
            }
            directories.push(dir);
        }
        for directory in directories {
            let files = self.files_in_dir_with_extensions(&directory, rule.extensions);
            if !files.is_empty() {
                return files;
            }
        }
        Vec::new()
    }

    /// Every indexed `.tf` file in the directory a Terraform `source` names.
    pub(super) fn resolve_terraform_module(
        &self,
        current_file: &str,
        specifier: &str,
    ) -> Vec<String> {
        // Only a local source is a path. `hashicorp/consul/aws` is a registry
        // address and `git::https://…` is a remote; both name something outside
        // the repository and must not be resolved against a same-named local
        // directory.
        if !(specifier.starts_with("./") || specifier.starts_with("../")) {
            return Vec::new();
        }
        let Some(directory) =
            crate::importpath::normalize_rel(&Self::parent_dir(current_file), specifier)
        else {
            return Vec::new();
        };
        if directory.is_empty() {
            return Vec::new();
        }
        self.files_in_dir_with_extensions(&directory, &[".tf"])
    }

    /// Indexed files sitting directly in `dir` whose name ends in one of
    /// `extensions`. Sorted, so an expansion is deterministic across runs.
    pub(super) fn files_in_dir_with_extensions(
        &self,
        dir: &str,
        extensions: &[&str],
    ) -> Vec<String> {
        let dir = dir.trim_end_matches('/');
        self.files_by_dir
            .get(dir)
            .map(|files| {
                files
                    .iter()
                    .filter(|path| {
                        extensions
                            .iter()
                            .any(|extension| !extension.is_empty() && path.ends_with(extension))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

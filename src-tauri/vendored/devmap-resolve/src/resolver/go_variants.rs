use super::*;

impl Resolver {
    /// The build-constraint variants of the Go package-level function `name` in
    /// package `package` of `dir`, as one [`Resolution::Dispatch`] naming every
    /// variant, or `None` when the name is not such a set.
    ///
    /// `proc.ConfigureGroup` is declared under `//go:build unix` and again under
    /// `//go:build !unix`. Exactly one is compiled into any build, and a call
    /// reaches whichever one was, so the call names the set. Without this the
    /// ladder saw two declarations, could not choose, and ledgered the call as
    /// `AmbiguousGlobal` or `uninferred_receiver` — about 74 of the 84 Go calls
    /// a `go/types` oracle found that DevMap missed.
    ///
    /// A set needs **two or more declaring files, and every one of them
    /// build-constrained**. Go forbids two declarations of one name in a
    /// package, so constrained twins can only coexist in a package that
    /// compiles because the constraints exclude one another; the constraint
    /// expressions themselves are not evaluated. A constrained file beside an
    /// unconstrained one, or two unconstrained twins, is a package that does
    /// not compile or an extraction fault, and is no evidence of variants. This
    /// is the predicate `go_build_variant_identities` in `devmap-analyze`'s
    /// liveness already uses, so a variant the resolver gives edges to is one
    /// liveness exempts.
    ///
    /// Keyed on `(directory, package clause)`, so it never reaches another
    /// package or the external `package x_test`. Functions only: a method is
    /// decided by its receiver's type, which a call of the bare name does not
    /// carry. `from_test` says the caller is a `_test.go` file, which sees the
    /// package's test declarations; any other caller does not.
    ///
    /// One edge per variant, emitted by the dispatch arm at the dispatch tier's
    /// confidence (0.4). The provenance is the edge detail, `via build variants
    /// of <name>`; no new resolution kind exists, so no stored reader changes.
    pub(super) fn go_build_variants(
        &self,
        dir: &str,
        package: Option<&String>,
        name: &str,
        from_test: bool,
    ) -> Option<Arc<Resolution>> {
        let package = package?;
        let hits =
            self.go_package_symbols
                .get(&(dir.to_string(), package.clone(), name.to_string()))?;
        let mut files: BTreeSet<&str> = BTreeSet::new();
        let mut candidates: Vec<(String, String)> = Vec::new();
        for (path, qualified, kind) in hits {
            if *kind != SymbolKind::Function || (!from_test && path.ends_with("_test.go")) {
                continue;
            }
            // Two declarations in one file is not two files declaring it.
            if !files.insert(path.as_str()) || !self.go_constrained_files.contains(path) {
                return None;
            }
            candidates.push((path.clone(), qualified.clone()));
        }
        if candidates.len() < 2 {
            return None;
        }
        candidates.sort();
        Some(Arc::new(Resolution::Dispatch {
            via_file: candidates[0].0.clone(),
            via_symbol: format!("build variants of {name}"),
            candidates: candidates.into(),
        }))
    }
}

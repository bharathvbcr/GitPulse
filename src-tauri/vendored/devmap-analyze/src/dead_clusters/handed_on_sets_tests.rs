//! `analyze_with_discovery` hands `dead_clusters_with` the exemption and
//! namesake sets the liveness pass already built, instead of letting
//! `dead_clusters` derive them a second time.
//!
//! A perf-only dedup: both sets are pure functions of `(extractions,
//! resolution)`, so no behaviour changes and there is no pre-fix failure to
//! show. What these pin is the invariant the hand-over depends on, so a later
//! edit that makes the two derivations diverge fails here instead of silently
//! shifting which components are reported.

use super::*;
use crate::liveness::{analyze_liveness_with_coverage, exempt_symbol_names, DiscoveryCoverage};
use devmap_extract::extract_file;
use devmap_resolve::Resolver;

fn resolve(files: &[(&str, &str)]) -> (Vec<Extraction>, ResolutionResult) {
    let extractions: Vec<Extraction> = files
        .iter()
        .map(|(path, source)| extract_file(path, source))
        .collect();
    let mut resolver = Resolver::new();
    resolver.index_extractions(&extractions);
    let resolution = resolver.resolve_all(&extractions).unwrap();
    (extractions, resolution)
}

/// Fixtures chosen so each input of the hand-over is non-empty somewhere:
/// a plain abandoned cycle, a cycle spared by `__all__`, a C header export, a
/// Go interface implementation, and a cycle whose name an untyped receiver
/// call (an unresolved-ledger row) also uses.
fn corpora() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            "abandoned cycle",
            vec![(
                "orphan.py",
                "def alpha():\n    return beta()\n\n\n\
                 def beta():\n    return gamma()\n\n\n\
                 def gamma():\n    return alpha()\n",
            )],
        ),
        (
            "exported Python members",
            vec![(
                "pkg/mod.py",
                "__all__ = [\"MyClass\"]\n\n\n\
                 class MyClass:\n\
                 \x20   def a(self):\n\
                 \x20       return self.b()\n\n\
                 \x20   def b(self):\n\
                 \x20       return self.a()\n",
            )],
        ),
        (
            "C header export",
            vec![
                ("lib.h", "int alpha(void);\nint beta(void);\n"),
                (
                    "lib.c",
                    "#include \"lib.h\"\nint alpha(void) { return beta(); }\n\
                     int beta(void) { return alpha(); }\n",
                ),
            ],
        ),
        (
            "Go interface implementation",
            vec![
                (
                    "svc/iface.go",
                    "package svc\n\ntype runner interface {\n\trun() error\n\tstop() error\n}\n",
                ),
                (
                    "svc/impl.go",
                    "package svc\n\ntype worker struct{}\n\n\
                     func (w *worker) run() error { return w.stop() }\n\n\
                     func (w *worker) stop() error { return w.run() }\n",
                ),
            ],
        ),
        (
            "cycle named by an unresolved receiver call",
            vec![
                (
                    "orphan.py",
                    "def alpha():\n    return beta()\n\n\n\
                     def beta():\n    return alpha()\n",
                ),
                ("user.py", "def use(thing):\n    return thing.alpha()\n"),
            ],
        ),
    ]
}

fn owned(names: std::collections::HashSet<&str>) -> BTreeSet<String> {
    names.into_iter().map(str::to_string).collect()
}

#[test]
fn the_outcome_carries_exactly_the_sets_the_helpers_derive() {
    let mut saw_exempt = false;
    let mut saw_namesake = false;
    for (kind, files) in corpora() {
        let (extractions, resolution) = resolve(&files);
        let outcome =
            analyze_liveness_with_coverage(&extractions, &resolution, DiscoveryCoverage::none());
        let exempt = exempt_symbol_names(&extractions, &resolution);
        let namesakes = owned(crate::liveness::unresolved_namesake_names(&resolution));
        assert_eq!(outcome.exempt_names, exempt, "{kind}: exemption set");
        assert_eq!(outcome.unresolved_names, namesakes, "{kind}: namesake set");
        saw_exempt |= !exempt.is_empty();
        saw_namesake |= !namesakes.is_empty();
    }
    assert!(
        saw_exempt,
        "no fixture produced an exemption: the test is vacuous"
    );
    assert!(
        saw_namesake,
        "no fixture produced an unresolved namesake: the test is vacuous"
    );
}

#[test]
fn handing_the_sets_over_reports_the_same_clusters_as_deriving_them() {
    let mut saw_cluster = false;
    for (kind, files) in corpora() {
        let (extractions, resolution) = resolve(&files);
        let outcome =
            analyze_liveness_with_coverage(&extractions, &resolution, DiscoveryCoverage::none());
        let derived = dead_clusters(&extractions, &resolution);
        let handed = dead_clusters_with(
            &extractions,
            &resolution,
            &outcome.exempt_names,
            &outcome.unresolved_names,
        );
        assert_eq!(derived, handed, "{kind}");
        saw_cluster |= !derived.clusters.is_empty();
    }
    assert!(
        saw_cluster,
        "no fixture produced a cluster: the comparison is vacuous"
    );
}

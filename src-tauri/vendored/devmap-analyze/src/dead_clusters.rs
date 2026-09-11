//! Abandoned subsystems that keep themselves alive.
//!
//! Liveness is a one-hop inbound-edge join and never transitive. A subsystem
//! whose functions call each other therefore has an inbound edge on **every**
//! symbol, and the kernel reports zero of it — the classic dead-code case, and
//! the single largest recall hole in the analysis.
//!
//! This is the traversal that closes it: strongly connected components over the
//! resolved call graph, then the components nothing outside them reaches.
//!
//! Three properties the implementation is built around:
//!
//! * **One finding per cluster, not one per member.** A 40-symbol dead cluster
//!   reported 40 times blows past `DEAD_CANDIDATE_CAP` and pushes real
//!   single-symbol findings out of the ranked list. The cluster *is* the
//!   finding.
//! * **Iterative Tarjan.** The natural formulation is recursive, and a 10,000
//!   node cycle is expressible in real code — a generated state machine, a
//!   mutually recursive parser. Recursion there is a stack overflow, which is a
//!   crash rather than a wrong answer, and no `catch_unwind` exists on this
//!   path because the CLI builds with `panic = "abort"`.
//! * **Deterministic edges only.** An ambiguous edge is evidence that a symbol
//!   *may* be called; letting one keep a cluster alive would silently suppress
//!   findings on exactly the speculative ground the confidence ladder exists to
//!   keep out of verdicts.

use devmap_extract::model::{EdgeKind, Extraction};
use devmap_resolve::model::{Resolution, ResolutionResult, ResolvedEdge};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// Most clusters reported. Shares the reasoning behind `DEAD_CANDIDATE_CAP`:
/// this is a debt list a human reads, and an unbounded one is not read at all.
pub const DEAD_CLUSTER_CAP: usize = 50;

/// Most members named per cluster.
///
/// The count is always exact — `size` is the real membership — and the *list*
/// is a sample. Carrying both is the difference between a capped sample and a
/// capped sample presented as complete.
pub const DEAD_CLUSTER_MEMBER_CAP: usize = 25;

/// Confidence ceiling for a cluster verdict.
///
/// Deliberately inside the `inferred` band (0.4–0.89) and never `extracted`. A
/// cluster verdict rests on the *whole graph* being complete in a way a
/// single-symbol verdict does not: one missed call edge anywhere into the
/// component makes the entire finding wrong, where the same missed edge costs a
/// single-symbol finding only itself. That is a strictly weaker claim and it
/// gets a strictly lower tier.
pub const DEAD_CLUSTER_CONFIDENCE: f32 = 0.5;

/// Confidence for a cluster something reaches through evidence the resolver
/// could not bind.
///
/// The single-symbol cascade already prices exactly this evidence: an ambiguous
/// caller is `only_ambiguous_callers` at 0.4, and an unresolved site naming the
/// symbol is [`crate::UNRESOLVED_NAMESAKE_REASON`] at 0.4. The component pass
/// excluded both from `is_reaching_edge` — correctly, because a guess must not
/// keep a cluster alive — and then never looked at them again, so the identical
/// evidence produced 0.4 for one symbol and 0.5 for a component containing it.
///
/// Measured on the fixture in `clusters_read_the_defect_ledger.rs`: one call
/// site, one ambiguity, two candidates. `Widget.alpha` came back at 0.4 saying
/// "only ambiguous callers"; `Task.alpha` came back inside a cluster at 0.5
/// saying it was "reached by nothing outside the component", which the edge
/// list contradicts. The stronger claim carried the higher confidence and the
/// more absolute prose.
///
/// Equal to the single-symbol tier rather than below it: the evidence is the
/// same, and the component's extra weakness is already priced by
/// `ExtractionCoverage::cap_cluster`, which compounds over the membership.
pub const DEAD_CLUSTER_QUALIFIED_CONFIDENCE: f32 = 0.4;

/// A group of symbols that reference only each other, reachable from nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadClusterReport {
    pub cluster_id: u32,
    /// Graph ids of the members, sorted, capped at [`DEAD_CLUSTER_MEMBER_CAP`].
    pub members: Vec<String>,
    /// The real membership count, never the length of `members`.
    pub size: usize,
    pub confidence: f32,
    pub reason: String,
}

/// The scan, with what it had to leave out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeadClusterScan {
    pub clusters: Vec<DeadClusterReport>,
    /// Files whose every declared symbol sits in a dead cluster.
    ///
    /// This is what makes `unreachable_files` answerable. The key shipped
    /// hardcoded `[]` beside an unconditional
    /// `liveness_unreachable_unreliable: true`, so four Python consumers
    /// suppressed it permanently and it was a third state — neither a computed
    /// empty nor an honest absence.
    ///
    /// **Not a BFS from entry roots.** That is the computation the `unreliable`
    /// flag was warning about, and it is genuinely noisy for routers, dynamic
    /// imports and JSX. This is a different and much narrower claim: every
    /// symbol this file declares belongs to a component that nothing outside
    /// reaches, where "reaches" already excludes ambiguous edges and already
    /// exempts anything exported or wired. A file with one live symbol does not
    /// qualify.
    pub unreachable_files: Vec<String>,
    /// Clusters found but not reported, because of [`DEAD_CLUSTER_CAP`].
    pub truncated_clusters: usize,
    /// Whether the graph was too large to walk at all. See
    /// [`DEAD_CLUSTER_MAX_NODES`].
    pub refused_oversized_graph: bool,
}

impl DeadClusterScan {
    /// The clusters to publish, or `None` when the pass refused to walk.
    ///
    /// Every surface that carries this scan needs the same conditional, and
    /// three of them wrote it — or rather, three of them wrote `clusters`
    /// straight through and rendered "the graph was too large to walk" as *the
    /// pass ran and found no abandoned subsystems*: the query response, the
    /// `code_graph.json` artifact, and the consumer manifest. A refusal leaves
    /// `clusters` empty, so a field-for-field copy is indistinguishable from a
    /// clean corpus at every one of them.
    ///
    /// Answered here rather than at each surface, with [`Self::incomplete_reason`]
    /// as its other half, so a fourth consumer inherits the distinction instead
    /// of having to know about it.
    pub fn reported_clusters(&self) -> Option<&[DeadClusterReport]> {
        if self.refused_oversized_graph {
            None
        } else {
            Some(&self.clusters)
        }
    }

    /// Why [`Self::reported_clusters`] is `None`, when it is `None` because the
    /// pass ran and refused.
    ///
    /// A reader told only that no scan is recorded will rebuild, and the
    /// rebuild walks the same graph and refuses again — so the ceiling is named
    /// rather than merely alluded to, which also lets a reader judge how far
    /// past it their repository is.
    pub fn incomplete_reason(&self) -> Option<String> {
        self.refused_oversized_graph.then(|| {
            format!(
                "the call graph exceeded {DEAD_CLUSTER_MAX_NODES} distinct symbols, \
                 so no component scan ran for this generation"
            )
        })
    }
}

/// The most distinct symbols the scan will hold before refusing.
///
/// Tarjan is linear, so this is not about asymptotics — it is about the memory
/// three index vectors over every node cost on a graph this kernel has measured
/// at 944,000 edges. Refusing loudly beats a walk that succeeds by consuming
/// the machine, and `refused_oversized_graph` says which happened rather than
/// leaving an empty result to read as "no dead clusters".
///
/// **Exactly this many, inclusive.** The guard is `names.len() >= MAX` *before*
/// a push, so a graph of exactly 400,000 distinct symbols is walked and the
/// 400,001st refuses the whole scan. The old wording — "beyond this many" —
/// described the same behaviour ambiguously enough that a reader could take the
/// boundary either way, and a bound whose edge is a matter of interpretation is
/// how an off-by-one becomes a load-bearing accident.
pub const DEAD_CLUSTER_MAX_NODES: usize = 400_000;

/// Whether this edge relates two *symbols* at all.
///
/// Structural edges say where a symbol lives, not that anything uses it —
/// `Contains` alone would put every symbol in a file into one component with
/// the file.
///
/// `Imports` is excluded for a different and sharper reason: an import edge
/// carries the **file path** in both symbol positions, so `a.py -> b.py ->
/// a.py` — an ordinary circular import, and TypeScript barrels and Python
/// packages are full of them — is a two-node strongly connected component in
/// the very graph this pass walks. Nothing came of it only because all three
/// `File`-symbol construction sites emit `is_exported: true`, which puts every
/// file into `externally_reachable_symbols`. That is a load-bearing dependency
/// on an unstated property of a different crate: the day a `File` node stops
/// reading as exported — a reasonable change, since a file node is not public
/// API — every circular import in every repository becomes a dead cluster.
///
/// Dropping them costs nothing that could ever have been a finding. Import
/// edges connect file nodes only, liveness never reports a `SymbolKind::File`,
/// and "a cluster of files" is not the claim this pass makes. It also shrinks
/// the graph Tarjan walks by one node per file plus every import edge.
fn is_symbol_edge(edge: &ResolvedEdge) -> bool {
    !matches!(
        edge.edge_kind,
        EdgeKind::Contains | EdgeKind::Defines | EdgeKind::MemberOf | EdgeKind::Imports
    )
}

/// Whether this edge is evidence that its target is reached.
///
/// Ambiguous and unresolved edges are excluded because a cluster kept alive by
/// a guess is a finding silently suppressed. They are not *forgotten*, though —
/// see [`qualifying_symbols`], which is where the same evidence that demotes a
/// single symbol reaches the component verdict.
fn is_reaching_edge(edge: &ResolvedEdge) -> bool {
    if !is_symbol_edge(edge) {
        return false;
    }
    !matches!(
        edge.resolution.as_deref(),
        Some(Resolution::AmbiguousGlobal { .. }) | Some(Resolution::Unresolved { .. })
    )
}

/// Symbols something reaches through evidence the resolver could not bind.
///
/// Two sources, and they are exactly the two the single-symbol cascade already
/// reads:
///
/// * an **ambiguous or unresolved edge** naming the symbol — the fan-out
///   `is_reaching_edge` refuses to count as reaching, which is right, but which
///   is still evidence that *something* meant this name;
/// * the **unresolved ledger**, filtered by [`crate::liveness::
///   unresolved_namesake_names`] so the two passes cannot drift about which
///   classes admit a veto.
///
/// Both the qualified name and the short name are recorded, because a cluster
/// member is identified by its full `file::Type.method` id while an unresolved
/// row carries only the callee spelling.
fn qualifying_symbols<'a>(resolution: &'a ResolutionResult) -> BTreeSet<&'a str> {
    let mut named: BTreeSet<&'a str> = BTreeSet::new();
    for edge in &resolution.edges {
        if !is_symbol_edge(edge) {
            continue;
        }
        if matches!(
            edge.resolution.as_deref(),
            Some(Resolution::AmbiguousGlobal { .. }) | Some(Resolution::Unresolved { .. })
        ) {
            named.insert(edge.target_symbol.as_str());
        }
    }
    named.extend(crate::liveness::unresolved_namesake_names(resolution));
    named
}

/// Whether any member of this component is named by unbindable evidence.
///
/// A member id is `file.py::Type.method`; the ledger's half of
/// [`qualifying_symbols`] holds bare callee names, so both spellings are tried.
fn component_is_qualified(members: &[String], qualifying: &BTreeSet<&str>) -> bool {
    members.iter().any(|member| {
        if qualifying.contains(member.as_str()) {
            return true;
        }
        let short = member.rsplit("::").next().unwrap_or(member);
        if qualifying.contains(short) {
            return true;
        }
        // `Type.method` also answers to `method`, which is what an unresolved
        // receiver-qualified call site records.
        short
            .rsplit_once('.')
            .is_some_and(|(_, bare)| qualifying.contains(bare))
    })
}

/// Strongly connected components, iteratively.
///
/// Tarjan's algorithm with the recursion made explicit. The `call_stack` holds
/// `(node, next child index)` so a resumed frame continues where it left off,
/// which is what the recursive form gets from the language.
///
/// Returns components in reverse topological order, as Tarjan does; callers
/// here do not depend on that, but changing it silently would be a trap.
pub(crate) fn strongly_connected_components(adjacency: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let n = adjacency.len();
    const UNVISITED: u32 = u32::MAX;

    let mut index = vec![UNVISITED; n];
    let mut lowlink = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut components: Vec<Vec<u32>> = Vec::new();
    let mut next_index: u32 = 0;

    for root in 0..n {
        if index[root] != UNVISITED {
            continue;
        }
        // `(node, position in that node's adjacency list)`.
        let mut call_stack: Vec<(u32, usize)> = vec![(root as u32, 0)];
        index[root] = next_index;
        lowlink[root] = next_index;
        next_index += 1;
        stack.push(root as u32);
        on_stack[root] = true;

        while let Some(&mut (node, ref mut child_position)) = call_stack.last_mut() {
            let neighbours = &adjacency[node as usize];
            if *child_position < neighbours.len() {
                let child = neighbours[*child_position];
                *child_position += 1;
                if index[child as usize] == UNVISITED {
                    index[child as usize] = next_index;
                    lowlink[child as usize] = next_index;
                    next_index += 1;
                    stack.push(child);
                    on_stack[child as usize] = true;
                    call_stack.push((child, 0));
                } else if on_stack[child as usize] {
                    lowlink[node as usize] = lowlink[node as usize].min(index[child as usize]);
                }
                continue;
            }

            // This node's children are exhausted: close it out.
            call_stack.pop();
            if let Some(&(parent, _)) = call_stack.last() {
                lowlink[parent as usize] = lowlink[parent as usize].min(lowlink[node as usize]);
            }
            if lowlink[node as usize] == index[node as usize] {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member as usize] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                components.push(component);
            }
        }
    }
    components
}

/// Symbols something outside the call graph is known to reach.
///
/// Seeded from the machinery that already answers this for single symbols, so a
/// cluster containing an exported symbol, a route handler or a framework entry
/// point is live for the same reason and by the same rule. Without it, a
/// perfectly ordinary set of mutually recursive exported functions is a
/// "cluster nothing reaches".
///
/// **It was only seeded from two of them.** `is_exported` and wiring
/// annotations were here; every exemption the single-symbol cascade computes —
/// a C-family header export, a Go interface implementation, a heritage
/// override, a member of an exported type, a Go build variant — was not,
/// because the cascade computed them one function later and kept them local.
///
/// The failure is not a tier disagreement. `__all__ += ["MyClass"]` with
/// `MyClass.a()` and `MyClass.b()` calling each other is exempted twice by the
/// single-symbol pass as declared public API, and was reported here at
/// `DEAD_CLUSTER_CONFIDENCE` with the reason "reached by nothing outside the
/// component" — a live proposal to delete public API. Two mutually recursive C
/// functions declared in a shared header are the same shape, and neither had a
/// test: `an_exported_member_keeps_the_cluster_alive` exercises `is_exported`
/// and stops there.
///
/// `liveness::exempt_symbol_names` is now the one owner and both passes read it.
fn externally_reachable_symbols(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
) -> BTreeSet<String> {
    let mut reachable = crate::liveness::exempt_symbol_names(extractions, resolution);
    for ext in extractions {
        for symbol in &ext.symbols {
            if symbol.is_exported {
                reachable.insert(symbol.qualified_name.clone());
            }
        }
        // Every wiring annotation is evidence that something outside the
        // resolvable call graph reaches its target — that is the type's whole
        // purpose. A file-scoped annotation names the file, which no symbol id
        // equals, so it is expanded to the file's symbols here.
        for annotation in &ext.wiring {
            if annotation.target_symbol == ext.file_path {
                for symbol in &ext.symbols {
                    reachable.insert(symbol.qualified_name.clone());
                }
            } else {
                reachable.insert(annotation.target_symbol.clone());
            }
        }
    }
    reachable
}

/// Find components of the resolved call graph that nothing outside reaches.
pub fn dead_clusters(extractions: &[Extraction], resolution: &ResolutionResult) -> DeadClusterScan {
    // Node ids, assigned in first-seen order over a deterministically ordered
    // edge list. `resolution.edges` is sorted by the resolver (R4), so the ids
    // — and therefore the cluster ids below — are stable across runs.
    let mut id_of: HashMap<&str, u32> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    let mut reaching: Vec<(u32, u32)> = Vec::new();

    for edge in &resolution.edges {
        if !is_reaching_edge(edge) {
            continue;
        }
        let source: &str = &edge.source_symbol;
        let target: &str = &edge.target_symbol;
        let source_id = match id_of.get(source) {
            Some(existing) => *existing,
            None => {
                if names.len() >= DEAD_CLUSTER_MAX_NODES {
                    return DeadClusterScan {
                        refused_oversized_graph: true,
                        ..Default::default()
                    };
                }
                let id = names.len() as u32;
                names.push(source);
                id_of.insert(source, id);
                id
            }
        };
        let target_id = match id_of.get(target) {
            Some(existing) => *existing,
            None => {
                if names.len() >= DEAD_CLUSTER_MAX_NODES {
                    return DeadClusterScan {
                        refused_oversized_graph: true,
                        ..Default::default()
                    };
                }
                let id = names.len() as u32;
                names.push(target);
                id_of.insert(target, id);
                id
            }
        };
        reaching.push((source_id, target_id));
    }

    if names.is_empty() {
        return DeadClusterScan::default();
    }

    let mut adjacency: Vec<Vec<u32>> = vec![Vec::new(); names.len()];
    for (source, target) in &reaching {
        adjacency[*source as usize].push(*target);
    }

    let components = strongly_connected_components(&adjacency);

    // Which component each node landed in, so an edge can be classified as
    // internal or incoming in constant time.
    let mut component_of: Vec<u32> = vec![u32::MAX; names.len()];
    for (component_id, members) in components.iter().enumerate() {
        for member in members {
            component_of[*member as usize] = component_id as u32;
        }
    }

    // A component is reached from outside if any edge crosses into it.
    let mut has_external_inbound = vec![false; components.len()];
    for (source, target) in &reaching {
        let from = component_of[*source as usize];
        let into = component_of[*target as usize];
        if from != into {
            has_external_inbound[into as usize] = true;
        }
    }

    let externally_reachable = externally_reachable_symbols(extractions, resolution);
    let qualifying = qualifying_symbols(resolution);

    let mut clustered_symbols: BTreeSet<String> = BTreeSet::new();
    let mut found: Vec<DeadClusterReport> = Vec::new();
    for (component_id, members) in components.iter().enumerate() {
        // A single node is a cluster only if it calls itself; otherwise it is
        // an ordinary symbol, and the single-symbol pass already owns it. This
        // pass exists for the case that pass structurally cannot see.
        let is_cycle = members.len() > 1
            || members
                .first()
                .is_some_and(|node| adjacency[*node as usize].contains(node));
        if !is_cycle || has_external_inbound[component_id] {
            continue;
        }

        let mut member_names: Vec<String> = members
            .iter()
            .map(|id| names[*id as usize].to_string())
            .collect();
        member_names.sort();

        // One externally reachable member makes the whole component live: an
        // exported symbol can be called from outside the corpus, and once it is
        // reached, everything it recurses with is reached too.
        if member_names
            .iter()
            .any(|name| externally_reachable.contains(name))
        {
            continue;
        }

        clustered_symbols.extend(member_names.iter().cloned());
        let size = member_names.len();
        let shown = size.min(DEAD_CLUSTER_MEMBER_CAP);
        // The evidence `is_reaching_edge` refused to count as reaching is still
        // evidence. Refusing it was right — a guess must not keep a cluster
        // alive — but discarding it made the component claim *more* confident
        // than the single-symbol claim built from the same call site.
        let qualified = component_is_qualified(&member_names, &qualifying);
        found.push(DeadClusterReport {
            cluster_id: component_id as u32,
            members: member_names.into_iter().take(shown).collect(),
            size,
            confidence: if qualified {
                DEAD_CLUSTER_QUALIFIED_CONFIDENCE
            } else {
                DEAD_CLUSTER_CONFIDENCE
            },
            reason: if qualified {
                qualified_cluster_reason(size, shown)
            } else {
                cluster_reason(size, shown)
            },
        });
    }

    // Largest first: a 40-symbol abandoned subsystem is worth more of a
    // reader's attention than a two-function recursion, and the cap below has
    // to keep the ones that matter.
    found.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.cluster_id.cmp(&right.cluster_id))
    });
    let truncated_clusters = found.len().saturating_sub(DEAD_CLUSTER_CAP);
    found.truncate(DEAD_CLUSTER_CAP);

    // Derived after truncation deliberately: the *file* claim rests on the
    // clusters that were found, not on the ones that fit in the report. Basing
    // it on the truncated list would make a file's reachability depend on how
    // many other clusters happened to exist.
    let unreachable_files = files_wholly_inside_clusters(extractions, &clustered_symbols);

    DeadClusterScan {
        clusters: found,
        unreachable_files,
        truncated_clusters,
        refused_oversized_graph: false,
    }
}

/// Files every one of whose declared symbols is in a dead cluster.
///
/// The `File` symbol itself is excluded from the test: it is the node the file
/// *is*, not something the file declares, and it never joins a call cycle. A
/// file that declares nothing else is skipped entirely rather than counted
/// unreachable, because "declares nothing" is not evidence of anything.
///
/// A file the one canonical predicate says is not a liveness candidate is
/// skipped for the same reason it is skipped by `unwired_candidates`: this is
/// a *file-level* verdict published as `unreachable_files`, and naming a
/// fixture, a package marker or a Terraform module in it is the same wrong
/// answer arriving by a second route. It had one — every symbol in a
/// `testdata/` module can perfectly well form an abandoned cycle, because that
/// is what fixture code looks like.
fn files_wholly_inside_clusters(
    extractions: &[Extraction],
    clustered: &BTreeSet<String>,
) -> Vec<String> {
    if clustered.is_empty() {
        return Vec::new();
    }
    let mut unreachable: Vec<String> = extractions
        .iter()
        .filter(|ext| {
            if !ext.file_liveness().is_candidate() {
                return false;
            }
            let mut declared = ext
                .symbols
                .iter()
                .filter(|symbol| symbol.kind != devmap_extract::model::SymbolKind::File)
                .peekable();
            if declared.peek().is_none() {
                return false;
            }
            declared.all(|symbol| clustered.contains(&symbol.qualified_name))
        })
        .map(|ext| ext.file_path.clone())
        .collect();
    unreachable.sort();
    unreachable.dedup();
    unreachable
}

fn cluster_reason(size: usize, shown: usize) -> String {
    let mut reason = if size == 1 {
        // A one-member strongly-connected component is a symbol that calls
        // itself. "1 symbols that reference only each other" is not merely
        // ungrammatical: it describes a group, and a reader looking for the
        // other members of a group that has none reads the finding as
        // truncated. Recursion is the whole of why a self-loop hides from the
        // one-hop join, so the sentence says that instead.
        recursive_cluster_reason()
    } else {
        format!(
            "{size} symbols that reference only each other, reached by nothing outside the \
             component — an abandoned cycle is invisible to the one-hop liveness join, because \
             every member has an inbound edge from another member"
        )
    };
    append_sample_note(&mut reason, size, shown);
    reason
}

/// The self-recursion wording, shared by both reason builders so a one-member
/// component reads the same way whichever tier reports it.
fn recursive_cluster_reason() -> String {
    "1 symbol that only calls itself, reached by nothing else — a recursive function is \
     invisible to the one-hop liveness join, because its one inbound edge is its own"
        .to_string()
}

/// The reason for a component something reaches through evidence the resolver
/// could not bind.
///
/// A separate sentence rather than a suffix on the one above, because the first
/// clause of that sentence — "reached by nothing outside the component" — is
/// exactly what stops being true here, and appending a caveat to a false claim
/// leaves the false claim in the text an agent reads first.
fn qualified_cluster_reason(size: usize, shown: usize) -> String {
    if size == 1 {
        let mut reason = recursive_cluster_reason();
        reason.push_str(
            " — except that something outside it names the symbol through a call the resolver \
             could not bind, so \"nothing reaches it\" is a statement about the resolver rather \
             than about the code",
        );
        append_sample_note(&mut reason, size, shown);
        return reason;
    }
    let mut reason = format!(
        "{size} symbols that reference only each other, and something outside the component \
         names one of them through a call the resolver could not bind — an ambiguous \
         candidate, or an unresolved site. So the cycle may well be abandoned, but \"nothing \
         reaches it\" is a statement about the resolver rather than about the code, and this \
         is reported at the same tier a single symbol with the same evidence gets"
    );
    append_sample_note(&mut reason, size, shown);
    reason
}

/// One owner for the truncation note, so a capped sample says so whichever
/// reason it carries.
fn append_sample_note(reason: &mut String, size: usize, shown: usize) {
    if shown < size {
        reason.push_str(&format!("; {shown} of {size} members listed"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The algorithm, on graphs small enough to reason about by hand.
    #[test]
    fn tarjan_finds_the_components() {
        // 0 -> 1 -> 2 -> 0  (one component), 3 -> 4 (two singletons)
        let adjacency = vec![vec![1], vec![2], vec![0], vec![4], vec![]];
        let mut sizes: Vec<usize> = strongly_connected_components(&adjacency)
            .into_iter()
            .map(|c| c.len())
            .collect();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![1, 1, 3]);
    }

    /// A graph with no edges has one component per node, and no cycles.
    #[test]
    fn tarjan_on_an_edgeless_graph_finds_only_singletons() {
        let adjacency = vec![vec![], vec![], vec![]];
        let components = strongly_connected_components(&adjacency);
        assert_eq!(components.len(), 3);
        assert!(components.iter().all(|c| c.len() == 1));
    }

    /// A self-loop is a component of one, and the walk must terminate.
    #[test]
    fn tarjan_handles_a_self_loop() {
        let adjacency = vec![vec![0]];
        assert_eq!(strongly_connected_components(&adjacency), vec![vec![0]]);
    }

    /// The adversarial case, and the reason this is iterative.
    ///
    /// A 100,000-node cycle in a recursive Tarjan is 100,000 stack frames. The
    /// CLI builds with `panic = "abort"` and has no `catch_unwind` anywhere, so
    /// the failure would be a process death on input a user can write — a
    /// generated state machine or a mutually recursive parser reaches this
    /// shape. Ten times the size the plan asked for, to leave headroom.
    #[test]
    fn a_hundred_thousand_node_cycle_does_not_overflow_the_stack() {
        let n = 100_000usize;
        let adjacency: Vec<Vec<u32>> = (0..n).map(|i| vec![((i + 1) % n) as u32]).collect();
        let components = strongly_connected_components(&adjacency);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].len(), n);
    }

    /// A deep *chain* is the other stack shape: no cycle, maximum depth.
    #[test]
    fn a_hundred_thousand_node_chain_does_not_overflow_the_stack() {
        let n = 100_000usize;
        let adjacency: Vec<Vec<u32>> = (0..n)
            .map(|i| {
                if i + 1 < n {
                    vec![(i + 1) as u32]
                } else {
                    vec![]
                }
            })
            .collect();
        let components = strongly_connected_components(&adjacency);
        assert_eq!(components.len(), n, "a chain is n singletons");
    }
}

//! File-level community detection.
//!
//! N5: weighted Louvain modularity optimisation followed by a connectivity
//! post-pass, so every emitted community is guaranteed internally connected.
//! Connected components alone are not community detection — on a real import
//! graph they collapse to one giant component and R6's cohesion score becomes
//! meaningless. Louvain partitions *within* a component; the post-pass restores
//! the connectivity guarantee that Louvain does not itself provide.
//!
//! R4: every container is ordered and every tie broken by lowest index, so the
//! partition is byte-identical across runs. No wall-clock budget is used —
//! elapsed time is not reproducible, so termination is bounded by pass count
//! instead.

use crate::model::*;
use devmap_extract::model::*;
use devmap_resolve::model::*;
use std::collections::{BTreeMap, BTreeSet};

/// Louvain aggregation levels. Each level strictly increases modularity or the
/// loop stops; the bound only guards pathological graphs.
const MAX_PASSES: usize = 20;
/// Local-moving sweeps within one level.
const MAX_LOCAL_ROUNDS: usize = 50;
/// Modularity gains below this are treated as noise rather than improvements,
/// so float jitter cannot flip a tie and break determinism.
const MIN_GAIN: f64 = 1e-12;

/// Undirected weighted graph over dense node indices.
///
/// `adj[i][j]` (i != j) is the edge weight; `adj[i][i]` is self-loop weight,
/// which carries a community's internal edges through aggregation.
struct Graph {
    adj: Vec<BTreeMap<usize, f64>>,
}

impl Graph {
    /// Weighted degree. Self-loops contribute twice, per the standard
    /// modularity convention.
    fn degree(&self, node: usize) -> f64 {
        self.adj[node]
            .iter()
            .map(|(&j, &w)| if j == node { 2.0 * w } else { w })
            .sum()
    }

    fn total_degree(&self) -> f64 {
        (0..self.adj.len()).map(|i| self.degree(i)).sum()
    }
}

pub fn detect_communities(
    extractions: &[Extraction],
    resolution: &ResolutionResult,
) -> Vec<CommunityReport> {
    // Sorted node index; ordering is the determinism anchor for everything below.
    let mut names: BTreeSet<String> = BTreeSet::new();
    for ext in extractions {
        names.insert(ext.file_path.clone());
    }
    for edge in &resolution.edges {
        names.insert(edge.source_file.clone());
        names.insert(edge.target_file.clone());
    }
    let names: Vec<String> = names.into_iter().collect();
    if names.is_empty() {
        return Vec::new();
    }
    let index: BTreeMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();

    // Edge weight is call multiplicity: every resolved edge between two files
    // adds one, so heavily-coupled file pairs resist being split apart.
    let mut weights: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for edge in &resolution.edges {
        let (Some(&s), Some(&t)) = (
            index.get(edge.source_file.as_str()),
            index.get(edge.target_file.as_str()),
        ) else {
            continue;
        };
        if s == t {
            continue; // Self-calls carry no partitioning information.
        }
        let key = if s < t { (s, t) } else { (t, s) };
        *weights.entry(key).or_insert(0.0) += 1.0;
    }

    let mut adj: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); names.len()];
    for (&(a, b), &w) in &weights {
        adj[a].insert(b, w);
        adj[b].insert(a, w);
    }
    let base = Graph { adj };

    let partition = louvain(&base);
    let partition = split_disconnected(&base, &partition);
    emit(&names, &base, &partition)
}

/// Multi-level Louvain. Returns a community label per node of `graph`.
fn louvain(graph: &Graph) -> Vec<usize> {
    let n = graph.adj.len();
    // Every node in its own community is already optimal when there are no
    // edges to trade off, and the modularity denominator would be zero.
    let total = graph.total_degree();
    if total <= 0.0 {
        return (0..n).collect();
    }

    // Maps original nodes to communities of the current (possibly aggregated)
    // level; rewritten after each aggregation.
    let mut labels: Vec<usize> = (0..n).collect();
    let mut level = Graph {
        adj: graph.adj.clone(),
    };

    for _ in 0..MAX_PASSES {
        let local = local_moving(&level, total);
        let (compact, count) = compact_labels(&local);
        // No node changed community: further aggregation cannot help.
        if count == level.adj.len() {
            break;
        }
        for label in labels.iter_mut() {
            *label = compact[*label];
        }
        level = aggregate(&level, &compact, count);
    }

    let (labels, _) = compact_labels(&labels);
    labels
}

/// Phase 1: move nodes to the neighbouring community with the best modularity
/// gain until nothing moves.
fn local_moving(graph: &Graph, total_degree: f64) -> Vec<usize> {
    let n = graph.adj.len();
    let mut community: Vec<usize> = (0..n).collect();
    let degree: Vec<f64> = (0..n).map(|i| graph.degree(i)).collect();
    let mut sigma_tot: Vec<f64> = degree.clone();

    for _ in 0..MAX_LOCAL_ROUNDS {
        let mut moved = false;
        // Ascending node order — the tie-break anchor (R4).
        for node in 0..n {
            let current = community[node];
            sigma_tot[current] -= degree[node];

            // Weight from `node` into each candidate community.
            let mut incident: BTreeMap<usize, f64> = BTreeMap::new();
            for (&other, &w) in &graph.adj[node] {
                if other == node {
                    continue;
                }
                *incident.entry(community[other]).or_insert(0.0) += w;
            }

            let gain = |c: usize| -> f64 {
                incident.get(&c).copied().unwrap_or(0.0)
                    - sigma_tot[c] * degree[node] / total_degree
            };

            let mut best = current;
            let mut best_gain = gain(current);
            // BTreeMap iterates ascending, and only a strictly greater gain
            // displaces the incumbent, so ties resolve to the lowest id.
            for &candidate in incident.keys() {
                let g = gain(candidate);
                if g > best_gain + MIN_GAIN {
                    best_gain = g;
                    best = candidate;
                }
            }

            sigma_tot[best] += degree[node];
            if best != current {
                community[node] = best;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }

    community
}

/// Renumber sparse labels to `0..count` in ascending order of first appearance.
fn compact_labels(labels: &[usize]) -> (Vec<usize>, usize) {
    let mut mapping: BTreeMap<usize, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(labels.len());
    for &label in labels {
        let next = mapping.len();
        let id = *mapping.entry(label).or_insert(next);
        out.push(id);
    }
    let count = mapping.len();
    (out, count)
}

/// Phase 2: collapse each community into a single node. Intra-community edges
/// become self-loop weight so the next level still sees them.
fn aggregate(graph: &Graph, community: &[usize], count: usize) -> Graph {
    let mut adj: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); count];
    for node in 0..graph.adj.len() {
        let cn = community[node];
        for (&other, &w) in &graph.adj[node] {
            if other == node {
                *adj[cn].entry(cn).or_insert(0.0) += w; // Self-loop carries over as-is.
                continue;
            }
            if other < node {
                continue; // Visit each undirected pair once.
            }
            let co = community[other];
            if co == cn {
                *adj[cn].entry(cn).or_insert(0.0) += w; // Now internal.
            } else {
                *adj[cn].entry(co).or_insert(0.0) += w;
                *adj[co].entry(cn).or_insert(0.0) += w;
            }
        }
    }
    Graph { adj }
}

/// N5's guarantee. Louvain can leave a community internally disconnected;
/// split any such community into its connected parts so the promise the
/// caller relies on actually holds.
fn split_disconnected(graph: &Graph, community: &[usize]) -> Vec<usize> {
    let n = community.len();
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (node, &c) in community.iter().enumerate() {
        members.entry(c).or_default().push(node);
    }

    let mut out = vec![usize::MAX; n];
    let mut next = 0usize;
    for group in members.values() {
        let group_set: BTreeSet<usize> = group.iter().copied().collect();
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for &start in group {
            if seen.contains(&start) {
                continue;
            }
            // BFS restricted to this community.
            let mut queue = vec![start];
            seen.insert(start);
            while let Some(curr) = queue.pop() {
                out[curr] = next;
                for &other in graph.adj[curr].keys() {
                    if other != curr && group_set.contains(&other) && seen.insert(other) {
                        queue.push(other);
                    }
                }
            }
            next += 1;
        }
    }
    out
}

/// Build reports, ordered by first member path so ids are stable (R4).
fn emit(names: &[String], graph: &Graph, community: &[usize]) -> Vec<CommunityReport> {
    let mut grouped: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for (node, &c) in community.iter().enumerate() {
        grouped.entry(c).or_default().insert(names[node].clone());
    }

    let index: BTreeMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();

    let mut ordered: Vec<Vec<String>> = grouped
        .into_values()
        .map(|m| m.into_iter().collect())
        .collect();
    ordered.sort();

    ordered
        .into_iter()
        .enumerate()
        .map(|(i, members)| {
            let ids: BTreeSet<usize> = members
                .iter()
                .filter_map(|m| index.get(m.as_str()).copied())
                .collect();

            // R6: cohesion is the share of incident weight that stays inside the
            // community. 1.0 means nothing leaves; 0.0 means everything does.
            // Isolated files have no incident weight and score 0.0 rather than a
            // flattering 1.0 — an unmeasurable value must not read as a perfect one.
            let mut internal = 0.0f64;
            let mut external = 0.0f64;
            for &node in &ids {
                for (&other, &w) in &graph.adj[node] {
                    if other == node {
                        continue;
                    }
                    if ids.contains(&other) {
                        internal += w; // Counted from both endpoints.
                    } else {
                        external += w;
                    }
                }
            }
            internal /= 2.0;
            let denominator = internal + external;
            let cohesion_score = if denominator > 0.0 {
                (internal / denominator) as f32
            } else {
                0.0
            };

            let id = (i + 1) as u32;
            CommunityReport {
                community_id: id,
                name: format!("community-{}", id),
                members,
                cohesion_score: cohesion_score.clamp(0.0, 1.0),
            }
        })
        .collect()
}

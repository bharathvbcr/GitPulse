//! The vault's link graph, and where to draw it.
//!
//! Layout lives here rather than in Swift for the reason the rest of the core
//! does: a force-directed layout is an inner loop over every pair of nodes,
//! run to convergence, and it has nothing to do with AppKit. Swift receives
//! finished coordinates and draws them.
//!
//! # Determinism
//!
//! Nodes start on a golden-angle spiral, not at random positions, and the
//! iteration count is fixed. The same vault therefore always lays out the
//! same way — the graph does not rearrange itself every time the panel is
//! opened, and the layout can be asserted in a test rather than eyeballed.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

use super::index::Vault;

/// One note, placed.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GraphNode {
    /// Vault-relative path; the identity Swift uses to open the note.
    pub path: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Links in plus links out. Drives the drawn radius, so a hub reads as one.
    pub degree: u32,
    /// Hops from the focused note, or 0 when the whole vault is shown.
    pub depth: u32,
    pub x: f32,
    pub y: f32,
}

/// A resolved link between two included notes.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct GraphEdge {
    pub source: u32,
    pub target: u32,
}

/// A laid-out graph, and an honest account of what it left out.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Notes in the whole vault.
    pub total_notes: u32,
    /// Whether a cap, rather than a filter, decided what is missing.
    ///
    /// Carried separately from the counts because "you asked for a subset" and
    /// "we could not draw them all" mean different things to a reader, and a
    /// graph that silently shows half a vault is worse than one that says so.
    pub truncated: bool,
}

/// What to include.
#[derive(Debug, Clone, Default)]
pub struct GraphQuery<'a> {
    /// Show only notes within `depth` hops of this one.
    pub focus: Option<&'a str>,
    /// Hops from `focus`. Ignored without a focus.
    pub depth: u32,
    /// Show only notes carrying this tag (matched case-insensitively, without
    /// the leading `#`).
    pub tag: Option<&'a str>,
    /// Folder prefix, vault-relative.
    pub folder: Option<&'a str>,
}

/// Above this, the O(n²) repulsion pass stops being instant.
///
/// A personal vault is far below it; a cap exists so a pathological one
/// degrades into a smaller graph rather than a frozen window. Nodes are kept
/// by degree, so what survives is the structure worth seeing.
pub const MAX_NODES: usize = 1500;

/// Layout iterations. Fixed, not convergence-tested: a bounded loop cannot
/// stall the caller, and the residual movement past this point is under a
/// pixel at any sane zoom.
const ITERATIONS: usize = 220;

/// Canvas the layout is normalised into. Swift rescales to its own view, so
/// this only fixes the aspect and keeps the numbers in a readable range.
const EXTENT: f32 = 1000.0;

impl Graph {
    /// Builds and lays out the graph for `query`.
    pub fn build(vault: &Vault, query: &GraphQuery) -> Graph {
        let total_notes = vault.notes().len() as u32;

        // Resolved adjacency over the whole vault first: a local view is a
        // breadth-first walk of the real graph, so it cannot be computed from
        // a pre-filtered one.
        let index_of: HashMap<&str, usize> = vault
            .notes()
            .iter()
            .enumerate()
            .map(|(index, note)| (note.path.as_str(), index))
            .collect();

        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); vault.notes().len()];
        let mut links: HashSet<(usize, usize)> = HashSet::new();
        for (source, note) in vault.notes().iter().enumerate() {
            for link in &note.links {
                // Unresolved links have no node to point at. They are not lost
                // — `Vault::broken_links` reports them — but a graph cannot
                // draw an edge to a note that does not exist.
                let Some(resolved) = vault.resolve_link(&note.path, link) else {
                    continue;
                };
                let Some(&target) = index_of.get(resolved.path.as_str()) else {
                    continue;
                };
                if target == source {
                    continue;
                }
                // Keyed on the *unordered* pair. A and B linking to each other
                // is one relationship: recording it twice would draw two lines
                // over each other, double the attraction pulling the pair
                // together, and count as two toward each note's degree — so a
                // mutually-linked pair would render as a hub.
                let pair = (source.min(target), source.max(target));
                if links.insert(pair) {
                    adjacency[source].push(target);
                    adjacency[target].push(source);
                }
            }
        }

        let (included, depths) = select(vault, query, &adjacency);
        let (included, truncated) = cap(included, &adjacency);

        // Dense renumbering, so edges index into `nodes` directly.
        let position: HashMap<usize, u32> = included
            .iter()
            .enumerate()
            .map(|(dense, &sparse)| (sparse, dense as u32))
            .collect();

        let mut edges: Vec<GraphEdge> = links
            .iter()
            .filter_map(|&(source, target)| {
                Some(GraphEdge {
                    source: *position.get(&source)?,
                    target: *position.get(&target)?,
                })
            })
            .collect();
        // Sorted so the output is byte-identical across runs; `HashSet`
        // iteration order is not.
        edges.sort_by_key(|edge| (edge.source, edge.target));

        let points = layout(included.len(), &edges);

        let nodes = included
            .iter()
            .enumerate()
            .map(|(dense, &sparse)| {
                let note = &vault.notes()[sparse];
                GraphNode {
                    path: note.path.clone(),
                    title: note.title.clone(),
                    tags: note.tags.clone(),
                    degree: adjacency[sparse].len() as u32,
                    depth: depths.get(&sparse).copied().unwrap_or(0),
                    x: points[dense].0,
                    y: points[dense].1,
                }
            })
            .collect();

        Graph {
            nodes,
            edges,
            total_notes,
            truncated,
        }
    }
}

/// The note indices the query admits, and how far each is from the focus.
fn select(
    vault: &Vault,
    query: &GraphQuery,
    adjacency: &[Vec<usize>],
) -> (Vec<usize>, HashMap<usize, u32>) {
    let mut depths: HashMap<usize, u32> = HashMap::new();

    let candidates: Vec<usize> = match query.focus {
        Some(focus) => {
            let start = vault
                .notes()
                .iter()
                .position(|note| note.path == focus)
                .or_else(|| {
                    vault
                        .resolve(focus, None)
                        .and_then(|r| vault.notes().iter().position(|n| n.path == r.path))
                });
            let Some(start) = start else {
                return (Vec::new(), depths);
            };

            // Breadth-first so `depth` counts hops, not link order.
            let mut queue = VecDeque::from([start]);
            let mut order = Vec::new();
            depths.insert(start, 0);
            while let Some(current) = queue.pop_front() {
                order.push(current);
                let next_depth = depths[&current] + 1;
                if next_depth > query.depth {
                    continue;
                }
                for &neighbour in &adjacency[current] {
                    if depths.contains_key(&neighbour) {
                        continue;
                    }
                    depths.insert(neighbour, next_depth);
                    queue.push_back(neighbour);
                }
            }
            order
        }
        None => (0..vault.notes().len()).collect(),
    };

    let mut included: Vec<usize> = candidates
        .into_iter()
        .filter(|&index| matches(&vault.notes()[index], query))
        .collect();

    // Vault order — which is path order — regardless of how the candidates
    // were gathered. The breadth-first walk yields focus-first order and the
    // cap re-sorts, so without this the node ordering, and therefore the
    // layout, would depend on which of those paths ran.
    included.sort_unstable();
    (included, depths)
}

fn matches(note: &super::note::Note, query: &GraphQuery) -> bool {
    if let Some(tag) = query.tag {
        let wanted = tag.trim_start_matches('#');
        if !note.tags.iter().any(|candidate| {
            candidate
                .trim_start_matches('#')
                .eq_ignore_ascii_case(wanted)
        }) {
            return false;
        }
    }
    if let Some(folder) = query.folder {
        let folder = folder.trim_matches('/');
        if !folder.is_empty() {
            let prefix = format!("{folder}/");
            if !note.path.starts_with(&prefix) {
                return false;
            }
        }
    }
    true
}

/// Trims to ``MAX_NODES``, keeping the best-connected notes.
fn cap(mut included: Vec<usize>, adjacency: &[Vec<usize>]) -> (Vec<usize>, bool) {
    if included.len() <= MAX_NODES {
        // Kept in vault order — which is path order — so the layout is stable.
        return (included, false);
    }
    included.sort_by_key(|&index| (std::cmp::Reverse(adjacency[index].len()), index));
    included.truncate(MAX_NODES);
    included.sort_unstable();
    (included, true)
}

/// Fruchterman-Reingold, run for a fixed number of cooling steps.
fn layout(count: usize, edges: &[GraphEdge]) -> Vec<(f32, f32)> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![(EXTENT / 2.0, EXTENT / 2.0)];
    }

    let area = EXTENT * EXTENT;
    let ideal = (area / count as f32).sqrt();

    // Golden-angle spiral: even coverage with no clumping, and identical every
    // run. Random placement would give a different picture each time the panel
    // opened, which reads as the graph being unstable rather than the layout
    // being unseeded.
    let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    let mut points: Vec<(f32, f32)> = (0..count)
        .map(|i| {
            let radius = EXTENT * 0.45 * ((i as f32 + 0.5) / count as f32).sqrt();
            let angle = golden * i as f32;
            (
                EXTENT / 2.0 + radius * angle.cos(),
                EXTENT / 2.0 + radius * angle.sin(),
            )
        })
        .collect();

    let mut displacement = vec![(0.0_f32, 0.0_f32); count];
    let mut temperature = EXTENT / 10.0;
    let cooling = temperature / (ITERATIONS as f32 + 1.0);

    for _ in 0..ITERATIONS {
        for slot in displacement.iter_mut() {
            *slot = (0.0, 0.0);
        }

        // Repulsion between every pair.
        for i in 0..count {
            for j in (i + 1)..count {
                let dx = points[i].0 - points[j].0;
                let dy = points[i].1 - points[j].1;
                // Coincident nodes have no direction to separate along.
                // Nudging by the index keeps that deterministic rather than
                // leaving them stacked forever.
                let (dx, dy) = if dx == 0.0 && dy == 0.0 {
                    (0.01 * (i as f32 + 1.0), 0.01 * (j as f32 + 1.0))
                } else {
                    (dx, dy)
                };
                let distance = (dx * dx + dy * dy).sqrt().max(0.01);
                let force = (ideal * ideal) / distance;
                let (ux, uy) = (dx / distance, dy / distance);
                displacement[i].0 += ux * force;
                displacement[i].1 += uy * force;
                displacement[j].0 -= ux * force;
                displacement[j].1 -= uy * force;
            }
        }

        // Attraction along edges.
        for edge in edges {
            let (source, target) = (edge.source as usize, edge.target as usize);
            let dx = points[source].0 - points[target].0;
            let dy = points[source].1 - points[target].1;
            let distance = (dx * dx + dy * dy).sqrt().max(0.01);
            let force = (distance * distance) / ideal;
            let (ux, uy) = (dx / distance, dy / distance);
            displacement[source].0 -= ux * force;
            displacement[source].1 -= uy * force;
            displacement[target].0 += ux * force;
            displacement[target].1 += uy * force;
        }

        for index in 0..count {
            let (dx, dy) = displacement[index];
            let magnitude = (dx * dx + dy * dy).sqrt().max(0.01);
            // Capped by the temperature, which is what makes the layout settle
            // instead of oscillating.
            let step = magnitude.min(temperature);
            points[index].0 += (dx / magnitude) * step;
            points[index].1 += (dy / magnitude) * step;
        }

        temperature -= cooling;
    }

    normalise(points)
}

/// Fits the laid-out points into the canvas, preserving the aspect ratio.
///
/// Scaling each axis independently would stretch a chain of notes into a line
/// across the panel, which reads as a layout bug rather than as a chain.
fn normalise(points: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for &(x, y) in &points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    let span = (max_x - min_x).max(max_y - min_y);
    if !span.is_finite() || span <= f32::EPSILON {
        return points
            .iter()
            .map(|_| (EXTENT / 2.0, EXTENT / 2.0))
            .collect();
    }

    let margin = EXTENT * 0.06;
    let scale = (EXTENT - margin * 2.0) / span;
    // Centred rather than corner-aligned, so a graph narrower than it is tall
    // does not hug the left edge.
    let offset_x = (EXTENT - (max_x - min_x) * scale) / 2.0;
    let offset_y = (EXTENT - (max_y - min_y) * scale) / 2.0;

    points
        .into_iter()
        .map(|(x, y)| {
            (
                (x - min_x) * scale + offset_x,
                (y - min_y) * scale + offset_y,
            )
        })
        .collect()
}

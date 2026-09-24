//! Plain-language find over names, docstrings, and the call graph.
//!
//! [`search_semantic`](crate::StoreQueryEngine::search_semantic) ranks names
//! only. A question about behaviour is a different question, so it gets its own
//! path: TF-IDF over names plus docstrings when present, then personalized
//! PageRank over stored call edges. Nothing is stored — no body index, no
//! second derived table.

use std::collections::{HashMap, HashSet};

use devmap_extract::model::{EdgeKind, Extraction};
use devmap_store::GenerationEdges;

use crate::cancel::{Cancel, QueryCancelled};
use crate::rung::Rung;

/// Restart (teleport) probability for personalized PageRank.
pub const ASK_RESTART: f32 = 0.25;

/// Hard iteration cap. Cancellation is checked each step; this bound is what
/// keeps a dense call graph from running unbounded even when nobody cancels.
pub const ASK_MAX_ITERS: usize = 32;

/// Default edge floor: the deterministic rung.
///
/// Edges below this are excluded unless the caller passes a lower
/// `min_confidence`. Speculative-only seed neighbourhoods therefore come back
/// empty at the default, with an explicit withheld line rather than a silent
/// zero.
pub const ASK_DEFAULT_MIN_CONFIDENCE: f32 = {
    // `Rung::Deterministic.floor_millis()` is 1000; keep the float here so the
    // IPC/CLI defaults name the same constant the walk uses.
    Rung::Deterministic.floor_millis() as f32 / 1000.0
};

/// One seed text for TF-IDF: bare name, qualified name, and docstring when set.
pub fn seed_text(name: &str, qualified_name: &str, docstring: Option<&str>) -> String {
    match docstring.map(str::trim).filter(|text| !text.is_empty()) {
        Some(doc) => format!("{name} {qualified_name} {doc}"),
        None => format!("{name} {qualified_name}"),
    }
}

/// Docstrings keyed by qualified name, only where the extraction carried one.
pub fn docstring_by_qualified_name(extractions: &[Extraction]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for extraction in extractions {
        for symbol in &extraction.symbols {
            if let Some(doc) = symbol
                .docstring
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                map.insert(symbol.qualified_name.clone(), doc.to_string());
            }
        }
    }
    map
}

/// Call-edge adjacency among `nodes`, under `min_confidence`, Calls only.
///
/// Returns `(outbound neighbor indices, any_call_touched_seed,
/// admitted_call_touched_seed)`. The two booleans let the caller distinguish
/// "no edges exist" from "edges exist but every one was below the floor".
pub fn call_adjacency(
    index: &GenerationEdges,
    nodes: &[String],
    seeds: &HashSet<&str>,
    min_confidence: f32,
) -> (Vec<Vec<usize>>, bool, bool) {
    let mut rank: HashMap<&str, usize> = HashMap::with_capacity(nodes.len());
    for (i, name) in nodes.iter().enumerate() {
        rank.insert(name.as_str(), i);
    }
    let mut outbound: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut any_call_touched_seed = false;
    let mut admitted_call_touched_seed = false;

    for id in 0..index.len() as u32 {
        if index.kind(id) != EdgeKind::Calls {
            continue;
        }
        let source = index.source_symbol(id);
        let target = index.target_symbol(id);
        let touches_seed = seeds.contains(source) || seeds.contains(target);
        if touches_seed {
            any_call_touched_seed = true;
        }
        if !index.admits(id, min_confidence) {
            continue;
        }
        if touches_seed {
            admitted_call_touched_seed = true;
        }
        let Some(&from) = rank.get(source) else {
            continue;
        };
        let Some(&to) = rank.get(target) else {
            continue;
        };
        if from != to {
            outbound[from].push(to);
        }
    }
    for neighbours in &mut outbound {
        neighbours.sort_unstable();
        neighbours.dedup();
    }
    (outbound, any_call_touched_seed, admitted_call_touched_seed)
}

/// Personalized PageRank with restart probability [`ASK_RESTART`].
///
/// `personalization` must be non-negative and sum to a positive total over the
/// seed mass; dangling nodes redistribute according to it. Cancellation is
/// checked once per iteration.
pub fn personalized_pagerank(
    outbound: &[Vec<usize>],
    personalization: &[f32],
    cancel: &Cancel,
) -> Result<Vec<f32>, QueryCancelled> {
    let n = outbound.len();
    debug_assert_eq!(n, personalization.len());
    if n == 0 {
        return Ok(Vec::new());
    }
    let teleport_sum: f32 = personalization.iter().sum();
    if teleport_sum <= 0.0 || teleport_sum.is_nan() {
        return Ok(vec![0.0; n]);
    }
    let teleport: Vec<f32> = personalization
        .iter()
        .map(|weight| weight / teleport_sum)
        .collect();

    let mut rank = teleport.clone();
    for step in 0..ASK_MAX_ITERS {
        cancel.check_every(step)?;
        let mut next = vec![0.0; n];
        for (i, outs) in outbound.iter().enumerate() {
            if outs.is_empty() {
                let mass = rank[i];
                for (j, weight) in teleport.iter().enumerate() {
                    next[j] += mass * weight;
                }
            } else {
                let share = rank[i] / outs.len() as f32;
                for &j in outs {
                    next[j] += share;
                }
            }
        }
        for j in 0..n {
            next[j] = ASK_RESTART * teleport[j] + (1.0 - ASK_RESTART) * next[j];
        }
        rank = next;
    }
    Ok(rank)
}

/// Line carried when seeds matched but every call edge among them sat below
/// the confidence floor.
pub fn confidence_withheld_reason() -> String {
    "matches were withheld for confidence rather than absent: every call edge \
     among the seeds sits below the confidence floor"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::Cancel;

    #[test]
    fn seed_text_includes_docstring_only_when_present() {
        assert_eq!(seed_text("cache", "a.py::cache", None), "cache a.py::cache");
        assert_eq!(
            seed_text("cache", "a.py::cache", Some("  stores llm replies  ")),
            "cache a.py::cache stores llm replies"
        );
        assert_eq!(
            seed_text("cache", "a.py::cache", Some("   ")),
            "cache a.py::cache"
        );
    }

    #[test]
    fn pagerank_survives_a_call_cycle() {
        // 0 → 1 → 0. Without a fixed iter cap this would not terminate under a
        // naive walk; with one it must finish and keep both nodes positive.
        let outbound = vec![vec![1], vec![0]];
        let personalization = vec![1.0, 0.0];
        let ranks = personalized_pagerank(&outbound, &personalization, &Cancel::new()).unwrap();
        assert_eq!(ranks.len(), 2);
        assert!(ranks[0] > 0.0 && ranks[1] > 0.0, "{ranks:?}");
        assert!(
            ranks[0] >= ranks[1],
            "seed should keep at least as much mass as its callee: {ranks:?}"
        );
    }

    #[test]
    fn default_floor_is_the_deterministic_rung() {
        assert!((ASK_DEFAULT_MIN_CONFIDENCE - 1.0).abs() < f32::EPSILON);
    }
}

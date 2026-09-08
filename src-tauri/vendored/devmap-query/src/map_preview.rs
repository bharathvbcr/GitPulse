//! The Dev Map preview: a self-contained `.devcouncil/map.html`.
//!
//! Reads the `repo_map.json` this crate's [`crate::manifest`] writes and
//! renders one offline page — no network, no CDN, the force-graph bundle
//! inlined. It supersedes the Python renderer that used to live at
//! `src/devcouncil/indexing/map_viz.py`, and the stub
//! `render_subsystem_map_html` it replaced in [`crate::artifacts`].
//!
//! Two things the old page could not say, and this one does:
//!
//! * **Language.** Nodes were coloured by hashing the area name, which encodes
//!   nothing. They now carry the subsystem's dominant language in GitHub
//!   Linguist's own colour (see [`crate::linguist`]), and every subsystem
//!   carries the full histogram behind it.
//! * **Coverage.** The map's subsystems are a *selected* set, not a partition —
//!   on this repository 14 of 227 areas, holding 236 of 1503 indexed files. The
//!   old page drew the 14 circles and said nothing about the other 84%. The
//!   header now carries both numbers, and the repo-wide language bar is built
//!   from every indexed file rather than from the covered slice.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::artifacts::{write_atomic, ArtifactFingerprint};
use crate::escape::{html_escape, json_script_escape};
use crate::linguist::{palette, LINGUIST_VERSION, NEUTRAL_COLOR};

/// Liveness lists run to tens of thousands; the page shows a sample and says so.
const LIVENESS_CAP: usize = 256;
const ROLE_FILES_CAP: usize = 24;
const ENTRY_CRITICAL_CAP: usize = 40;
/// Languages listed per subsystem before the tail folds into "Other".
const SUBSYSTEM_LANG_CAP: usize = 8;
/// Languages in the repo-wide bar before the tail folds into "Other".
const REPO_LANG_CAP: usize = 32;

/// The vendored force-graph build, inlined so the page works offline.
///
/// Read from this crate's own asset tree so `devmap-query` can be vendored or
/// published without preserving DevCouncil's Python-package layout. The graph
/// visualizer uses this same copy; one crate owns the renderer bytes.
const FORCE_GRAPH_JS: &str = include_str!("../assets/force-graph.min.js.bundle");

const MAP_HTML_TEMPLATE: &str = include_str!("map_preview.html");

fn as_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn string_list(value: &Value, key: &str, cap: usize) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .take(cap)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn strip_glob(hint: &str) -> String {
    let text = normalize(hint.trim());
    let text = text.strip_suffix("/*").unwrap_or(&text);
    text.trim().trim_matches('/').to_string()
}

/// Longest-prefix / suffix match of a handoff path fragment to a subsystem area.
///
/// Ported from the Python renderer unchanged: handoff text names *files*
/// (`cli/main.py -> executors/*`) while the graph's nodes are areas, so each
/// side has to be resolved to the area that contains it before an edge exists.
pub fn match_area(hint: &str, areas: &[String]) -> Option<String> {
    let cleaned = strip_glob(hint);
    if cleaned.is_empty() {
        return None;
    }
    let area_list: Vec<String> = areas
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| normalize(a))
        .collect();
    if area_list.iter().any(|a| a == &cleaned) {
        return Some(cleaned);
    }
    let mut best: Option<String> = None;
    let mut best_score: isize = -1;
    for area in &area_list {
        if area == &cleaned
            || area.ends_with(&format!("/{cleaned}"))
            || cleaned.starts_with(&format!("{area}/"))
        {
            let score = if area.contains(&cleaned) || area.ends_with(&cleaned) {
                cleaned.len() as isize
            } else {
                area.len() as isize
            };
            if score > best_score {
                best_score = score;
                best = Some(area.clone());
            }
            continue;
        }
        let parts: Vec<&str> = area.split('/').collect();
        for i in 0..parts.len() {
            let suffix = parts[i..].join("/");
            if cleaned == suffix
                || cleaned.starts_with(&format!("{suffix}/"))
                || suffix.starts_with(&format!("{cleaned}/"))
            {
                let score = suffix.len() as isize;
                if score > best_score {
                    best_score = score;
                    best = Some(area.clone());
                }
            }
        }
    }
    best
}

/// Parse `A -> B` handoff text into `(source area, target area, display text)`.
pub fn resolve_handoff(
    handoff: &str,
    areas: &[String],
) -> (Option<String>, Option<String>, String) {
    let raw = handoff.trim().to_string();
    let Some((left, right)) = raw.split_once(" -> ") else {
        return (None, None, raw);
    };
    let src = match_area(left, areas);
    let dst = match_area(right, areas);
    (src, dst, raw)
}

/// `{id: n}` as `[[id, n], …]`, biggest first, ties broken by id.
///
/// Everything past `cap` folds into the `generic` ("Other") bucket rather than
/// being dropped, so the result always re-sums to the input total. A truncated
/// ranking makes every bar built from it quietly describe fewer files than the
/// header beside it claims — which is exactly the bug the first cut of this
/// page shipped: a 32-language cap turned 1503 files into a bar totalling 1486.
fn rank(counts: &BTreeMap<String, usize>, cap: usize) -> Vec<(String, usize)> {
    let mut ordered: Vec<(String, usize)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if ordered.len() <= cap {
        return ordered;
    }
    let tail: usize = ordered[cap.saturating_sub(1)..]
        .iter()
        .map(|(_, n)| n)
        .sum();
    let mut merged: BTreeMap<String, usize> =
        ordered[..cap.saturating_sub(1)].iter().cloned().collect();
    if tail > 0 {
        *merged.entry("generic".to_string()).or_insert(0) += tail;
    }
    let mut out: Vec<(String, usize)> = merged.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

fn pairs_json(ranked: &[(String, usize)]) -> Value {
    Value::Array(
        ranked
            .iter()
            .map(|(id, n)| json!([id, n]))
            .collect::<Vec<_>>(),
    )
}

/// What one pass over `files[]` established.
///
/// Named rather than a 4-tuple because three of the four members are
/// `BTreeMap<String, …>` and a positional return makes them interchangeable at
/// every call site — `by_area` and `repo_languages` are not the same question.
#[derive(Debug, Clone, Default)]
pub struct FileAttribution {
    /// Language histogram per subsystem area.
    pub by_area: BTreeMap<String, BTreeMap<String, usize>>,
    /// Language histogram for every indexed file, attributed or not.
    pub repo_languages: BTreeMap<String, usize>,
    /// Kind histogram for every indexed file.
    pub repo_kinds: BTreeMap<String, usize>,
    /// How many files landed inside some subsystem.
    pub attributed: usize,
}

/// Files bucketed by owning subsystem, plus repo-wide language and kind tallies.
///
/// A file belongs to the *longest* subsystem area that prefixes its path, so
/// nested subsystems (`vendor/grammars/cobol` inside
/// `vendor/grammars/cobol/tree_sitter`) each keep their own files instead of
/// the outer one swallowing the inner.
///
/// The repo tallies count every indexed file, attributed or not. Subsystems
/// cover a minority of the tree, and a language bar drawn from the covered
/// slice alone would describe a sixth of the repository while looking like it
/// described all of it.
pub fn attribute_files_to_areas(file_rows: &[Value], areas: &[String]) -> FileAttribution {
    let mut by_area: BTreeMap<String, BTreeMap<String, usize>> = areas
        .iter()
        .map(|area| (area.clone(), BTreeMap::new()))
        .collect();
    let mut repo_languages: BTreeMap<String, usize> = BTreeMap::new();
    let mut repo_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut attributed = 0usize;

    // Longest first so the first prefix hit is the most specific one.
    let mut ordered: Vec<&String> = areas.iter().filter(|a| !a.is_empty()).collect();
    ordered.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));

    for row in file_rows {
        let path = normalize(row.get("path").and_then(Value::as_str).unwrap_or_default());
        if path.is_empty() {
            continue;
        }
        let language = {
            let raw = row
                .get("language")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if raw.is_empty() {
                "generic".to_string()
            } else {
                raw
            }
        };
        *repo_languages.entry(language.clone()).or_insert(0) += 1;
        let kind = row
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !kind.is_empty() {
            *repo_kinds.entry(kind).or_insert(0) += 1;
        }
        for area in &ordered {
            if path == **area || path.starts_with(&format!("{area}/")) {
                *by_area
                    .get_mut(*area)
                    .expect("every area seeded above")
                    .entry(language)
                    .or_insert(0) += 1;
                attributed += 1;
                break;
            }
        }
    }
    FileAttribution {
        by_area,
        repo_languages,
        repo_kinds,
        attributed,
    }
}

/// Build the preview payload embedded in the page.
///
/// Deliberately excludes `files[]` and `dependents{}`: both are large (1503 and
/// 14k entries here) and neither is needed once the histograms above are
/// derived. The page ships aggregates, not the inventory.
pub fn build_preview_payload(repo_map: &Value) -> Value {
    let empty = Vec::new();
    let subsystems_raw = repo_map
        .get("subsystems")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut subsystems: Vec<Map<String, Value>> = Vec::new();
    let mut areas: Vec<String> = Vec::new();
    for sub in subsystems_raw {
        if !sub.is_object() {
            continue;
        }
        let area = normalize(&as_str(sub, "area"));
        if area.is_empty() {
            continue;
        }
        areas.push(area.clone());

        let mut roles = Map::new();
        if let Some(role_files) = sub.get("role_files").and_then(Value::as_object) {
            for (role, paths) in role_files {
                if let Some(items) = paths.as_array() {
                    roles.insert(
                        role.clone(),
                        Value::Array(
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .take(ROLE_FILES_CAP)
                                .map(|p| Value::String(p.to_string()))
                                .collect(),
                        ),
                    );
                }
            }
        }

        let mut entry = Map::new();
        entry.insert("id".into(), json!(area));
        entry.insert("area".into(), json!(area));
        entry.insert(
            "name".into(),
            json!(area.rsplit_once('/').map(|(_, n)| n).unwrap_or(&area)),
        );
        entry.insert("summary".into(), json!(as_str(sub, "summary")));
        entry.insert(
            "entry_points".into(),
            json!(string_list(sub, "entry_points", ENTRY_CRITICAL_CAP)),
        );
        entry.insert(
            "critical_files".into(),
            json!(string_list(sub, "critical_files", ENTRY_CRITICAL_CAP)),
        );
        entry.insert(
            "neighbors".into(),
            json!(string_list(sub, "neighbors", usize::MAX)
                .iter()
                .map(|n| normalize(n))
                .collect::<Vec<_>>()),
        );
        entry.insert(
            "handoff_paths".into(),
            json!(string_list(sub, "handoff_paths", usize::MAX)),
        );
        entry.insert("role_files".into(), Value::Object(roles));
        subsystems.push(entry);
    }

    let area_set: BTreeSet<String> = areas.iter().cloned().collect();

    let file_rows = repo_map
        .get("files")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let files = attribute_files_to_areas(file_rows, &areas);

    for sub in subsystems.iter_mut() {
        let id = sub["id"].as_str().unwrap_or_default().to_string();
        let counts = files.by_area.get(&id).cloned().unwrap_or_default();
        let total: usize = counts.values().sum();
        let ranked = rank(&counts, SUBSYSTEM_LANG_CAP);
        sub.insert(
            "primary_language".into(),
            json!(ranked.first().map(|(id, _)| id.clone()).unwrap_or_default()),
        );
        sub.insert("languages".into(), pairs_json(&ranked));
        sub.insert("file_count".into(), json!(total));
    }

    let nodes: Vec<Value> = subsystems
        .iter()
        .map(|s| {
            let neighbors = s["neighbors"].as_array().map(Vec::len).unwrap_or(0);
            let handoffs = s["handoff_paths"].as_array().map(Vec::len).unwrap_or(0);
            let entries = s["entry_points"].as_array().map(Vec::len).unwrap_or(0);
            json!({
                "id": s["id"],
                "name": s["name"],
                "area": s["area"],
                "summary": s["summary"],
                "val": std::cmp::max(1, neighbors + handoffs + entries),
                "file_count": s["file_count"],
                "lang": s["primary_language"],
                "entry": entries > 0,
            })
        })
        .collect();

    let mut links: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<(String, String, &'static str)> = BTreeSet::new();
    let mut unresolved: Vec<Value> = Vec::new();

    for s in &subsystems {
        let src = s["id"].as_str().unwrap_or_default().to_string();
        for neighbor in s["neighbors"].as_array().unwrap_or(&empty) {
            let neighbor = neighbor.as_str().unwrap_or_default();
            let dst = if area_set.contains(neighbor) {
                Some(neighbor.to_string())
            } else {
                match_area(neighbor, &areas)
            };
            let Some(dst) = dst else { continue };
            if dst == src {
                continue;
            }
            let key = (src.clone(), dst.clone(), "neighbor");
            let rev = (dst.clone(), src.clone(), "neighbor");
            if seen.contains(&key) || seen.contains(&rev) {
                continue;
            }
            seen.insert(key);
            links.push(json!({"source": src, "target": dst, "kind": "neighbor"}));
        }
        for handoff in s["handoff_paths"].as_array().unwrap_or(&empty) {
            let text = handoff.as_str().unwrap_or_default();
            let (h_src, h_dst, display) = resolve_handoff(text, &areas);
            let mut source = h_src.unwrap_or_else(|| src.clone());
            let Some(h_dst) = h_dst else {
                unresolved.push(json!({"from": src, "text": display}));
                continue;
            };
            if !area_set.contains(&source) {
                source = src.clone();
            }
            if h_dst == source {
                continue;
            }
            let key = (source.clone(), h_dst.clone(), "handoff");
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            links.push(
                json!({"source": source, "target": h_dst, "kind": "handoff", "label": display}),
            );
        }
    }
    unresolved.truncate(100);

    let repo_ranked = rank(&files.repo_languages, REPO_LANG_CAP);
    let palette_ids: Vec<&str> = files.repo_languages.keys().map(String::as_str).collect();
    let palette_map: Map<String, Value> = palette(if palette_ids.is_empty() {
        vec!["generic"]
    } else {
        palette_ids
    })
    .into_iter()
    .map(|(id, s)| {
        (
            id,
            json!({"color": s.color, "label": s.label, "official": s.official}),
        )
    })
    .collect();

    let liveness_list = |key: &str| -> Value {
        json!(repo_map
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().take(LIVENESS_CAP).cloned().collect::<Vec<_>>())
            .unwrap_or_default())
    };

    json!({
        "nodes": nodes,
        "links": links,
        "subsystems": subsystems.into_iter().map(Value::Object).collect::<Vec<_>>(),
        "unresolved_handoffs": unresolved,
        "liveness": {
            "entry_roots": liveness_list("entry_roots"),
            "unwired_candidates": liveness_list("unwired_candidates"),
            "unreachable_files": liveness_list("unreachable_files"),
            "dead_symbol_candidates": liveness_list("dead_symbol_candidates"),
            "liveness_unreachable_unreliable": repo_map
                .get("liveness_unreachable_unreliable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "meta": {
            // Repo-wide, from every indexed file — see `attribute_files_to_areas`.
            "language_totals": pairs_json(&repo_ranked),
            "kind_totals": pairs_json(&rank(&files.repo_kinds, 16)),
            "palette": palette_map,
            "neutral_color": NEUTRAL_COLOR,
            "linguist_version": LINGUIST_VERSION,
            // What share of the tree the subsystems account for. The page
            // prints both numbers; a covered count shown alone reads as the
            // whole repository.
            "coverage": {
                "indexed_total": files.repo_languages.values().sum::<usize>(),
                "in_subsystems": files.attributed,
            },
            "generated_head": as_str(repo_map, "generated_head"),
            "graph_html": "graph/graph.html",
            "handoff_paths_computed": handoff_paths_computed(repo_map),
            "role_files_computed": role_files_computed(repo_map),
        },
    })
}

/// Whether this map carries evidence that handoff paths were derived at all.
///
/// The producer's marker, or one non-empty list anywhere in `subsystems`. The
/// kernel emitted `"handoff_paths": []` as a literal for the whole life of the
/// field, so on an older map an empty field means "this producer does not
/// compute it", not "nothing here crosses a boundary". The page has to say
/// which, or the detail pane's "(none)" is a claim it cannot support.
fn handoff_paths_computed(repo_map: &Value) -> bool {
    marker_or_evidence(repo_map, "handoff_paths_computed", |sub| {
        sub.get("handoff_paths")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    })
}

/// The same two claims for `role_files`, which carried the identical defect.
fn role_files_computed(repo_map: &Value) -> bool {
    marker_or_evidence(repo_map, "role_files_computed", |sub| {
        sub.get("role_files")
            .and_then(Value::as_object)
            .is_some_and(|roles| !roles.is_empty())
    })
}

fn marker_or_evidence(repo_map: &Value, marker: &str, has_evidence: fn(&Value) -> bool) -> bool {
    let marked = repo_map
        .get("meta")
        .and_then(|m| m.get("devmap_rust"))
        .and_then(|m| m.get(marker))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if marked {
        return true;
    }
    repo_map
        .get("subsystems")
        .and_then(Value::as_array)
        .is_some_and(|subs| subs.iter().any(has_evidence))
}

/// Render the self-contained preview page for a `repo_map.json` document.
pub fn render_map_preview_html(repo_map: &Value, fingerprint: &ArtifactFingerprint) -> String {
    let payload = build_preview_payload(repo_map);
    let raw = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    // Escaped for a raw-text `<script>`: HTML entities are not decoded there,
    // so `html_escape` would produce invalid JSON. `\uXXXX` keeps the HTML
    // parser and `JSON.parse` on the same representation.
    let safe_json = json_script_escape(&raw);

    // `__DATA_JSON__` is substituted last. `str::replace` does not rescan what
    // it inserts, so map content can never be mistaken for a later placeholder.
    MAP_HTML_TEMPLATE
        .replace("__FINGERPRINT__", &html_escape(&fingerprint.fingerprint))
        .replace("__VENDOR_JS__", FORCE_GRAPH_JS)
        .replace("__DATA_JSON__", &safe_json)
}

/// Derive the artifact fingerprint from the map's own freshness stamps.
///
/// Both artifacts of one build then agree about how fresh they are, and
/// [`crate::artifacts::should_regenerate`] can skip an unchanged rewrite.
pub fn fingerprint_for(repo_map: &Value) -> ArtifactFingerprint {
    let head = as_str(repo_map, "generated_head");
    let content = as_str(repo_map, "content_fingerprint");
    let indexed = as_str(repo_map, "indexed_hash");
    ArtifactFingerprint {
        generated_head: head.clone(),
        built_at: 0,
        fingerprint: format!("map-preview:{head}:{indexed}:{content}"),
    }
}

/// Read `repo_map.json`, render the preview, write it atomically.
///
/// Returns the output path. A missing or unreadable map is an error rather
/// than an empty page: "no subsystems" and "no map" are different answers and
/// the caller has to be able to tell them apart.
pub fn write_map_preview(repo_map_path: &Path, output: &Path) -> anyhow::Result<PathBuf> {
    let text = std::fs::read_to_string(repo_map_path).map_err(|err| {
        anyhow::anyhow!(
            "cannot read repo map at {}: {err} (run `devmap manifest` first)",
            repo_map_path.display()
        )
    })?;
    let repo_map: Value = serde_json::from_str(&text)
        .map_err(|err| anyhow::anyhow!("{} is not valid JSON: {err}", repo_map_path.display()))?;
    let fingerprint = fingerprint_for(&repo_map);
    let html = render_map_preview_html(&repo_map, &fingerprint);
    write_atomic(output, html.as_bytes())?;
    Ok(output.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_map() -> Value {
        json!({
            "generated_head": "abc1234",
            "languages": ["python", "rust"],
            "files": [
                {"path": "src/cli/main.py", "area": "src/cli", "kind": "module", "language": "python"},
                {"path": "src/cli/util.py", "area": "src/cli", "kind": "module", "language": "python"},
                {"path": "src/cli/run.sh", "area": "src/cli", "kind": "script", "language": "shell"},
                {"path": "crates/core/lib.rs", "area": "crates/core", "kind": "module", "language": "rust"},
                {"path": "docs/README.md", "area": "docs", "kind": "doc", "language": "markdown"},
            ],
            "subsystems": [
                {
                    "area": "src/cli",
                    "summary": "CLI surface",
                    "entry_points": ["src/cli/main.py"],
                    "critical_files": ["src/cli/main.py"],
                    "neighbors": ["crates/core"],
                    "handoff_paths": ["cli/main.py -> core/lib.rs"],
                    "role_files": {"entrypoints": ["src/cli/main.py"]},
                },
                {
                    "area": "crates/core",
                    "summary": "Core",
                    "entry_points": [],
                    "critical_files": [],
                    "neighbors": ["src/cli"],
                    "handoff_paths": [],
                    "role_files": {},
                },
            ],
            "dependents": {"src/cli/main.py": ["x"]},
            "entry_roots": ["src/cli/main.py"],
            "unwired_candidates": vec!["a.py"; 400],
            "unreachable_files": [],
            "dead_symbol_candidates": [],
            "liveness_unreachable_unreliable": false,
        })
    }

    fn fp() -> ArtifactFingerprint {
        ArtifactFingerprint {
            generated_head: "abc1234".into(),
            built_at: 1,
            fingerprint: "fp-test".into(),
        }
    }

    #[test]
    fn payload_carries_language_histograms_not_the_inventory() {
        let payload = build_preview_payload(&sample_map());
        // The inventory stays out: it is large and fully summarised by the
        // histograms derived from it.
        assert!(payload.get("files").is_none());
        assert!(payload.get("dependents").is_none());

        let subs = payload["subsystems"].as_array().unwrap();
        let cli = subs.iter().find(|s| s["area"] == "src/cli").unwrap();
        assert_eq!(cli["file_count"], 3);
        assert_eq!(cli["primary_language"], "python");
        assert_eq!(cli["languages"], json!([["python", 2], ["shell", 1]]));
    }

    #[test]
    fn every_bar_re_sums_to_the_total_beside_it() {
        // The 1486-vs-1503 bug: a capped ranking under-reports, and the page
        // draws the bar against a total it did not come from.
        let mut counts = BTreeMap::new();
        for i in 0..50 {
            counts.insert(format!("lang{i:02}"), i + 1);
        }
        let total: usize = counts.values().sum();
        let ranked = rank(&counts, REPO_LANG_CAP);
        assert_eq!(ranked.len(), REPO_LANG_CAP);
        assert_eq!(
            ranked.iter().map(|(_, n)| n).sum::<usize>(),
            total,
            "a capped ranking must fold its tail, not drop it"
        );
        assert!(ranked.iter().any(|(id, _)| id == "generic"));
    }

    #[test]
    fn a_file_lands_in_the_most_specific_subsystem() {
        let areas = vec!["vendor/g".to_string(), "vendor/g/inner".to_string()];
        let files = vec![
            json!({"path": "vendor/g/a.rs", "language": "rust"}),
            json!({"path": "vendor/g/inner/b.rs", "language": "rust"}),
            json!({"path": "elsewhere/c.rs", "language": "rust"}),
        ];
        let attribution = attribute_files_to_areas(&files, &areas);
        assert_eq!(attribution.by_area["vendor/g"]["rust"], 1);
        assert_eq!(attribution.by_area["vendor/g/inner"]["rust"], 1);
        assert_eq!(attribution.attributed, 2);
        // Repo totals count the unattributed file too, or the language bar
        // would describe only the covered slice.
        assert_eq!(attribution.repo_languages["rust"], 3);
    }

    #[test]
    fn coverage_reports_both_numbers() {
        let payload = build_preview_payload(&sample_map());
        let coverage = &payload["meta"]["coverage"];
        assert_eq!(coverage["indexed_total"], 5);
        assert_eq!(coverage["in_subsystems"], 4); // docs/ is not a subsystem
    }

    #[test]
    fn nodes_carry_official_linguist_colours() {
        let payload = build_preview_payload(&sample_map());
        let palette = &payload["meta"]["palette"];
        assert_eq!(palette["python"]["color"], "#3572A5");
        assert_eq!(palette["python"]["official"], true);
        assert_eq!(palette["rust"]["color"], "#dea584");
        assert_eq!(payload["meta"]["linguist_version"], LINGUIST_VERSION);

        let nodes = payload["nodes"].as_array().unwrap();
        let cli = nodes.iter().find(|n| n["area"] == "src/cli").unwrap();
        assert_eq!(cli["lang"], "python");
        assert_eq!(cli["entry"], true);
    }

    #[test]
    fn handoffs_and_neighbours_become_edges() {
        let payload = build_preview_payload(&sample_map());
        let links = payload["links"].as_array().unwrap();
        assert!(links.iter().any(|l| l["kind"] == "neighbor"));
        assert!(links
            .iter()
            .any(|l| l["kind"] == "handoff" && l["target"] == "crates/core"));
    }

    #[test]
    fn liveness_lists_are_capped() {
        let payload = build_preview_payload(&sample_map());
        assert_eq!(
            payload["liveness"]["unwired_candidates"]
                .as_array()
                .unwrap()
                .len(),
            LIVENESS_CAP
        );
    }

    #[test]
    fn hostile_map_content_cannot_break_out_of_the_script() {
        let mut map = sample_map();
        map["subsystems"][0]["summary"] = json!("</script><script>alert(1)</script>");
        let html = render_map_preview_html(&map, &fp());
        assert!(!html.contains("</script><script>"));
        assert!(html.contains("\\u003c"));
        // The payload still round-trips: escaping must not corrupt the JSON.
        let blob = html
            .split("const DATA = ")
            .nth(1)
            .unwrap()
            .split(";\n")
            .next()
            .unwrap();
        let parsed: Value = serde_json::from_str(blob).unwrap();
        assert_eq!(
            parsed["subsystems"][0]["summary"],
            "</script><script>alert(1)</script>"
        );
    }

    #[test]
    fn a_hostile_area_name_renders_inert() {
        // Closes V1, now against the live renderer. The old stub interpolated
        // subsystem names straight into markup and relied on `html_escape` at
        // each sink. This renderer interpolates *no* map content into HTML at
        // all — every name reaches the page inside the escaped JSON and is put
        // in the DOM by `escapeHtml` client-side — so the guarantee is
        // structural, and this test pins that it stays that way.
        let hostile = "x<img src=x onerror=alert(1)>.ts";
        let mut map = sample_map();
        map["subsystems"][0]["area"] = json!(hostile);
        map["files"][0]["path"] = json!(hostile);
        let html = render_map_preview_html(&map, &fp());
        // The guarantee is that no *tag* forms. The attribute text itself
        // survives as characters inside a JSON string — harmless, and asserting
        // its absence would only pin an escaping scheme that does not need one.
        assert!(!html.contains("<img"), "no raw tag from map content");
        assert!(
            html.contains("\\u003cimg"),
            "the angle brackets are what must be escaped"
        );
        // It survives, escaped, in the payload — inert but not lost.
        let blob = html
            .split("const DATA = ")
            .nth(1)
            .unwrap()
            .split(";\n")
            .next()
            .unwrap();
        let parsed: Value = serde_json::from_str(blob).unwrap();
        assert_eq!(parsed["subsystems"][0]["area"], hostile);
    }

    #[test]
    fn page_is_offline_and_self_contained() {
        let html = render_map_preview_html(&sample_map(), &fp());
        assert!(html.contains("DevCouncil Repo Map"));
        assert!(html.contains("vasturiano/force-graph"), "vendor inlined");
        assert!(!html.contains("cdn.jsdelivr"));
        assert!(!html.contains("unpkg.com"));
        assert!(!html.contains("src=\"http"));
        assert!(html.contains("graph/graph.html"));
        assert!(html.contains("fingerprint:fp-test"));
        // No placeholder survives substitution.
        assert!(!html.contains("__VENDOR_JS__"));
        assert!(!html.contains("__DATA_JSON__"));
        assert!(!html.contains("__FINGERPRINT__"));
    }

    #[test]
    fn an_empty_map_renders_without_claiming_coverage() {
        let payload = build_preview_payload(&json!({}));
        assert_eq!(payload["nodes"].as_array().unwrap().len(), 0);
        assert_eq!(payload["meta"]["coverage"]["indexed_total"], 0);
        assert_eq!(payload["meta"]["coverage"]["in_subsystems"], 0);
        let html = render_map_preview_html(&json!({}), &fp());
        assert!(html.contains("DevCouncil Repo Map"));
    }

    #[test]
    fn an_uncomputed_field_is_not_reported_as_empty() {
        // `handoff_paths: []` from a producer that never derived them must not
        // read as "nothing crosses a boundary".
        let stub = json!({
            "subsystems": [{"area": "a", "handoff_paths": [], "role_files": {}}],
        });
        let payload = build_preview_payload(&stub);
        assert_eq!(payload["meta"]["handoff_paths_computed"], false);
        assert_eq!(payload["meta"]["role_files_computed"], false);
        // And a map that demonstrably computed them says so.
        let payload = build_preview_payload(&sample_map());
        assert_eq!(payload["meta"]["handoff_paths_computed"], true);
        assert_eq!(payload["meta"]["role_files_computed"], true);
    }

    #[test]
    fn write_map_preview_reports_a_missing_map_rather_than_rendering_nothing() {
        let dir = std::env::temp_dir().join(format!("devmap-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("absent.json");
        let out = dir.join("map.html");
        let err = write_map_preview(&missing, &out).unwrap_err().to_string();
        assert!(err.contains("cannot read repo map"), "{err}");
        assert!(
            !out.exists(),
            "no page is written for a map that is not there"
        );

        let map_path = dir.join("repo_map.json");
        std::fs::write(&map_path, serde_json::to_string(&sample_map()).unwrap()).unwrap();
        let written = write_map_preview(&map_path, &out).unwrap();
        assert_eq!(written, out);
        assert!(std::fs::read_to_string(&out)
            .unwrap()
            .contains("DevCouncil Repo Map"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

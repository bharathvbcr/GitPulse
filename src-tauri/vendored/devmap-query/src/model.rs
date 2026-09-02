use serde::{Deserialize, Serialize};

pub struct Budget;

impl Budget {
    pub const SEARCH: u32 = 2000;
    pub const DEPS: u32 = 2000;
    pub const DEAD: u32 = 2000;
    pub const MANIFEST: u32 = 2000;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request<Q> {
    pub query: Q,
    pub token_budget: u32, // T1-T4: required token budget primitive
    pub min_confidence: f32,
    pub max_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolutionAvailability {
    Available,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response<T> {
    pub items: Vec<T>,
    pub shown: u32,
    pub hidden: u32,
    pub total: u32, // what a complete answer would have held
    pub truncated: bool,
    pub tokens_used: u32,
    pub resolution: ResolutionAvailability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolHit {
    pub symbol_name: String,
    pub file_path: String,
    pub kind: String,
    pub span: (u32, u32),
    pub source_span: String, // R2: verbatim source lines grouped by file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_unavailable_reason: Option<String>,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub subsystems: Vec<SubsystemEntry>,
    pub entry_roots: Vec<String>,
    pub important_files: Vec<String>,
    pub freshness: FreshnessInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubsystemEntry {
    pub name: String,
    pub path: String,
    pub entry_points: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshnessInfo {
    pub head_sha: String,
    pub generation_id: u32,
    pub pending_count: usize,
}

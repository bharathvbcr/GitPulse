use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadSymbolReport {
    pub symbol_name: String,
    pub file_path: String,
    pub confidence: f32,
    pub is_exempt: bool,
    pub exemption_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityReport {
    pub community_id: u32,
    pub name: String,
    pub members: Vec<String>,
    pub cohesion_score: f32, // R6: every community reports cohesion
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisSummary {
    pub total_files: usize,
    pub total_symbols: usize,
    pub total_edges: usize,
    pub dead_symbols: Vec<DeadSymbolReport>,
    pub communities: Vec<CommunityReport>,
    pub status: AnalysisStatus,
    /// Calls the resolution ladder could not attribute (R5 / D17).
    ///
    /// Persisted with the generation so a reader can tell "nothing calls this"
    /// apart from "we could not work out what this calls". `serde(default)`
    /// keeps pre-existing serialized summaries readable, where the field's
    /// absence honestly means "not recorded", not "zero".
    #[serde(default)]
    pub unresolved_calls: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnalysisStatus {
    Ok,
    Partial { reason: String },
    Timeout { reason: String },
}

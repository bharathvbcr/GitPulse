use super::lane_solver::VisualCommitRow;
use serde::{Deserialize, Serialize};

/// Packed metadata entry for memory-efficient viewport windowing.
/// Uses a compact fixed header (16 bytes) with extended lane spillover support for 64+ concurrent lanes.
///
/// Deliberate packing limit: the 16-byte representation is pinned by
/// `test_packed_struct_size`, so `lane_index`, `color_index`,
/// `parent_count` and `active_lane_count` saturate at 255 via `min(255)`
/// during [`CommitRowMetadata::from_visual_row`] — values beyond that are
/// lossy by design. Lanes ≥ 64 never widen `active_lanes_mask`; rows
/// carrying them set `has_extended_lanes` and spill those lanes into
/// [`TopologyIndex::extended_lanes`], which is the only place full-width
/// lane data survives.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRowMetadata {
    pub lane_index: u8,
    pub color_index: u8,
    pub parent_count: u8,
    pub is_merge: u8,
    pub is_root: u8,
    pub active_lane_count: u8,
    pub has_extended_lanes: u8,
    pub reserved_padding: u8,
    pub active_lanes_mask: u64,
}

impl CommitRowMetadata {
    pub fn from_visual_row(row: &VisualCommitRow) -> Self {
        let mut mask: u64 = 0;
        let mut has_extended = 0;

        for &lane in &row.active_lanes {
            if lane < 64 {
                mask |= 1 << lane;
            } else {
                has_extended = 1;
            }
        }

        Self {
            lane_index: row.lane.min(255) as u8,
            color_index: row.color_index.min(255) as u8,
            parent_count: row.parent_ids.len().min(255) as u8,
            is_merge: if row.is_merge { 1 } else { 0 },
            is_root: if row.is_root { 1 } else { 0 },
            active_lane_count: row.active_lanes.len().min(255) as u8,
            has_extended_lanes: has_extended,
            reserved_padding: 0,
            active_lanes_mask: mask,
        }
    }

    /// Packed-mask membership for lanes 0–63. Lanes ≥ 64 live only in
    /// [`TopologyIndex::extended_lanes`]; this method cannot name them and
    /// returns `false` so a row with *any* extended lane is not treated as
    /// active on *every* lane ≥ 64. Use [`TopologyIndex::is_lane_active_at`].
    pub fn is_lane_active(&self, lane_index: u32) -> bool {
        if lane_index < 64 {
            (self.active_lanes_mask & (1 << lane_index)) != 0
        } else {
            false
        }
    }
}

/// Compact in-memory topology index for random-access scrolling supporting unlimited commit histories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopologyIndex {
    pub rows: Vec<CommitRowMetadata>,
    pub extended_lanes: Vec<(usize, Vec<u32>)>,
    /// Direct row -> position-in-`extended_lanes` lookup so membership
    /// queries for lanes ≥ 64 do not linearly scan every spillover row.
    #[serde(default)]
    extended_by_row: std::collections::HashMap<usize, usize>,
    /// Rows where any packed field saturated at 255 (lane/color/parent/lane
    /// counts). Non-zero means the packed representation lost fidelity and
    /// the renderer should not pretend the layout is exact.
    #[serde(default)]
    pub lossy_rows: u64,
}

impl TopologyIndex {
    pub fn build(visual_rows: &[VisualCommitRow]) -> Self {
        let mut rows = Vec::with_capacity(visual_rows.len());
        let mut extended_lanes = Vec::new();
        let mut extended_by_row = std::collections::HashMap::new();
        let mut lossy_rows = 0u64;

        for (idx, vrow) in visual_rows.iter().enumerate() {
            let meta = CommitRowMetadata::from_visual_row(vrow);
            if meta.lane_index as u32 != vrow.lane
                || meta.color_index as u32 != vrow.color_index.min(255)
                || meta.parent_count as usize != vrow.parent_ids.len().min(255)
                || meta.active_lane_count as usize != vrow.active_lanes.len().min(255)
                || vrow.lane > 255
                || vrow.color_index > 255
                || vrow.parent_ids.len() > 255
                || vrow.active_lanes.len() > 255
            {
                lossy_rows += 1;
            }
            rows.push(meta);

            if meta.has_extended_lanes == 1 {
                let over_64: Vec<u32> = vrow
                    .active_lanes
                    .iter()
                    .copied()
                    .filter(|&l| l >= 64)
                    .collect();
                if !over_64.is_empty() {
                    extended_by_row.insert(idx, extended_lanes.len());
                    extended_lanes.push((idx, over_64));
                }
            }
        }

        Self {
            rows,
            extended_lanes,
            extended_by_row,
            lossy_rows,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn slice(&self, start: usize, count: usize) -> &[CommitRowMetadata] {
        if start >= self.rows.len() {
            return &[];
        }
        // Clamp first: `start + count` can overflow usize for hostile
        // viewport arguments, so the window length is bounded by what is
        // left after `start` before any addition happens.
        let take = count.min(self.rows.len() - start);
        &self.rows[start..start + take]
    }

    pub fn is_lane_active_at(&self, row_idx: usize, lane_index: u32) -> bool {
        if let Some(row) = self.rows.get(row_idx) {
            if lane_index < 64 {
                (row.active_lanes_mask & (1 << lane_index)) != 0
            } else if row.has_extended_lanes == 1 {
                self.extended_by_row
                    .get(&row_idx)
                    .and_then(|&pos| self.extended_lanes.get(pos))
                    .is_some_and(|(_, lanes)| lanes.contains(&lane_index))
            } else {
                false
            }
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packed_struct_size() {
        assert_eq!(std::mem::size_of::<CommitRowMetadata>(), 16);
    }

    #[test]
    fn test_extended_lanes_beyond_64() {
        let visual_row = VisualCommitRow {
            id: "commit_wide".to_string(),
            parent_ids: vec!["p1".to_string()],
            summary: "Wide merge commit".to_string(),
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            timestamp: 1700000000,
            lane: 72,
            color_index: 5,
            active_lanes: vec![0, 3, 63, 64, 72, 100],
            active_lane_colors: vec![0, 1, 2, 3, 4, 5],
            connections: vec![],
            is_merge: false,
            is_root: false,
        };

        let index = TopologyIndex::build(&[visual_row]);
        assert_eq!(index.len(), 1);
        assert_eq!(index.rows[0].has_extended_lanes, 1);
        assert!(index.is_lane_active_at(0, 0));
        assert!(index.is_lane_active_at(0, 63));
        assert!(index.is_lane_active_at(0, 64));
        assert!(index.is_lane_active_at(0, 72));
        assert!(index.is_lane_active_at(0, 100));
        assert!(!index.is_lane_active_at(0, 1));
        assert!(!index.is_lane_active_at(0, 75));
    }

    #[test]
    fn test_slice_clamps_hostile_windows_instead_of_overflowing() {
        let visual_rows: Vec<VisualCommitRow> = (0..5)
            .map(|i| VisualCommitRow {
                id: format!("c{i}"),
                parent_ids: vec![],
                summary: "s".to_string(),
                author_name: "Dev".to_string(),
                author_email: "dev@example.com".to_string(),
                timestamp: 1700000000,
                lane: 0,
                color_index: 0,
                active_lanes: vec![0],
                active_lane_colors: vec![0],
                connections: vec![],
                is_merge: false,
                is_root: false,
            })
            .collect();
        let index = TopologyIndex::build(&visual_rows);

        assert!(index.slice(usize::MAX, 1).is_empty(), "start past end");
        assert!(index.slice(index.len(), 10).is_empty(), "start == len");
        assert_eq!(
            index.slice(0, usize::MAX).len(),
            index.len(),
            "count overflow must clamp to the remaining rows"
        );
        assert_eq!(
            index.slice(3, usize::MAX).len(),
            2,
            "count overflow mid-index must clamp to rows.len() - start"
        );
        assert_eq!(index.slice(1, 2).len(), 2, "in-range window unchanged");
    }

    fn wide_row(id: &str, lane: u32, lanes: &[u32]) -> VisualCommitRow {
        VisualCommitRow {
            id: id.to_string(),
            parent_ids: vec![],
            summary: "s".to_string(),
            author_name: "Dev".to_string(),
            author_email: "dev@example.com".to_string(),
            timestamp: 1700000000,
            lane,
            color_index: lane.min(255),
            active_lanes: lanes.to_vec(),
            active_lane_colors: lanes.iter().map(|l| (*l).min(255)).collect(),
            connections: vec![],
            is_merge: false,
            is_root: false,
        }
    }

    /// Regression (audit M5a): saturation at 255 used to be silent, so a
    /// pathological topology silently rendered overlapping edges with no
    /// signal that fidelity was lost.
    #[test]
    fn build_counts_rows_that_lost_fidelity_to_saturation() {
        let mut rows = vec![
            wide_row("normal", 3, &[0, 1, 2]),
            wide_row("huge", 300, &[0, 64]),
            wide_row("fine", 7, &[7]),
        ];
        // parent_count also saturates at 255.
        rows[2].parent_ids = (0..300).map(|i| format!("p{i}")).collect();
        let index = TopologyIndex::build(&rows);
        assert_eq!(index.lossy_rows, 2, "rows 1 and 2 saturated fields");
        let clean = TopologyIndex::build(&rows[..1]);
        assert_eq!(clean.lossy_rows, 0);
    }

    /// Regression (audit M5b): extended-lane membership was resolved by a
    /// linear scan over every spillover row on each query; interleaved wide
    /// rows must all resolve correctly through the direct lookup.
    #[test]
    fn extended_lane_queries_stay_correct_with_interleaved_wide_rows() {
        let rows = vec![
            wide_row("a", 70, &[0, 70]),
            wide_row("b", 1, &[1]),
            wide_row("c", 90, &[90]),
            wide_row("d", 200, &[63, 200]),
        ];
        let index = TopologyIndex::build(&rows);
        assert!(index.is_lane_active_at(0, 70));
        assert!(!index.is_lane_active_at(0, 90));
        assert!(index.is_lane_active_at(2, 90));
        assert!(!index.is_lane_active_at(2, 70));
        assert!(index.is_lane_active_at(3, 200));
        assert!(index.is_lane_active_at(3, 63));
        assert!(!index.is_lane_active_at(1, 70));
        assert!(!index.is_lane_active_at(4, 70), "row past end");

        // Packed metadata cannot name which extended lanes are live: a row
        // with lane 70 active must not report every lane ≥ 64 as active.
        let packed = index.rows[0];
        assert!(packed.is_lane_active(0));
        assert!(!packed.is_lane_active(1));
        assert!(!packed.is_lane_active(70), "false-positive extended lane");
        assert!(!packed.is_lane_active(90));
        assert!(index.is_lane_active_at(0, 70));
        assert!(!index.is_lane_active_at(0, 90));
    }
}

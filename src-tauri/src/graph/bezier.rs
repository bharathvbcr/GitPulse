use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point2D {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CubicBezierCurve {
    pub start: Point2D,
    pub cp1: Point2D,
    pub cp2: Point2D,
    pub end: Point2D,
    pub color_index: u32,
    pub is_merge: bool,
}

/// Computes cubic Bézier curve control points with vertical tangency constraints
/// for smooth connection lines between commit rows in a 2D canvas.
pub struct BezierGeometryCalculator {
    pub lane_width: f64,
    pub row_height: f64,
    pub x_offset: f64,
    pub y_offset: f64,
}

impl BezierGeometryCalculator {
    pub fn new(lane_width: f64, row_height: f64, x_offset: f64, y_offset: f64) -> Self {
        Self {
            lane_width: if lane_width > 0.0 { lane_width } else { 16.0 },
            row_height: if row_height > 0.0 { row_height } else { 24.0 },
            x_offset: if x_offset >= 0.0 { x_offset } else { 12.0 },
            y_offset: if y_offset >= 0.0 { y_offset } else { 12.0 },
        }
    }

    pub fn commit_center(&self, lane: u32, row: usize) -> Point2D {
        Point2D {
            x: self.x_offset + (lane as f64) * self.lane_width,
            y: self.y_offset + (row as f64) * self.row_height,
        }
    }

    /// Generates cubic Bézier curve coordinates connecting a child row to its parent row.
    pub fn calculate_connector(
        &self,
        from_lane: u32,
        from_row: usize,
        to_lane: u32,
        to_row: usize,
        color_index: u32,
        is_merge: bool,
    ) -> CubicBezierCurve {
        let start = self.commit_center(from_lane, from_row);
        let end = self.commit_center(to_lane, to_row);

        let delta_y = end.y - start.y;
        let control_offset_y = delta_y * 0.5;

        // Vertical tangency at both start and end ensures connections enter/exit nodes vertically
        let cp1 = Point2D {
            x: start.x,
            y: start.y + control_offset_y,
        };
        let cp2 = Point2D {
            x: end.x,
            y: end.y - control_offset_y,
        };

        CubicBezierCurve {
            start,
            cp1,
            cp2,
            end,
            color_index,
            is_merge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bezier_tangency() {
        let calc = BezierGeometryCalculator::new(20.0, 30.0, 10.0, 15.0);
        let curve = calc.calculate_connector(0, 0, 2, 1, 1, true);

        assert_eq!(curve.start.x, 10.0);
        assert_eq!(curve.start.y, 15.0);
        assert_eq!(curve.end.x, 50.0);
        assert_eq!(curve.end.y, 45.0);

        // Control points should align vertically with start and end
        assert_eq!(curve.cp1.x, curve.start.x);
        assert_eq!(curve.cp2.x, curve.end.x);
        assert_eq!(curve.cp1.y, 30.0); // 15 + 15
        assert_eq!(curve.cp2.y, 30.0); // 45 - 15
    }
}

//! Wrapping a sequence into rows and packing feature segments into lanes per row.
//! Pure geometry in base units; the editor converts to pixels.

use crate::model::Feature;

/// A slice of one feature segment as it appears on one row.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub feature: usize,
    pub start: usize,
    pub end: usize,
    pub lane: usize,
    /// This piece holds the feature's 3' end, so it gets the arrowhead.
    pub head: bool,
    /// This piece holds the feature's first appearance, so it gets the label.
    pub labeled: bool,
}

pub struct Layout {
    pub bases_per_row: usize,
    pub rows: usize,
    /// Feature lanes used by each row; rows have variable height.
    pub row_lanes: Vec<usize>,
    pub pieces: Vec<Vec<Piece>>,
}

/// Round to a multiple of 10 so ruler ticks land at the same column on every row.
pub fn bases_per_row(width_px: f32, char_w: f32) -> usize {
    if width_px <= 0.0 || char_w <= 0.0 {
        return 100;
    }
    let fit = (width_px / char_w).floor() as usize;
    (fit / 10 * 10).max(10)
}

pub fn compute(len: usize, features: &[Feature], bases_per_row: usize) -> Layout {
    let rows = len.div_ceil(bases_per_row).max(1);
    let mut pieces: Vec<Vec<Piece>> = vec![Vec::new(); rows];

    for (fi, f) in features.iter().enumerate() {
        // On the reverse strand the 3' end is the leftmost segment start.
        let head_seg = if f.strand < 0 { 0 } else { f.segments.len() - 1 };
        let mut first = true;
        for (si, &(s, e)) in f.segments.iter().enumerate() {
            let (first_row, last_row) = (s / bases_per_row, (e - 1) / bases_per_row);
            for row in first_row..=last_row.min(rows - 1) {
                let rs = row * bases_per_row;
                let (ps, pe) = (s.max(rs), e.min(rs + bases_per_row));
                let head = si == head_seg && if f.strand < 0 { ps == s } else { pe == e };
                pieces[row].push(Piece { feature: fi, start: ps, end: pe, lane: 0, head: head && f.strand != 0, labeled: first });
                first = false;
            }
        }
    }

    let mut row_lanes = Vec::with_capacity(rows);
    for row in pieces.iter_mut() {
        // Longest first gives big features the top lanes, like Benchling.
        row.sort_by_key(|p| (p.start, std::cmp::Reverse(p.end - p.start)));
        let mut lane_ends: Vec<usize> = Vec::new();
        for p in row.iter_mut() {
            p.lane = match lane_ends.iter().position(|&end| end <= p.start) {
                Some(l) => l,
                None => {
                    lane_ends.push(0);
                    lane_ends.len() - 1
                }
            };
            lane_ends[p.lane] = p.end;
        }
        row_lanes.push(lane_ends.len());
    }

    Layout { bases_per_row, rows, row_lanes, pieces }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat(segs: Vec<(usize, usize)>, strand: i8) -> Feature {
        Feature { kind: "CDS".into(), label: "x".into(), segments: segs, strand, source: None }
    }

    #[test]
    fn rounds_row_width_to_tens() {
        assert_eq!(bases_per_row(1000.0, 8.0), 120);
        assert_eq!(bases_per_row(50.0, 8.0), 10);
    }

    #[test]
    fn splits_features_across_rows_and_packs_lanes() {
        let feats = vec![feat(vec![(5, 25)], 1), feat(vec![(8, 12)], -1), feat(vec![(12, 15)], 1)];
        let l = compute(40, &feats, 10);
        assert_eq!(l.rows, 4);
        assert_eq!(l.pieces[0].len(), 2);
        assert_eq!(l.pieces[1].iter().map(|p| (p.feature, p.lane)).collect::<Vec<_>>(), vec![(0, 0), (1, 1), (2, 1)]);
        assert_eq!(l.row_lanes, vec![2, 2, 1, 0]);
        let f0: Vec<_> = l.pieces.iter().flatten().filter(|p| p.feature == 0).collect();
        assert_eq!(f0.iter().map(|p| p.head).collect::<Vec<_>>(), vec![false, false, true]);
        let f1: Vec<_> = l.pieces.iter().flatten().filter(|p| p.feature == 1).collect();
        assert_eq!(f1.iter().map(|p| p.head).collect::<Vec<_>>(), vec![true, false]);
    }
}

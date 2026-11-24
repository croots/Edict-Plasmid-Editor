//! Placing SBOL glyphs along a linear or circular backbone.
//!
//! Pure geometry in pixels along the backbone (`u`), no UI. Glyphs that would
//! overlap are pushed into lanes further from the backbone: forward-strand
//! features above (outside the circle), reverse-strand ones below (inside).
//! Lane 0 sits on the backbone and is shared, because centered glyphs such as a
//! CDS straddle it and reach into both sides.

use crate::model::Feature;
use crate::sbol::{Align, Glyph, Sizing};

#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub feature: usize,
    pub glyph: Glyph,
    pub reverse: bool,
    /// Feature extent in bases, `start < end`. On a circular map `end` may exceed the
    /// sequence length when the feature wraps the origin.
    pub start: usize,
    pub end: usize,
    /// The glyph's left edge and width along the backbone.
    pub u0: f32,
    pub w: f32,
    /// The feature's real extent along the backbone (may be narrower than the glyph).
    pub span: (f32, f32),
    pub lane: usize,
}

impl Placed {
    pub fn center(&self) -> f32 {
        self.u0 + self.w / 2.0
    }
}

pub struct Placement {
    pub items: Vec<Placed>,
    /// Lanes used above (forward) and below (reverse) the backbone, at least 1 each.
    pub fwd_lanes: usize,
    pub rev_lanes: usize,
}

/// The smallest extent covering every segment. On a circular sequence that is the
/// complement of the largest gap between segments, which handles origin-spanning
/// features (stored as `[(0, a), (b, len)]`).
pub fn feature_extent(segments: &[(usize, usize)], len: usize, circular: bool) -> (usize, usize) {
    let start = segments.iter().map(|s| s.0).min().unwrap_or(0);
    let end = segments.iter().map(|s| s.1).max().unwrap_or(0);
    if !circular || segments.len() < 2 {
        return (start, end);
    }
    let mut segs = segments.to_vec();
    segs.sort_unstable();
    let mut best_gap = len - segs[segs.len() - 1].1 + segs[0].0; // across the origin
    let mut extent = (segs[0].0, segs[segs.len() - 1].1);
    for i in 0..segs.len() - 1 {
        let gap = segs[i + 1].0.saturating_sub(segs[i].1);
        if gap > best_gap {
            best_gap = gap;
            extent = (segs[i + 1].0, segs[i].1 + len);
        }
    }
    extent
}

/// Glyph left edge and width for a feature spanning `[a, b)` pixels.
fn glyph_box(glyph: Glyph, a: f32, b: f32, reverse: bool) -> (f32, f32) {
    let span = b - a;
    match glyph.sizing() {
        Sizing::Span { min } => {
            let w = span.max(min);
            ((a + b) / 2.0 - w / 2.0, w)
        }
        Sizing::FromStart { min } => {
            let w = span.max(min);
            if reverse { (b - w, w) } else { (a, w) }
        }
        Sizing::Point { min, max } => {
            let w = span.clamp(min, max);
            ((a + b) / 2.0 - w / 2.0, w)
        }
    }
}

struct Lanes {
    circumference: Option<f32>,
    /// `[side][lane]` occupied intervals; side 0 forward, 1 reverse.
    sides: [Vec<Vec<(f32, f32)>>; 2],
}

impl Lanes {
    fn overlaps(&self, a: (f32, f32), b: (f32, f32)) -> bool {
        let hit = |s: f32| a.0 < b.1 + s && b.0 + s < a.1;
        match self.circumference {
            Some(c) => hit(-c) || hit(0.0) || hit(c),
            None => hit(0.0),
        }
    }

    fn free(&self, side: usize, lane: usize, iv: (f32, f32)) -> bool {
        self.sides[side].get(lane).is_none_or(|l| l.iter().all(|&o| !self.overlaps(o, iv)))
    }

    fn take(&mut self, side: usize, lane: usize, iv: (f32, f32)) {
        let lanes = &mut self.sides[side];
        if lanes.len() <= lane {
            lanes.resize(lane + 1, Vec::new());
        }
        lanes[lane].push(iv);
    }
}

/// Place every feature. `px_per_bp` scales bases to pixels along the backbone;
/// `circumference` is `Some` on a circular map so lanes wrap around. `label_w`
/// is the label width a feature reserves beside its glyph (0 for none) and `gap`
/// the minimum spacing between neighbours.
pub fn place(
    features: &[Feature],
    glyphs: &[Option<Glyph>],
    len: usize,
    px_per_bp: f32,
    circumference: Option<f32>,
    label_w: impl Fn(usize) -> f32,
    gap: f32,
) -> Placement {
    let mut items: Vec<Placed> = features
        .iter()
        .enumerate()
        .filter(|(i, f)| glyphs[*i].is_some() && !f.segments.is_empty())
        .map(|(i, f)| {
            let glyph = glyphs[i].expect("filtered");
            let (start, end) = feature_extent(&f.segments, len, circumference.is_some());
            let reverse = f.strand < 0;
            let span = (start as f32 * px_per_bp, end as f32 * px_per_bp);
            let (u0, w) = glyph_box(glyph, span.0, span.1, reverse);
            Placed { feature: i, glyph, reverse, start, end, u0, w, span, lane: 0 }
        })
        .collect();

    // Widest first: big genes hug the backbone, small sites stack outside them.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| items[b].w.total_cmp(&items[a].w).then(items[a].u0.total_cmp(&items[b].u0)));

    let mut lanes = Lanes { circumference, sides: [Vec::new(), Vec::new()] };
    for ix in order {
        let it = &items[ix];
        let side = it.reverse as usize;
        let c = it.center();
        let lw = label_w(it.feature);
        let glyph_iv = (it.u0 - gap / 2.0, it.u0 + it.w + gap / 2.0);
        let full_iv = (glyph_iv.0.min(c - lw / 2.0 - gap / 2.0), glyph_iv.1.max(c + lw / 2.0 + gap / 2.0));
        let centered = it.glyph.align() == Align::Centered;
        let mut lane = 0;
        loop {
            let ok = if lane == 0 && centered {
                lanes.free(side, 0, full_iv) && lanes.free(1 - side, 0, glyph_iv)
            } else {
                lanes.free(side, lane, full_iv)
            };
            if ok {
                break;
            }
            lane += 1;
        }
        lanes.take(side, lane, full_iv);
        if lane == 0 && centered {
            lanes.take(1 - side, 0, glyph_iv);
        }
        items[ix].lane = lane;
    }

    let fwd_lanes = lanes.sides[0].len().max(1);
    let rev_lanes = lanes.sides[1].len().max(1);
    Placement { items, fwd_lanes, rev_lanes }
}

/// A "nice" tick spacing (1, 2 or 5 × 10ⁿ bases) giving roughly `target` ticks.
pub fn tick_step(len: usize, target: usize) -> usize {
    let raw = (len as f64 / target.max(1) as f64).max(1.0);
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|&s| s >= raw).unwrap_or(10.0 * mag);
    step as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat(kind: &str, segments: Vec<(usize, usize)>, strand: i8) -> Feature {
        Feature { kind: kind.into(), label: kind.into(), segments, strand, source: None }
    }

    fn glyphs(fs: &[Feature]) -> Vec<Option<Glyph>> {
        fs.iter().map(|f| Some(Glyph::classify(&f.kind, None))).collect()
    }

    #[test]
    fn extent_of_origin_spanning_feature() {
        assert_eq!(feature_extent(&[(0, 100), (900, 1000)], 1000, true), (900, 1100));
        assert_eq!(feature_extent(&[(0, 100), (900, 1000)], 1000, false), (0, 1000));
        // A spliced gene keeps its natural extent.
        assert_eq!(feature_extent(&[(100, 200), (300, 400)], 1000, true), (100, 400));
    }

    #[test]
    fn overlapping_cds_stack_on_their_own_strand() {
        let fs = vec![feat("CDS", vec![(0, 500)], 1), feat("CDS", vec![(100, 300)], 1), feat("CDS", vec![(600, 900)], 1)];
        let p = place(&fs, &glyphs(&fs), 1000, 1.0, None, |_| 0.0, 2.0);
        let lane = |i: usize| p.items.iter().find(|it| it.feature == i).unwrap().lane;
        assert_eq!((lane(0), lane(1), lane(2)), (0, 1, 0));
        assert_eq!(p.fwd_lanes, 2);
    }

    #[test]
    fn centered_glyphs_share_the_backbone_across_strands() {
        // Forward and reverse CDS over the same bases cannot both straddle the backbone.
        let fs = vec![feat("CDS", vec![(0, 500)], 1), feat("CDS", vec![(100, 400)], -1)];
        let p = place(&fs, &glyphs(&fs), 1000, 1.0, None, |_| 0.0, 2.0);
        assert_eq!(p.items[0].lane, 0);
        assert_eq!(p.items[1].lane, 1);
        assert!(p.items[1].reverse);
        // A promoter sits above the backbone, so a reverse CDS beneath it is fine.
        let fs = vec![feat("promoter", vec![(0, 50)], 1), feat("terminator", vec![(0, 50)], -1)];
        let p = place(&fs, &glyphs(&fs), 1000, 1.0, None, |_| 0.0, 2.0);
        assert_eq!((p.items[0].lane, p.items[1].lane), (0, 0));
    }

    #[test]
    fn circular_lanes_wrap_around_the_origin() {
        let fs = vec![feat("CDS", vec![(0, 100), (900, 1000)], 1), feat("CDS", vec![(20, 60)], 1)];
        let p = place(&fs, &glyphs(&fs), 1000, 1.0, Some(1000.0), |_| 0.0, 2.0);
        assert_eq!((p.items[0].start, p.items[0].end), (900, 1100));
        assert_eq!(p.items[1].lane, 1, "the wrapped CDS covers bases 20-60");
    }

    #[test]
    fn labels_reserve_room() {
        let fs = vec![feat("CDS", vec![(0, 10)], 1), feat("CDS", vec![(20, 30)], 1)];
        let p = place(&fs, &glyphs(&fs), 1000, 1.0, None, |_| 0.0, 2.0);
        assert_eq!((p.items[0].lane, p.items[1].lane), (0, 0));
        let p = place(&fs, &glyphs(&fs), 1000, 1.0, None, |_| 40.0, 2.0);
        assert_ne!(p.items[0].lane, p.items[1].lane);
    }

    #[test]
    fn nice_ticks() {
        assert_eq!(tick_step(2686, 10), 500);
        assert_eq!(tick_step(48502, 10), 5000);
        assert_eq!(tick_step(100, 10), 10);
        assert_eq!(tick_step(5, 10), 1);
    }
}

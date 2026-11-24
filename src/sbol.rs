//! SBOL Visual glyphs (https://sbolstandard.org/visual-glyphs/) for sequence features.
//!
//! Pure geometry, no UI. A glyph is drawn in a local frame: `x` runs along the
//! backbone from the feature's 5' end (0) to its 3' end (`w`), and `y` points away
//! from the backbone. The map view mirrors both axes for reverse-strand features,
//! which is the SBOL convention (the glyph rotated 180° below the backbone), and
//! bends the frame around the circle for circular maps.

/// The glyphs we draw. Kinds without an SBOL equivalent use `NoGlyph`, the
/// spec's "no glyph assigned" dashed box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Cds,
    Promoter,
    Rbs,
    Terminator,
    Origin,
    OriginOfTransfer,
    PrimerBindingSite,
    Operator,
    Insulator,
    RecombinationSite,
    PolyA,
    NcRna,
    EngineeredRegion,
    NoGlyph,
}

/// How a glyph sits relative to the backbone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// Bottom edge on the backbone (promoter, RBS, terminator…).
    Baseline,
    /// Vertically centered on the backbone (CDS, origin, operator…).
    Centered,
}

/// How wide a glyph is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sizing {
    /// Stretches over the feature, but never narrower than `min`.
    Span { min: f32 },
    /// Starts at the feature's 5' end and stretches to its 3' end (a promoter's arrow).
    FromStart { min: f32 },
    /// Fixed width centered on the feature, clamped to `[min, max]` of its span.
    Point { min: f32, max: f32 },
}

impl Glyph {
    /// `kind` is the GenBank feature key; `regulatory_class` is the qualifier that
    /// INSDC 2014+ files use in place of the retired `promoter`/`RBS`/`terminator` keys.
    /// Feature kinds that annotate the sequence record (edits, errata, polymorphisms)
    /// rather than describe a part. They have no SBOL glyph and are left off the map.
    pub fn is_annotation(kind: &str) -> bool {
        matches!(
            kind,
            "variation" | "misc_difference" | "conflict" | "old_sequence" | "unsure" | "modified_base" | "gap" | "STS"
        )
    }

    pub fn classify(kind: &str, regulatory_class: Option<&str>) -> Glyph {
        let key = if kind == "regulatory" { regulatory_class.unwrap_or("other") } else { kind };
        match key {
            "CDS" | "gene" => Glyph::Cds,
            "promoter" | "minus_10_signal" | "minus_35_signal" => Glyph::Promoter,
            "RBS" | "ribosome_binding_site" => Glyph::Rbs,
            "terminator" => Glyph::Terminator,
            "rep_origin" | "origin" | "ori" => Glyph::Origin,
            "oriT" => Glyph::OriginOfTransfer,
            "primer_bind" | "primer" => Glyph::PrimerBindingSite,
            "protein_bind" | "misc_binding" | "operator" => Glyph::Operator,
            "insulator" => Glyph::Insulator,
            "misc_recomb" | "LTR" | "recombination_site" => Glyph::RecombinationSite,
            "polyA_signal" | "polyA_site" | "polyA_signal_sequence" => Glyph::PolyA,
            "ncRNA" | "misc_RNA" | "tRNA" | "rRNA" | "tmRNA" | "mRNA" | "precursor_RNA" => Glyph::NcRna,
            "misc_feature" | "mobile_element" | "repeat_region" | "enhancer" | "sig_peptide" | "mat_peptide"
            | "transit_peptide" | "intron" | "exon" => Glyph::EngineeredRegion,
            _ => Glyph::NoGlyph,
        }
    }

    pub fn align(self) -> Align {
        match self {
            Glyph::Promoter | Glyph::Rbs | Glyph::Terminator | Glyph::PrimerBindingSite | Glyph::PolyA => Align::Baseline,
            _ => Align::Centered,
        }
    }

    pub fn sizing(self) -> Sizing {
        match self {
            Glyph::Cds | Glyph::EngineeredRegion | Glyph::NcRna | Glyph::NoGlyph => Sizing::Span { min: 6.0 },
            Glyph::PrimerBindingSite => Sizing::Span { min: 10.0 },
            Glyph::Promoter => Sizing::FromStart { min: 14.0 },
            Glyph::Rbs => Sizing::Point { min: 12.0, max: 18.0 },
            Glyph::Terminator => Sizing::Point { min: 12.0, max: 12.0 },
            Glyph::PolyA => Sizing::Point { min: 17.0, max: 17.0 },
            Glyph::Origin | Glyph::OriginOfTransfer | Glyph::Insulator | Glyph::RecombinationSite => {
                Sizing::Point { min: 14.0, max: 14.0 }
            }
            Glyph::Operator => Sizing::Point { min: 11.0, max: 11.0 },
        }
    }

    /// The feature key whose theme color this glyph uses.
    pub fn color_kind(self) -> &'static str {
        match self {
            Glyph::Cds => "CDS",
            Glyph::Promoter => "promoter",
            Glyph::Rbs => "RBS",
            Glyph::Terminator | Glyph::PolyA | Glyph::Insulator => "terminator",
            Glyph::Origin | Glyph::OriginOfTransfer => "rep_origin",
            Glyph::PrimerBindingSite => "primer_bind",
            Glyph::Operator => "protein_bind",
            Glyph::NcRna => "ncRNA",
            Glyph::RecombinationSite | Glyph::EngineeredRegion | Glyph::NoGlyph => "misc_feature",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Glyph::Cds => "CDS",
            Glyph::Promoter => "promoter",
            Glyph::Rbs => "ribosome binding site",
            Glyph::Terminator => "terminator",
            Glyph::Origin => "origin of replication",
            Glyph::OriginOfTransfer => "origin of transfer",
            Glyph::PrimerBindingSite => "primer binding site",
            Glyph::Operator => "operator",
            Glyph::Insulator => "insulator",
            Glyph::RecombinationSite => "recombination site",
            Glyph::PolyA => "polyA site",
            Glyph::NcRna => "non-coding RNA gene",
            Glyph::EngineeredRegion => "engineered region",
            Glyph::NoGlyph => "no glyph assigned",
        }
    }

    /// Lowest and highest `y` the glyph reaches, for a glyph of height `h`.
    pub fn y_range(self, h: f32) -> (f32, f32) {
        match self.align() {
            Align::Baseline => (0.0, h),
            Align::Centered => (-h / 2.0, h / 2.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    /// Filled with the feature color and outlined.
    Outline,
    /// Filled with the feature color only (arrowheads).
    Fill,
    /// A line in the feature color.
    Stroke,
    /// A dashed outline ("no glyph assigned").
    Dashed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub points: Vec<(f32, f32)>,
    pub closed: bool,
    pub paint: Paint,
}

fn shape(points: Vec<(f32, f32)>, closed: bool, paint: Paint) -> Shape {
    Shape { points, closed, paint }
}

fn circle(cx: f32, cy: f32, r: f32, from: f32, to: f32, steps: usize) -> Vec<(f32, f32)> {
    (0..=steps)
        .map(|i| {
            let a = from + (to - from) * i as f32 / steps as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn arrowhead(tip: (f32, f32), size: f32) -> Shape {
    let (x, y) = tip;
    shape(vec![(x - size, y - size * 0.7), (x, y), (x - size, y + size * 0.7)], true, Paint::Fill)
}

/// The outlines of `glyph` drawn `w` wide and `h` tall, pointing 5'→3' along +x.
pub fn shapes(glyph: Glyph, w: f32, h: f32) -> Vec<Shape> {
    use std::f32::consts::{PI, TAU};
    let mid = w / 2.0;
    match glyph {
        Glyph::Cds => {
            let head = (h * 0.55).min(w * 0.5);
            let y = h / 2.0;
            vec![shape(vec![(0.0, -y), (w - head, -y), (w, 0.0), (w - head, y), (0.0, y)], true, Paint::Outline)]
        }
        Glyph::EngineeredRegion => {
            let y = h / 2.0;
            vec![shape(vec![(0.0, -y), (w, -y), (w, y), (0.0, y)], true, Paint::Outline)]
        }
        Glyph::NoGlyph => {
            let y = h * 0.4;
            vec![shape(vec![(0.0, -y), (w, -y), (w, y), (0.0, y)], true, Paint::Dashed)]
        }
        Glyph::NcRna => {
            // A shorter gene arrow with a wavy back: SBOL's "non-coding RNA gene".
            let head = (h * 0.45).min(w * 0.5);
            let y = h * 0.35;
            let body = w - head;
            let waves = (body / 8.0).ceil().max(1.0);
            let steps = (waves * 8.0) as usize;
            let mut pts = vec![(0.0, -y)];
            pts.extend([(body, -y), (w, 0.0), (body, y)]);
            pts.extend((0..=steps).map(|i| {
                let x = body - body * i as f32 / steps as f32;
                (x, y + 2.0 * (x / body * waves * TAU).sin())
            }));
            vec![shape(pts, true, Paint::Outline)]
        }
        Glyph::Promoter => {
            let top = h * 0.72;
            let a = 4.5f32.min(w * 0.4);
            vec![
                shape(vec![(0.0, 0.0), (0.0, top), (w - a * 0.5, top)], false, Paint::Stroke),
                arrowhead((w, top), a),
            ]
        }
        Glyph::Rbs => {
            let r = (w / 2.0).min(h * 0.45);
            vec![shape(circle(mid, 0.0, r, 0.0, PI, 16), true, Paint::Outline)]
        }
        Glyph::Terminator => {
            let top = h * 0.75;
            vec![
                shape(vec![(mid, 0.0), (mid, top)], false, Paint::Stroke),
                shape(vec![(0.0, top), (w, top)], false, Paint::Stroke),
            ]
        }
        Glyph::Origin => {
            let r = (w / 2.0).min(h * 0.4);
            vec![shape(circle(mid, 0.0, r, 0.0, TAU, 24), true, Paint::Outline)]
        }
        Glyph::OriginOfTransfer => {
            let r = (w / 2.0).min(h * 0.25);
            let top = r + 2.5;
            vec![
                shape(circle(mid, 0.0, r, 0.0, TAU, 24), true, Paint::Outline),
                shape(vec![(mid, r), (mid, top), (w - 1.5, top)], false, Paint::Stroke),
                arrowhead((w, top), 2.5),
            ]
        }
        Glyph::PrimerBindingSite => {
            let y = h * 0.35;
            let barb = 5.0f32.min(w * 0.5);
            vec![shape(vec![(0.0, y), (w, y), (w - barb, y + 4.0)], false, Paint::Stroke)]
        }
        Glyph::Operator => {
            let s = w.min(h * 0.7) / 2.0;
            vec![shape(vec![(mid - s, -s), (mid + s, -s), (mid + s, s), (mid - s, s)], true, Paint::Outline)]
        }
        Glyph::Insulator => {
            let s = w.min(h * 0.8) / 2.0;
            let i = s * 0.5;
            vec![
                shape(vec![(mid - s, -s), (mid + s, -s), (mid + s, s), (mid - s, s)], true, Paint::Stroke),
                shape(vec![(mid - i, -i), (mid + i, -i), (mid + i, i), (mid - i, i)], true, Paint::Outline),
            ]
        }
        Glyph::RecombinationSite => {
            let s = w.min(h * 0.75) / 2.0;
            vec![shape(vec![(mid - s, -s), (mid + s, 0.0), (mid - s, s)], true, Paint::Outline)]
        }
        Glyph::PolyA => {
            // Three letter A's standing on the backbone.
            let lw = w / 3.0;
            let lh = h * 0.6;
            let mut out = Vec::new();
            for i in 0..3 {
                let x = i as f32 * lw + 0.5;
                let aw = lw - 1.0;
                out.push(shape(vec![(x, 0.0), (x + aw / 2.0, lh), (x + aw, 0.0)], false, Paint::Stroke));
                out.push(shape(vec![(x + aw * 0.25, lh * 0.45), (x + aw * 0.75, lh * 0.45)], false, Paint::Stroke));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_genbank_keys_and_regulatory_class() {
        assert_eq!(Glyph::classify("CDS", None), Glyph::Cds);
        assert_eq!(Glyph::classify("promoter", None), Glyph::Promoter);
        assert_eq!(Glyph::classify("regulatory", Some("promoter")), Glyph::Promoter);
        assert_eq!(Glyph::classify("regulatory", Some("ribosome_binding_site")), Glyph::Rbs);
        assert_eq!(Glyph::classify("regulatory", Some("terminator")), Glyph::Terminator);
        assert_eq!(Glyph::classify("regulatory", None), Glyph::NoGlyph);
        assert_eq!(Glyph::classify("rep_origin", None), Glyph::Origin);
        assert_eq!(Glyph::classify("primer_bind", None), Glyph::PrimerBindingSite);
        assert_eq!(Glyph::classify("variation", None), Glyph::NoGlyph);
    }

    #[test]
    fn cds_arrow_points_to_the_3_prime_end() {
        let s = &shapes(Glyph::Cds, 100.0, 16.0)[0];
        let tip = s.points.iter().cloned().fold((f32::MIN, 0.0), |a, p| if p.0 > a.0 { p } else { a });
        assert_eq!(tip, (100.0, 0.0));
    }

    #[test]
    fn glyphs_stay_inside_their_box() {
        let all = [
            Glyph::Cds,
            Glyph::Promoter,
            Glyph::Rbs,
            Glyph::Terminator,
            Glyph::Origin,
            Glyph::OriginOfTransfer,
            Glyph::PrimerBindingSite,
            Glyph::Operator,
            Glyph::Insulator,
            Glyph::RecombinationSite,
            Glyph::PolyA,
            Glyph::NcRna,
            Glyph::EngineeredRegion,
            Glyph::NoGlyph,
        ];
        for g in all {
            let (lo, hi) = g.y_range(16.0);
            for s in shapes(g, 40.0, 16.0) {
                for (x, y) in s.points {
                    assert!((-0.01..=41.01).contains(&x), "{g:?} x={x}");
                    assert!(y >= lo - 0.01 && y <= hi + 2.5, "{g:?} y={y}");
                }
            }
        }
    }
}

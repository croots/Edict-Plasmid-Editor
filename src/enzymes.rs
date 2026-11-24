//! Restriction enzymes: NEB's catalogue (from REBASE, see `enzyme_data`), finding
//! their sites on both strands, where they cut, and the fragments a digest leaves.
//!
//! Positions are boundaries between bases in top-strand coordinates: a cut at `p`
//! falls between base `p - 1` and base `p`. On a circular sequence sites, cuts and
//! fragments may run through the origin; cut positions are then taken modulo the
//! length, so a cut exactly at the origin is position 0.

use std::sync::OnceLock;

use crate::enzyme_data::ENZYMES;
use crate::model::{self, Feature};

pub struct Enzyme {
    pub name: &'static str,
    /// Recognition site 5'→3' on the strand it's written for, in IUPAC codes.
    pub site: &'static str,
    /// Each cut as (top, bottom) boundaries relative to the site's first base: 0 is
    /// just before the site, `site.len()` just after it, negative is upstream.
    pub cuts: Vec<(i32, i32)>,
}

/// EMBOSS counts cuts from 1 at the site's first base and skips 0 going upstream.
fn emboss_offset(c: i16) -> i32 {
    if c < 0 { c as i32 + 1 } else { c as i32 }
}

pub fn all() -> &'static [Enzyme] {
    static ALL: OnceLock<Vec<Enzyme>> = OnceLock::new();
    ALL.get_or_init(|| {
        ENZYMES
            .iter()
            .map(|&(name, site, c1, c2, c3, c4)| {
                let mut cuts = vec![(emboss_offset(c1), emboss_offset(c2))];
                if c3 != 0 || c4 != 0 {
                    cuts.push((emboss_offset(c3), emboss_offset(c4)));
                }
                Enzyme { name, site, cuts }
            })
            .collect()
    })
}

#[cfg(test)]
pub fn by_name(name: &str) -> Option<usize> {
    all().iter().position(|e| e.name.eq_ignore_ascii_case(name))
}

/// Bit mask of the bases an IUPAC code stands for (A=1, C=2, G=4, T=8); 0 if not one.
fn mask(b: u8) -> u8 {
    match b.to_ascii_uppercase() {
        b'A' => 1,
        b'C' => 2,
        b'G' => 4,
        b'T' | b'U' => 8,
        b'M' => 1 | 2,
        b'R' => 1 | 4,
        b'W' => 1 | 8,
        b'S' => 2 | 4,
        b'Y' => 2 | 8,
        b'K' => 4 | 8,
        b'V' => 1 | 2 | 4,
        b'H' => 1 | 2 | 8,
        b'D' => 1 | 4 | 8,
        b'B' => 2 | 4 | 8,
        b'N' => 15,
        _ => 0,
    }
}

impl Enzyme {
    pub fn len(&self) -> usize {
        self.site.len()
    }

    /// The same site on the other strand reads the same.
    pub fn palindromic(&self) -> bool {
        model::reverse_complement(self.site.as_bytes()).eq_ignore_ascii_case(self.site.as_bytes())
    }

    /// Type IIS: an asymmetric site, cut once to one side of it.
    pub fn type_iis(&self) -> bool {
        let l = self.len() as i32;
        let [(t, b)] = self.cuts[..] else { return false };
        !self.palindromic() && (t.min(b) >= l || t.max(b) <= 0)
    }

    /// How NEB writes it: `G/AATTC` for a cut inside the site, `GGTCTC(1/5)` or
    /// `(10/15)ACNNNNGTAYC(12/7)` for cuts outside it.
    pub fn specificity(&self) -> String {
        let l = self.len() as i32;
        let mut before = None;
        let mut after = None;
        let mut inside = None;
        for &(t, b) in &self.cuts {
            if t.max(b) <= 0 {
                before = Some(format!("({}/{})", -t, -b));
            } else if t.min(b) >= l {
                after = Some(format!("({}/{})", t - l, b - l));
            } else {
                inside = Some(t);
            }
        }
        let mut site = self.site.to_string();
        if let Some(t) = inside.filter(|&t| t >= 0 && t <= l) {
            site.insert(t as usize, '/');
        }
        format!("{}{site}{}", before.unwrap_or_default(), after.unwrap_or_default())
    }

    /// The overhang a cut leaves, as text: "5' AATT", "3' GTAC", "blunt", or
    /// "5' NNNN" where it depends on the bases outside a type IIS site.
    pub fn end_description(&self) -> String {
        let (t, b) = self.cuts[0];
        let (lo, hi) = (t.min(b), t.max(b));
        let l = self.len() as i32;
        let bases: String = if lo >= 0 && hi <= l { self.site[lo as usize..hi as usize].to_string() } else { "N".repeat((hi - lo) as usize) };
        match b - t {
            0 => "blunt".into(),
            o if o > 0 => format!("5′ {bases}"),
            _ => format!("3′ {bases}"),
        }
    }
}

/// One place an enzyme recognises.
#[derive(Clone, Debug, PartialEq)]
pub struct Site {
    pub enzyme: usize,
    /// The site covers `start .. start + len`, through the origin on a circular sequence.
    pub start: usize,
    pub len: usize,
    /// Read on the top strand; otherwise on the bottom strand.
    pub forward: bool,
    pub cuts: Vec<Cut>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    /// Where the top strand is cut, 0..len (0..=len on a linear sequence, though a cut
    /// at either end isn't kept).
    pub top: usize,
    /// Where the bottom strand is cut, in top-strand coordinates.
    pub bottom: usize,
    /// `bottom - top` measured the short way: positive leaves a 5' overhang.
    pub overhang: i32,
}

fn site_masks(site: &str) -> (Vec<u8>, Vec<u8>) {
    let fwd: Vec<u8> = site.bytes().map(mask).collect();
    let rev: Vec<u8> = model::reverse_complement(site.as_bytes()).into_iter().map(mask).collect();
    (fwd, rev)
}

/// Every start position where `pat` matches. A base matches a code when it's one of
/// the bases the code allows; an ambiguous base (N) only matches a code that covers it.
fn matches(seq: &[u8], circular: bool, pat: &[u8], out: &mut Vec<usize>) {
    let (n, l) = (seq.len(), pat.len());
    if l == 0 || n < l {
        return;
    }
    let last = if circular { n } else { n - l + 1 };
    for s in 0..last {
        let ok = if s + l <= n {
            seq[s..s + l].iter().zip(pat).all(|(&b, &p)| {
                let m = mask(b);
                m != 0 && m & !p == 0
            })
        } else {
            (0..l).all(|i| {
                let m = mask(seq[(s + i) % n]);
                m != 0 && m & !pat[i] == 0
            })
        };
        if ok {
            out.push(s);
        }
    }
}

/// Every site of enzyme `ix`, in order of position, with the cuts that land in the
/// sequence. On a linear sequence a cut that would fall off either end isn't made.
pub fn find_sites(seq: &[u8], circular: bool, ix: usize) -> Vec<Site> {
    let e = &all()[ix];
    let (n, l) = (seq.len() as i64, e.len() as i64);
    let (fwd, rev) = site_masks(e.site);
    let mut starts = Vec::new();
    matches(seq, circular, &fwd, &mut starts);
    let mut found: Vec<(usize, bool)> = starts.iter().map(|&s| (s, true)).collect();
    if !e.palindromic() {
        starts.clear();
        matches(seq, circular, &rev, &mut starts);
        found.extend(starts.iter().map(|&s| (s, false)));
        found.sort_unstable();
    }
    found
        .into_iter()
        .map(|(start, forward)| {
            let s = start as i64;
            let cuts = e
                .cuts
                .iter()
                .filter_map(|&(t, b)| {
                    // On the bottom strand the site reads right to left from its end.
                    let (t, b) = if forward { (s + t as i64, s + b as i64) } else { (s + l - b as i64, s + l - t as i64) };
                    if circular {
                        Some(Cut { top: t.rem_euclid(n) as usize, bottom: b.rem_euclid(n) as usize, overhang: (b - t) as i32 })
                    } else if t > 0 && t < n && b > 0 && b < n {
                        Some(Cut { top: t as usize, bottom: b as usize, overhang: (b - t) as i32 })
                    } else {
                        None
                    }
                })
                .collect();
            Site { enzyme: ix, start, len: e.len(), forward, cuts }
        })
        .collect()
}

/// How many times enzyme `ix` cuts the sequence.
pub fn count_cuts(seq: &[u8], circular: bool, ix: usize) -> usize {
    find_sites(seq, circular, ix).iter().map(|s| s.cuts.len()).sum()
}

/// Where `[a, b)` (unwrapped; `b` may pass the end or `a` precede the start) falls on
/// a sequence of `n` bases: one span, or two where it runs through the origin.
pub fn wrap_span(a: i64, b: i64, n: usize) -> Vec<(usize, usize)> {
    let n = n as i64;
    if n == 0 || b <= a {
        return Vec::new();
    }
    if b - a >= n {
        return vec![(0, n as usize)];
    }
    let a0 = a.rem_euclid(n);
    let b0 = a0 + (b - a);
    if b0 <= n { vec![(a0 as usize, b0 as usize)] } else { vec![(a0 as usize, n as usize), (0, (b0 - n) as usize)] }
}

/// The end of a fragment: where an enzyme cut it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    pub enzyme: usize,
    pub overhang: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    /// Top-strand start; the fragment runs `len` bases from here, through the origin
    /// on a circular sequence.
    pub start: usize,
    pub len: usize,
    /// None at the end of a linear sequence.
    pub left: Option<End>,
    pub right: Option<End>,
    /// An uncut circle.
    pub circular: bool,
}

/// The fragments the sites' cuts leave, in order along the sequence. Fragment sizes
/// are top-strand lengths. A linear sequence cut k times gives k + 1 fragments; a
/// circular one gives k, the last running through the origin (k = 0 leaves the
/// circle whole).
pub fn digest(n: usize, circular: bool, sites: &[Site]) -> Vec<Fragment> {
    let mut cuts: Vec<(usize, End)> =
        sites.iter().flat_map(|s| s.cuts.iter().map(move |c| (c.top, End { enzyme: s.enzyme, overhang: c.overhang }))).collect();
    cuts.sort_by_key(|c| c.0);
    cuts.dedup_by_key(|c| c.0);
    if n == 0 {
        return Vec::new();
    }
    if circular {
        if cuts.is_empty() {
            return vec![Fragment { start: 0, len: n, left: None, right: None, circular: true }];
        }
        let k = cuts.len();
        (0..k)
            .map(|i| {
                let (a, ea) = cuts[i];
                let (b, eb) = cuts[(i + 1) % k];
                let len = if k == 1 { n } else { (b + n - a) % n };
                Fragment { start: a, len, left: Some(ea), right: Some(eb), circular: false }
            })
            .collect()
    } else {
        let mut out = Vec::with_capacity(cuts.len() + 1);
        let mut prev: (usize, Option<End>) = (0, None);
        for &(p, e) in &cuts {
            out.push(Fragment { start: prev.0, len: p - prev.0, left: prev.1, right: Some(e), circular: false });
            prev = (p, Some(e));
        }
        out.push(Fragment { start: prev.0, len: n - prev.0, left: prev.1, right: None, circular: false });
        out
    }
}

/// The fragment's top strand.
pub fn fragment_bases(seq: &[u8], f: &Fragment) -> Vec<u8> {
    let n = seq.len();
    if f.start + f.len <= n {
        seq[f.start..f.start + f.len].to_vec()
    } else {
        seq[f.start..].iter().chain(&seq[..f.start + f.len - n]).copied().collect()
    }
}

/// The features that lie wholly inside the fragment, moved to its coordinates. A
/// feature that ran through the origin comes out in one piece.
pub fn fragment_features(features: &[Feature], n: usize, f: &Fragment) -> Vec<Feature> {
    if f.circular {
        return features.to_vec();
    }
    features
        .iter()
        .filter_map(|feat| {
            let mut segs: Vec<(usize, usize)> = Vec::with_capacity(feat.segments.len());
            for &(s, e) in &feat.segments {
                let rel = (s + n - f.start) % n;
                if e <= s || rel + (e - s) > f.len {
                    return None;
                }
                let seg = (rel, rel + (e - s));
                match segs.last_mut() {
                    Some(last) if last.1 == seg.0 => last.1 = seg.1,
                    Some(last) if seg.1 == last.0 => last.0 = seg.0,
                    _ => segs.push(seg),
                }
            }
            (!segs.is_empty()).then(|| Feature { segments: segs, ..feat.clone() })
        })
        .collect()
}

// ----- gel --------------------------------------------------------------------------

/// NEB's 1 kb Plus DNA Ladder (N3200), in bp; the 3 kb and 1 kb bands are brighter.
pub const LADDER: &[usize] = &[10_000, 8_000, 6_000, 5_000, 4_000, 3_000, 2_000, 1_500, 1_200, 1_000, 900, 800, 700, 600, 500, 400, 300, 200, 100];
pub const LADDER_BRIGHT: &[usize] = &[3_000, 1_000];

/// The largest and smallest sizes a 1% gel resolves; bands outside pile up at the ends.
const GEL_MAX: f32 = 20_000.0;
const GEL_MIN: f32 = 60.0;

/// How far a linear duplex of `bp` runs down a 1% agarose gel, 0 (the well) to 1
/// (the dye front): roughly in proportion to the log of its size.
pub fn migration(bp: f32) -> f32 {
    let bp = bp.clamp(GEL_MIN, GEL_MAX);
    0.03 + 0.94 * (GEL_MAX.log10() - bp.log10()) / (GEL_MAX.log10() - GEL_MIN.log10())
}

/// Supercoiled plasmid runs ahead of the same plasmid cut open, at roughly this
/// share of its size.
pub const SUPERCOILED: f32 = 0.7;

#[cfg(test)]
mod tests {
    use super::*;

    fn ix(name: &str) -> usize {
        by_name(name).unwrap_or_else(|| panic!("{name} missing"))
    }

    #[test]
    fn catalogue_reads_the_way_neb_writes_it() {
        assert!(all().len() > 200);
        assert_eq!(all()[ix("EcoRI")].specificity(), "G/AATTC");
        assert_eq!(all()[ix("KpnI")].specificity(), "GGTAC/C");
        assert_eq!(all()[ix("BsaI")].specificity(), "GGTCTC(1/5)");
        assert_eq!(all()[ix("SapI")].specificity(), "GCTCTTC(1/4)");
        assert_eq!(all()[ix("BaeI")].specificity(), "(10/15)ACNNNNGTAYC(12/7)");
        assert_eq!(all()[ix("EcoRI")].end_description(), "5′ AATT");
        assert_eq!(all()[ix("KpnI")].end_description(), "3′ GTAC");
        assert_eq!(all()[ix("EcoRV")].end_description(), "blunt");
        for name in ["BsaI", "BsmBI", "BbsI", "SapI", "MmeI", "BtsCI", "MlyI"] {
            assert!(all()[ix(name)].type_iis(), "{name} is type IIS");
        }
        for name in ["EcoRI", "NotI", "BglI", "BaeI", "SfiI"] {
            assert!(!all()[ix(name)].type_iis(), "{name} isn't type IIS");
        }
    }

    #[test]
    fn finds_sites_and_cuts_on_both_strands() {
        //            0         1         2         3
        //            0123456789012345678901234567890123
        let seq = b"AAGAATTCAAAAGGTCTCAAAAAAAAGAGACCAA";
        let e = find_sites(seq, false, ix("EcoRI"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].cuts, vec![Cut { top: 3, bottom: 7, overhang: 4 }]);

        // BsaI GGTCTC(1/5): forward at 12, and its reverse complement GAGACC at 26.
        let b = find_sites(seq, false, ix("BsaI"));
        assert_eq!(b.iter().map(|s| (s.start, s.forward)).collect::<Vec<_>>(), vec![(12, true), (26, false)]);
        assert_eq!(b[0].cuts, vec![Cut { top: 19, bottom: 23, overhang: 4 }]);
        // Read on the bottom strand, it cuts to the left: 1 and 5 bases before the site.
        assert_eq!(b[1].cuts, vec![Cut { top: 21, bottom: 25, overhang: 4 }]);
    }

    #[test]
    fn iupac_codes_match_only_what_they_allow() {
        // BglI GCCNNNNNGGC; AccI GT/MKAC.
        assert_eq!(count_cuts(b"TTGCCATGCAGGCTT", false, ix("BglI")), 1);
        assert_eq!(count_cuts(b"TTGTAGACTT", false, ix("AccI")), 1);
        assert_eq!(count_cuts(b"TTGTCGACTT", false, ix("AccI")), 1); // M = A/C, K = G/T
        assert_eq!(count_cuts(b"TTGTGTACTT", false, ix("AccI")), 0);
        assert_eq!(count_cuts(b"TTGAANTCTT", false, ix("EcoRI")), 0);
    }

    #[test]
    fn linear_ends_lose_cuts_that_fall_off() {
        // BsaI's cut is 1 base past the site; at the very end there's nothing to cut.
        assert_eq!(find_sites(b"AAGGTCTC", false, ix("BsaI"))[0].cuts, vec![]);
        // A site flush against the end of a linear sequence cuts inside it.
        assert_eq!(count_cuts(b"GAATTC", false, ix("EcoRI")), 1);
    }

    #[test]
    fn circular_sites_and_cuts_run_through_the_origin() {
        // EcoRI split over the origin: ATTC....GA
        let seq = b"ATTCAAAAAAAAGA";
        assert_eq!(find_sites(seq, false, ix("EcoRI")).len(), 0);
        let s = find_sites(seq, true, ix("EcoRI"));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].start, 12);
        // G/AATTC: the top cut is after G at 12, the bottom after AATT, 3 bases past the origin.
        assert_eq!(s[0].cuts, vec![Cut { top: 13, bottom: 3, overhang: 4 }]);

        // A cut exactly on the origin.
        let seq = b"AATTCAAAAAAAAG";
        let s = find_sites(seq, true, ix("EcoRI"));
        assert_eq!(s[0].cuts, vec![Cut { top: 0, bottom: 4, overhang: 4 }]);
        let f = digest(seq.len(), true, &s);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start, f[0].len), (0, 14));
        assert_eq!(fragment_bases(seq, &f[0]), seq.to_vec());
    }

    #[test]
    fn digests_count_fragments_by_topology() {
        //          0123456789012345678901234567
        let seq = b"AAGAATTCAAAAAAGGATCCAAAAAAAA";
        let n = seq.len();
        let mut sites = find_sites(seq, false, ix("EcoRI"));
        sites.extend(find_sites(seq, false, ix("BamHI")));
        let lin = digest(n, false, &sites);
        assert_eq!(lin.iter().map(|f| (f.start, f.len)).collect::<Vec<_>>(), vec![(0, 3), (3, 12), (15, 13)]);
        assert_eq!(lin[0].left, None);
        assert_eq!(lin[2].right, None);

        let mut sites = find_sites(seq, true, ix("EcoRI"));
        sites.extend(find_sites(seq, true, ix("BamHI")));
        let circ = digest(n, true, &sites);
        assert_eq!(circ.iter().map(|f| (f.start, f.len)).collect::<Vec<_>>(), vec![(3, 12), (15, 16)]);
        // The second runs through the origin.
        let b = fragment_bases(seq, &circ[1]);
        assert_eq!(b, [&seq[15..], &seq[..3]].concat());
        assert_eq!(circ.iter().map(|f| f.len).sum::<usize>(), n);

        assert!(digest(n, true, &[])[0].circular);
    }

    #[test]
    fn fragment_features_follow_their_bases() {
        let f = |segs: Vec<(usize, usize)>| Feature { kind: "misc_feature".into(), label: "x".into(), segments: segs, strand: 1, source: None };
        let features = vec![f(vec![(26, 30), (0, 2)]), f(vec![(4, 8)]), f(vec![(10, 20)])];
        // A fragment from 25 through the origin to 9.
        let frag = Fragment { start: 25, len: 14, left: None, right: None, circular: false };
        let out = fragment_features(&features, 30, &frag);
        assert_eq!(out.iter().map(|f| f.segments.clone()).collect::<Vec<_>>(), vec![vec![(1, 7)], vec![(9, 13)]]);
    }

    #[test]
    fn gel_runs_small_fragments_further() {
        assert!(migration(500.0) > migration(5_000.0));
        assert!(migration(1.0e6) >= 0.0 && migration(1.0) <= 1.0);
    }

    #[test]
    fn real_plasmid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/pEGFP-N1.gb");
        let doc = model::Document::open(&path).unwrap();
        // pEGFP-N1's MCS has single EcoRI, BamHI and NotI sites.
        for name in ["EcoRI", "BamHI", "NotI"] {
            assert_eq!(count_cuts(&doc.seq, doc.circular, ix(name)), 1, "{name}");
        }
        let start = std::time::Instant::now();
        let total: usize = (0..all().len()).map(|i| count_cuts(&doc.seq, doc.circular, i)).sum();
        assert!(total > 1000);
        eprintln!("counted every enzyme over {} bp in {:?}", doc.seq.len(), start.elapsed());
    }
}

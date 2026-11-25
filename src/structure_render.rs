//! A small software renderer for molecules: everything is drawn as shaded spheres
//! (a tube of overlapping spheres along the backbone, or one sphere per atom) into a
//! depth buffer, then lit, fogged by depth, outlined and downsampled.
//!
//! GPUI has no 3D API, so this produces a BGRA bitmap the view hands to `paint_image`.
//! Alongside it comes a buffer of which residue is at each pixel, for picking.

use crate::structure::{Molecule, Ss};

pub type V3 = [f32; 3];
pub type M3 = [[f32; 3]; 3];

pub const IDENTITY: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn mat_vec(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

pub fn mat_mul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 1e-9 { scale(a, 1.0 / l) } else { [1.0, 0.0, 0.0] }
}

fn mix(a: V3, b: V3, t: f32) -> V3 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// A rotation about the screen's x (`dx` = 0) or y axis, composed onto `rot`.
pub fn rotate(rot: &M3, about_y: f32, about_x: f32) -> M3 {
    let (sy, cy) = about_y.sin_cos();
    let (sx, cx) = about_x.sin_cos();
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    orthonormalize(mat_mul(&rx, &mat_mul(&ry, rot)))
}

/// Re-orthonormalize the rows, so accumulated rotations don't drift into shears.
fn orthonormalize(m: M3) -> M3 {
    let x = normalize(m[0]);
    let z = normalize(cross(x, m[1]));
    let y = cross(z, x);
    [x, y, z]
}

/// The rotation that lays the molecule's longest axis horizontal and its second
/// longest vertical (principal axes of the backbone).
pub fn principal_axes(points: &[V3], center: V3) -> M3 {
    if points.len() < 3 {
        return IDENTITY;
    }
    let mut c = [[0.0f32; 3]; 3];
    for p in points {
        let d = sub(*p, center);
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] += d[i] * d[j];
            }
        }
    }
    let power = |m: &M3, start: V3| {
        let mut v = normalize(start);
        for _ in 0..64 {
            v = normalize(mat_vec(m, v));
        }
        v
    };
    let e1 = power(&c, [1.0, 0.7, 0.3]);
    let l1 = dot(mat_vec(&c, e1), e1);
    let mut d = c;
    for i in 0..3 {
        for j in 0..3 {
            d[i][j] -= l1 * e1[i] * e1[j];
        }
    }
    let mut e2 = power(&d, cross(e1, [0.3, 0.2, 1.0]));
    e2 = normalize(sub(e2, scale(e1, dot(e1, e2))));
    let e3 = cross(e1, e2);
    [e1, e2, e3]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Ribbons for helices, arrows for strands and a thin tube for loops, as in PyMOL;
    /// ligands and modified residues as small balls.
    Cartoon,
    /// A tube along the backbone, thicker through helices and strands; ligands and
    /// modified residues (like a chromophore) as small balls.
    Tube,
    /// Every atom at its van der Waals radius.
    Atoms,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coloring {
    /// Blue at the N-terminus through to red at the C-terminus, per chain.
    Rainbow,
    Secondary,
    Chain,
    /// B-factors, or for AlphaFold models the pLDDT confidence they hold.
    BFactor,
    /// The plasmid features each residue's codons fall in.
    Features,
}

#[derive(Clone, Copy, Debug)]
pub struct Sphere {
    pub c: V3,
    pub r: f32,
    pub color: V3,
    pub residue: u32,
}

pub const NO_RESIDUE: u32 = u32::MAX;

fn element_radius(e: &str) -> f32 {
    match e {
        "H" => 1.1,
        "C" => 1.7,
        "N" => 1.55,
        "O" => 1.52,
        "S" => 1.8,
        "P" => 1.8,
        "F" => 1.47,
        "CL" => 1.75,
        "SE" => 1.9,
        _ => 1.8,
    }
}

pub fn element_color(e: &str) -> V3 {
    match e {
        "H" => [0.92, 0.92, 0.92],
        "C" => [0.56, 0.58, 0.6],
        "N" => [0.25, 0.4, 0.95],
        "O" => [0.95, 0.2, 0.15],
        "S" => [0.95, 0.8, 0.2],
        "P" => [1.0, 0.55, 0.0],
        "FE" => [0.88, 0.4, 0.2],
        "MG" => [0.4, 0.85, 0.3],
        "ZN" => [0.5, 0.5, 0.7],
        "CA" => [0.4, 0.8, 0.4],
        "SE" => [1.0, 0.63, 0.0],
        _ => [0.9, 0.4, 0.9],
    }
}

pub fn hsv(h: f32, s: f32, v: f32) -> V3 {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

const CHAIN_COLORS: [V3; 8] = [
    [0.33, 0.62, 0.92],
    [0.95, 0.55, 0.25],
    [0.4, 0.78, 0.45],
    [0.88, 0.38, 0.55],
    [0.65, 0.5, 0.88],
    [0.9, 0.8, 0.3],
    [0.35, 0.8, 0.8],
    [0.7, 0.55, 0.4],
];

/// A colour for every residue under `coloring`. `feature_colors` (only read for
/// `Coloring::Features`) gives the colour of the feature each residue's codons fall in.
pub fn residue_colors(mol: &Molecule, coloring: Coloring, alphafold: bool, feature_colors: &[Option<V3>]) -> Vec<V3> {
    let mut out = vec![[0.7, 0.7, 0.7]; mol.residues.len()];
    match coloring {
        Coloring::Rainbow => {
            for chain in &mol.chains {
                let polymer: Vec<usize> = chain.residues.clone().filter(|&r| mol.residues[r].polymer).collect();
                let n = polymer.len().max(2) - 1;
                for (i, &r) in polymer.iter().enumerate() {
                    out[r] = hsv(0.66 * (1.0 - i as f32 / n as f32), 0.75, 0.95);
                }
            }
        }
        Coloring::Secondary => {
            for (r, res) in mol.residues.iter().enumerate() {
                out[r] = match res.ss {
                    Ss::Helix => [0.9, 0.3, 0.55],
                    Ss::Strand => [0.98, 0.78, 0.2],
                    Ss::Coil => [0.75, 0.77, 0.8],
                };
            }
        }
        Coloring::Chain => {
            for (r, res) in mol.residues.iter().enumerate() {
                out[r] = CHAIN_COLORS[res.chain % CHAIN_COLORS.len()];
            }
        }
        Coloring::BFactor => {
            let mean: Vec<f32> = mol
                .residues
                .iter()
                .map(|r| {
                    let n = r.atoms.len().max(1) as f32;
                    mol.atoms[r.atoms.clone()].iter().map(|a| a.b_factor).sum::<f32>() / n
                })
                .collect();
            if alphafold {
                for (r, &b) in mean.iter().enumerate() {
                    // The AlphaFold DB's pLDDT bands.
                    out[r] = if b > 90.0 {
                        [0.0, 0.33, 0.84]
                    } else if b > 70.0 {
                        [0.4, 0.8, 0.95]
                    } else if b > 50.0 {
                        [1.0, 0.86, 0.07]
                    } else {
                        [1.0, 0.49, 0.27]
                    };
                }
            } else {
                let lo = mean.iter().copied().fold(f32::INFINITY, f32::min);
                let hi = mean.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let span = (hi - lo).max(1e-3);
                for (r, &b) in mean.iter().enumerate() {
                    let t = (b - lo) / span;
                    out[r] = if t < 0.5 {
                        mix([0.2, 0.35, 0.9], [0.95, 0.95, 0.95], t * 2.0)
                    } else {
                        mix([0.95, 0.95, 0.95], [0.9, 0.2, 0.15], (t - 0.5) * 2.0)
                    };
                }
            }
        }
        Coloring::Features => {
            for (r, c) in feature_colors.iter().enumerate().take(out.len()) {
                out[r] = c.unwrap_or([0.55, 0.55, 0.58]);
            }
        }
    }
    out
}

const HIGHLIGHT_MIX: f32 = 0.7;

/// Each chain's trace atoms, split into unbroken runs of (position, residue).
fn chain_runs(mol: &Molecule) -> Vec<Vec<(V3, usize)>> {
    let mut runs = Vec::new();
    for chain in &mol.chains {
        let mut run: Vec<(V3, usize)> = Vec::new();
        let mut prev: Option<usize> = None;
        for r in chain.residues.clone() {
            let res = &mol.residues[r];
            let Some(t) = res.trace.filter(|_| res.polymer) else { continue };
            let p = mol.atoms[t].pos;
            if let (Some(&(q, _)), Some(pr)) = (run.last(), prev) {
                // A gap in the model (missing residues) breaks the tube. Residues
                // that stand for several (chromophores) reach further.
                let extra = (mol.residues[pr].letters.len() + res.letters.len()).saturating_sub(2) as f32;
                let limit = if mol.atoms[t].element == "P" { 8.0 } else { 4.6 } + 3.8 * extra;
                let d = sub(p, q);
                if dot(d, d) > limit * limit {
                    runs.push(std::mem::take(&mut run));
                }
            }
            run.push((p, r));
            prev = Some(r);
        }
        runs.push(run);
    }
    runs
}

/// The model-space scene for `mol`. `highlight` residues are tinted with `accent`.
pub fn build_scene(mol: &Molecule, style: Style, colors: &[V3], highlight: &[bool], accent: V3) -> Scene {
    let tint = |r: usize, c: V3| if highlight.get(r).copied().unwrap_or(false) { mix(c, accent, HIGHLIGHT_MIX) } else { c };
    let mut scene = Scene::default();
    let out = &mut scene.spheres;
    match style {
        Style::Atoms => {
            for a in &mol.atoms {
                let res = &mol.residues[a.residue];
                // Ligands keep element colours whatever the scheme.
                let base = if res.polymer { colors[a.residue] } else { element_color(&a.element) };
                out.push(Sphere { c: a.pos, r: element_radius(&a.element), color: tint(a.residue, base), residue: a.residue as u32 });
            }
        }
        Style::Tube | Style::Cartoon => {
            for run in chain_runs(mol) {
                if style == Style::Cartoon {
                    cartoon(&mut scene, &run, mol, colors, &tint);
                } else {
                    tube(&mut scene.spheres, &run, mol, colors, &tint);
                }
            }
            let out = &mut scene.spheres;
            // Ligands, ions and modified residues as balls in element colours.
            for (r, res) in mol.residues.iter().enumerate() {
                if res.polymer && !res.is_modified() {
                    continue;
                }
                for a in &mol.atoms[res.atoms.clone()] {
                    if a.element == "H" {
                        continue;
                    }
                    let lone = res.atoms.len() == 1;
                    let mut c = element_color(&a.element);
                    if a.element == "C" {
                        c = [0.35, 0.85, 0.35];
                    }
                    out.push(Sphere { c: a.pos, r: if lone { 1.1 } else { 0.55 }, color: tint(r, c), residue: r as u32 });
                }
            }
        }
    }
    scene
}

fn tube_radius(ss: Ss) -> f32 {
    match ss {
        Ss::Helix => 1.3,
        Ss::Strand => 1.0,
        Ss::Coil => 0.42,
    }
}

/// Spheres along a Catmull–Rom spline through the run's trace atoms.
fn tube(out: &mut Vec<Sphere>, run: &[(V3, usize)], mol: &Molecule, colors: &[V3], tint: &dyn Fn(usize, V3) -> V3) {
    const STEPS: usize = 20;
    let sphere = |c: V3, r: f32, res: usize| Sphere { c, r, color: tint(res, colors[res]), residue: res as u32 };
    match run.len() {
        0 => return,
        1 => {
            out.push(sphere(run[0].0, 1.0, run[0].1));
            return;
        }
        _ => {}
    }
    let n = run.len();
    for i in 0..n - 1 {
        let p0 = run[i.saturating_sub(1)].0;
        let (p1, r1) = run[i];
        let (p2, r2) = run[i + 1];
        let p3 = run[(i + 2).min(n - 1)].0;
        let (rad1, rad2) = (tube_radius(mol.residues[r1].ss), tube_radius(mol.residues[r2].ss));
        for k in 0..STEPS {
            let t = k as f32 / STEPS as f32;
            let (t2, t3) = (t * t, t * t * t);
            let mut c = [0.0; 3];
            for d in 0..3 {
                c[d] = 0.5
                    * (2.0 * p1[d]
                        + (p2[d] - p0[d]) * t
                        + (2.0 * p0[d] - 5.0 * p1[d] + 4.0 * p2[d] - p3[d]) * t2
                        + (3.0 * p1[d] - p0[d] - 3.0 * p2[d] + p3[d]) * t3);
            }
            let s = t * t * (3.0 - 2.0 * t);
            let res = if t < 0.5 { r1 } else { r2 };
            out.push(sphere(c, rad1 + (rad2 - rad1) * s, res));
        }
    }
    let (p, r) = run[n - 1];
    out.push(sphere(p, tube_radius(mol.residues[r].ss), r));
}

// ----- cartoon ------------------------------------------------------------------------

/// A triangle of the cartoon mesh, in model space, with per-vertex normals and colours.
#[derive(Clone, Copy, Debug)]
pub struct Tri {
    pub v: [V3; 3],
    pub n: [V3; 3],
    pub color: [V3; 3],
    pub residue: u32,
}

/// Everything to draw: shaded spheres and a lit triangle mesh.
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub spheres: Vec<Sphere>,
    pub tris: Vec<Tri>,
}

/// A ribbon cross-section: half-width, half-thickness and superellipse exponent
/// (2 is an ellipse; larger is boxier).
#[derive(Clone, Copy, Debug)]
struct Section {
    hw: f32,
    ht: f32,
    e: f32,
}

const HELIX: Section = Section { hw: 1.4, ht: 0.22, e: 2.4 };
const STRAND: Section = Section { hw: 1.15, ht: 0.3, e: 8.0 };
const COIL: Section = Section { hw: 0.36, ht: 0.36, e: 2.0 };
/// Half-width at the back of a strand's arrowhead.
const ARROW_HW: f32 = 1.9;
/// Vertices around each cross-section, and cross-sections per residue.
const RING: usize = 16;
const CARTOON_STEPS: usize = 10;

fn section(ss: Ss) -> Section {
    match ss {
        Ss::Helix => HELIX,
        Ss::Strand => STRAND,
        Ss::Coil => COIL,
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_section(a: Section, b: Section, t: f32) -> Section {
    Section { hw: lerp(a.hw, b.hw, t), ht: lerp(a.ht, b.ht, t), e: lerp(a.e, b.e, t) }
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Any unit vector perpendicular to `t`.
fn perpendicular(t: V3) -> V3 {
    let a = if t[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    normalize(cross(t, a))
}

struct Ring {
    pts: [V3; RING],
    nrm: [V3; RING],
    color: V3,
    res: usize,
    /// Stitch to the previous ring.
    join: bool,
}

/// The cross-section `sec` at `p`, with tangent `t` and width axis `w`. `flat` replaces
/// the profile's normals (for caps and the back of arrowheads); `collapse` shrinks the
/// ring to its centre (the tip of a cap).
#[allow(clippy::too_many_arguments)]
fn ring(p: V3, t: V3, w: V3, sec: Section, flat: Option<V3>, collapse: bool, color: V3, res: usize, join: bool) -> Ring {
    let up = cross(t, w);
    let mut pts = [p; RING];
    let mut nrm = [up; RING];
    let ex = 2.0 / sec.e;
    for k in 0..RING {
        let (s, c) = (std::f32::consts::TAU * k as f32 / RING as f32).sin_cos();
        let (ux, uy) = (c.signum() * c.abs().powf(ex), s.signum() * s.abs().powf(ex));
        if !collapse {
            pts[k] = add(p, add(scale(w, ux * sec.hw), scale(up, uy * sec.ht)));
        }
        nrm[k] = flat.unwrap_or_else(|| {
            // The gradient of the superellipse |x/hw|^e + |y/ht|^e.
            let gx = ux.signum() * ux.abs().powf(sec.e - 1.0) / sec.hw;
            let gy = uy.signum() * uy.abs().powf(sec.e - 1.0) / sec.ht;
            normalize(add(scale(w, gx), scale(up, gy)))
        });
    }
    Ring { pts, nrm, color, res, join }
}

fn stitch(out: &mut Vec<Tri>, rings: &[Ring]) {
    for pair in rings.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if !b.join {
            continue;
        }
        let res = a.res as u32;
        for k in 0..RING {
            let k2 = (k + 1) % RING;
            out.push(Tri { v: [a.pts[k], b.pts[k], b.pts[k2]], n: [a.nrm[k], b.nrm[k], b.nrm[k2]], color: [a.color, b.color, b.color], residue: res });
            out.push(Tri { v: [a.pts[k], b.pts[k2], a.pts[k2]], n: [a.nrm[k], b.nrm[k2], a.nrm[k2]], color: [a.color, b.color, a.color], residue: res });
        }
    }
}

/// A cartoon along one unbroken run of trace atoms: flat ribbons through helices,
/// boxy arrows through strands and a thin round tube elsewhere. Ribbons are oriented by
/// the backbone carbonyls (which point along a helix's axis and lie in a sheet's plane).
fn cartoon(scene: &mut Scene, run: &[(V3, usize)], mol: &Molecule, colors: &[V3], tint: &dyn Fn(usize, V3) -> V3) {
    let n = run.len();
    if n == 0 {
        return;
    }
    let color = |r: usize| tint(r, colors[r]);
    if n == 1 {
        let (p, r) = run[0];
        scene.spheres.push(Sphere { c: p, r: COIL.hw * 1.6, color: color(r), residue: r as u32 });
        return;
    }
    let ss: Vec<Ss> = run.iter().map(|&(_, r)| mol.residues[r].ss).collect();
    let raw: Vec<V3> = run.iter().map(|&(p, _)| p).collect();

    // Strands pleat; average each strand residue with its neighbours to straighten them.
    let mut pts = raw.clone();
    for i in 1..n - 1 {
        if ss[i] == Ss::Strand {
            pts[i] = scale(add(add(raw[i - 1], raw[i + 1]), scale(raw[i], 2.0)), 0.25);
        }
    }

    // The width direction at each residue, kept from flipping between neighbours.
    let tangent = |i: usize| normalize(sub(pts[(i + 1).min(n - 1)], pts[i.saturating_sub(1)]));
    let mut guide: Vec<Option<V3>> = (0..n)
        .map(|i| {
            let res = &mol.residues[run[i].1];
            let t = tangent(i);
            let g = match res.carbonyl {
                Some(o) => sub(mol.atoms[o].pos, raw[i]),
                None if i > 0 && i + 1 < n => cross(t, sub(scale(raw[i], 2.0), add(raw[i - 1], raw[i + 1]))),
                None => return None,
            };
            let g = sub(g, scale(t, dot(g, t)));
            (dot(g, g) > 1e-6).then(|| normalize(g))
        })
        .collect();
    // Fill gaps from neighbours, or any perpendicular when there are none at all.
    for i in 1..n {
        if guide[i].is_none() {
            guide[i] = guide[i - 1];
        }
    }
    for i in (0..n - 1).rev() {
        if guide[i].is_none() {
            guide[i] = guide[i + 1];
        }
    }
    let mut guide: Vec<V3> = guide.into_iter().enumerate().map(|(i, g)| g.unwrap_or_else(|| perpendicular(tangent(i)))).collect();
    for i in 1..n {
        if dot(guide[i], guide[i - 1]) < 0.0 {
            guide[i] = scale(guide[i], -1.0);
        }
    }
    let unsmoothed = guide.clone();
    for i in 1..n - 1 {
        if ss[i] == Ss::Strand {
            guide[i] = normalize(add(add(unsmoothed[i - 1], unsmoothed[i + 1]), scale(unsmoothed[i], 2.0)));
        }
    }

    // Each strand's last residue is its arrow's tip: the segment before it is the head.
    let mut sec: Vec<Section> = ss.iter().map(|&s| section(s)).collect();
    let mut arrow = vec![false; n];
    for i in 1..n {
        if ss[i] == Ss::Strand && ss[i - 1] == Ss::Strand && (i + 1 == n || ss[i + 1] != Ss::Strand) {
            arrow[i - 1] = true;
            sec[i] = COIL;
        }
    }

    let mut rings: Vec<Ring> = Vec::with_capacity((n - 1) * CARTOON_STEPS + 8);
    for j in 0..n - 1 {
        let p0 = pts[j.saturating_sub(1)];
        let (p1, p2) = (pts[j], pts[j + 1]);
        let p3 = pts[(j + 2).min(n - 1)];
        let last = j + 2 == n;
        let steps = if last { CARTOON_STEPS + 1 } else { CARTOON_STEPS };
        for k in 0..steps {
            let t = k as f32 / CARTOON_STEPS as f32;
            let (t2, t3) = (t * t, t * t * t);
            let mut p = [0.0; 3];
            let mut d = [0.0; 3];
            for a in 0..3 {
                let (c1, c2, c3) = (p2[a] - p0[a], 2.0 * p0[a] - 5.0 * p1[a] + 4.0 * p2[a] - p3[a], 3.0 * p1[a] - p0[a] - 3.0 * p2[a] + p3[a]);
                p[a] = 0.5 * (2.0 * p1[a] + c1 * t + c2 * t2 + c3 * t3);
                d[a] = 0.5 * (c1 + 2.0 * c2 * t + 3.0 * c3 * t2);
            }
            let tan = if dot(d, d) > 1e-8 { normalize(d) } else { tangent(j) };
            let s = t * t * (3.0 - 2.0 * t);
            let g = mix(guide[j], guide[j + 1], s);
            let g = sub(g, scale(tan, dot(g, tan)));
            let w = if dot(g, g) > 1e-6 { normalize(g) } else { perpendicular(tan) };
            let section = if arrow[j] {
                Section { hw: lerp(ARROW_HW, COIL.hw, t), ht: lerp(STRAND.ht, COIL.ht, t), e: STRAND.e }
            } else {
                lerp_section(sec[j], sec[j + 1], s)
            };
            let res = if t < 0.5 { run[j].1 } else { run[j + 1].1 };
            let c = color(res);
            let back = Some(scale(tan, -1.0));
            if j == 0 && k == 0 {
                // Close the start.
                rings.push(ring(p, tan, w, sec[0], back, true, c, res, false));
                rings.push(ring(p, tan, w, sec[0], back, false, c, res, true));
            }
            if arrow[j] && k == 0 {
                // The step out to the back of the arrowhead.
                rings.push(ring(p, tan, w, sec[j], None, false, c, res, true));
                rings.push(ring(p, tan, w, sec[j], back, false, c, res, true));
                rings.push(ring(p, tan, w, section, back, false, c, res, true));
            }
            rings.push(ring(p, tan, w, section, None, false, c, res, true));
            if last && k + 1 == steps {
                // Close the end.
                rings.push(ring(p, tan, w, section, Some(tan), false, c, res, true));
                rings.push(ring(p, tan, w, section, Some(tan), true, c, res, true));
            }
        }
    }
    stitch(&mut scene.tris, &rings);
}

/// Where the camera is: model space is rotated about `center`, then scaled to pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub rot: M3,
    pub center: V3,
    /// Pixels per ångström at the output size.
    pub scale: f32,
    /// Offset of the centre from the middle of the image, in output pixels.
    pub pan: (f32, f32),
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Premultiplied-free BGRA, opaque.
    pub bgra: Vec<u8>,
    /// The residue drawn at each output pixel, or `NO_RESIDUE`.
    pub residues: Vec<u32>,
}

impl Frame {
    pub fn residue_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return None;
        }
        let r = self.residues[y * self.width + x];
        (r != NO_RESIDUE).then_some(r as usize)
    }
}

/// A sphere on the supersampled screen: centre, depth and radius in pixels.
#[derive(Clone, Copy, Default)]
struct ProjSphere {
    x: f32,
    y: f32,
    z: f32,
    r: f32,
}

/// A triangle on the supersampled screen, as planes over (x, y): its three barycentric
/// weights and its depth. View-space normals are ready for shading.
#[derive(Clone, Copy, Default)]
struct ProjTri {
    bary: [[f32; 3]; 3],
    z: [f32; 3],
    n: [V3; 3],
    /// Pixel bounds (inclusive); `y0 > y1` when the triangle is culled.
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
}

fn plane(p: &[f32; 3], x: f32, y: f32) -> f32 {
    p[0] * x + p[1] * y + p[2]
}

/// Runs `f(index, item)` over `items` on every core, handing out items as threads free up.
fn par_each<T: Send>(items: Vec<T>, f: impl Fn(usize, T) + Sync) {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(items.len().max(1));
    let queue = std::sync::Mutex::new(items.into_iter().enumerate());
    let next = || queue.lock().unwrap().next();
    if threads <= 1 {
        while let Some((i, t)) = next() {
            f(i, t);
        }
        return;
    }
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some((i, t)) = next() {
                    f(i, t);
                }
            });
        }
    });
}

/// Rows of output per unit of parallel work.
const BAND: usize = 16;

/// Draw `scene` into a `width` × `height` image, supersampled `ss` × `ss`. The work is
/// split into horizontal bands across all cores.
pub fn render(scene: &Scene, cam: &Camera, width: usize, height: usize, ss: usize, bg: V3, outline: V3) -> Frame {
    let spheres = &scene.spheres;
    let ns = spheres.len();
    let ss = ss.max(1);
    let (w, h) = (width * ss, height * ss);
    let k = cam.scale * ss as f32;
    let (cx, cy) = (w as f32 / 2.0 + cam.pan.0 * ss as f32, h as f32 / 2.0 + cam.pan.1 * ss as f32);
    let rot = cam.rot;

    // Project. Spheres are few; triangles are projected in parallel chunks.
    let sproj: Vec<ProjSphere> = spheres
        .iter()
        .map(|s| {
            let v = mat_vec(&rot, sub(s.c, cam.center));
            ProjSphere { x: cx + v[0] * k, y: cy - v[1] * k, z: v[2] * k, r: s.r * k }
        })
        .collect();
    let mut tproj = vec![ProjTri::default(); scene.tris.len()];
    let (mut zmin, mut zmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for s in &sproj {
        zmin = zmin.min(s.z - s.r);
        zmax = zmax.max(s.z + s.r);
    }
    {
        const CHUNK: usize = 4096;
        let range = std::sync::Mutex::new((zmin, zmax));
        let chunks: Vec<(&[Tri], &mut [ProjTri])> = scene.tris.chunks(CHUNK).zip(tproj.chunks_mut(CHUNK)).collect();
        par_each(chunks, |_, (tris, out)| {
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for (t, o) in tris.iter().zip(out.iter_mut()) {
                let (p, zlo, zhi) = project_tri(t, &rot, cam.center, k, cx, cy, w, h);
                *o = p;
                lo = lo.min(zlo);
                hi = hi.max(zhi);
            }
            let mut r = range.lock().unwrap();
            *r = (r.0.min(lo), r.1.max(hi));
        });
        (zmin, zmax) = range.into_inner().unwrap();
    }

    // Bin primitives into the bands of output rows they touch.
    let rows_per_band = BAND * ss;
    let nbands = h.div_ceil(rows_per_band).max(1);
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); nbands];
    for (i, s) in sproj.iter().enumerate() {
        if s.r <= 0.0 || s.x + s.r < 0.0 || s.y + s.r < 0.0 || s.x - s.r >= w as f32 || s.y - s.r >= h as f32 {
            continue;
        }
        let y0 = (s.y - s.r).floor().max(0.0) as usize;
        let y1 = ((s.y + s.r).ceil() as usize).min(h - 1);
        for bin in &mut bins[y0 / rows_per_band..=y1 / rows_per_band] {
            bin.push(i as u32);
        }
    }
    for (i, t) in tproj.iter().enumerate() {
        if t.y0 > t.y1 {
            continue;
        }
        for bin in &mut bins[t.y0 as usize / rows_per_band..=t.y1 as usize / rows_per_band] {
            bin.push((ns + i) as u32);
        }
    }

    // Rasterize: the nearest surface per pixel, and what it belongs to.
    let mut depth = vec![f32::NEG_INFINITY; w * h];
    let mut owner = vec![u32::MAX; w * h];
    {
        let bands: Vec<(&mut [f32], &mut [u32])> =
            depth.chunks_mut(rows_per_band * w).zip(owner.chunks_mut(rows_per_band * w)).collect();
        par_each(bands, |b, (depth, owner)| {
            let top = b * rows_per_band;
            let bottom = top + depth.len() / w; // exclusive
            for &id in &bins[b] {
                let id_us = id as usize;
                if id_us < ns {
                    raster_sphere(&sproj[id_us], id, top, bottom, w, depth, owner);
                } else {
                    raster_tri(&tproj[id_us - ns], id, top, bottom, w, depth, owner);
                }
            }
        });
    }

    // Shade each supersampled pixel and average down to the output, band by band.
    let light = normalize([-0.45, 0.55, 0.7]);
    let half_vec = normalize([light[0], light[1], light[2] + 1.0]);
    let zspan = (zmax - zmin).max(1e-3);
    // Neighbours this much nearer (in ångströms) draw an outline: the edges of strands
    // and helices that pass in front of others.
    let edge = 1.6 * k;
    let mut bgra = vec![0u8; width * height * 4];
    let mut residues = vec![NO_RESIDUE; width * height];
    let residue_of = |o: u32| if (o as usize) < ns { spheres[o as usize].residue } else { scene.tris[o as usize - ns].residue };
    let shade = |x: usize, y: usize| -> V3 {
        let ix = y * w + x;
        let o = owner[ix];
        if o == u32::MAX {
            return bg;
        }
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (nrm, base) = if (o as usize) < ns {
            let s = &sproj[o as usize];
            let nx = (px - s.x) / s.r;
            let ny = -(py - s.y) / s.r;
            let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
            ([nx, ny, nz], spheres[o as usize].color)
        } else {
            let ti = o as usize - ns;
            let (t, p) = (&scene.tris[ti], &tproj[ti]);
            let b = [plane(&p.bary[0], px, py).max(0.0), plane(&p.bary[1], px, py).max(0.0), plane(&p.bary[2], px, py).max(0.0)];
            let mut nm = [0.0; 3];
            let mut col = [0.0; 3];
            for v in 0..3 {
                for d in 0..3 {
                    nm[d] += p.n[v][d] * b[v];
                    col[d] += t.color[v][d] * b[v];
                }
            }
            let mut nm = normalize(nm);
            // Ribbons are seen from both sides.
            if nm[2] < 0.0 {
                nm = scale(nm, -1.0);
            }
            (nm, col)
        };
        let diffuse = dot(nrm, light).max(0.0);
        let spec = dot(nrm, half_vec).max(0.0).powi(40) * 0.35;
        let lit = 0.3 + 0.7 * diffuse;
        let mut c = [base[0] * lit + spec, base[1] * lit + spec, base[2] * lit + spec];
        // Depth cue: the far side fades toward the background.
        let z = depth[ix];
        let fog = ((zmax - z) / zspan).clamp(0.0, 1.0);
        c = mix(c, bg, fog * 0.55);
        // Outline where the surface steps back or meets the background.
        let behind = |nx: isize, ny: isize| {
            if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                return true;
            }
            let j = ny as usize * w + nx as usize;
            owner[j] == u32::MAX || z - depth[j] > edge
        };
        let (xi, yi, s) = (x as isize, y as isize, ss as isize);
        if behind(xi + s, yi) || behind(xi - s, yi) || behind(xi, yi + s) || behind(xi, yi - s) {
            c = mix(c, outline, 0.8);
        }
        c
    };
    {
        let bands: Vec<(&mut [u8], &mut [u32])> = bgra.chunks_mut(BAND * width * 4).zip(residues.chunks_mut(BAND * width)).collect();
        let inv = 1.0 / (ss * ss) as f32;
        let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        par_each(bands, |b, (bgra, residues)| {
            let y_out0 = b * BAND;
            for (ry, (row_px, row_res)) in bgra.chunks_mut(width * 4).zip(residues.chunks_mut(width)).enumerate() {
                let y = y_out0 + ry;
                for x in 0..width {
                    let mut acc = [0.0f32; 3];
                    for sy in 0..ss {
                        for sx in 0..ss {
                            let c = shade(x * ss + sx, y * ss + sy);
                            acc[0] += c[0];
                            acc[1] += c[1];
                            acc[2] += c[2];
                        }
                    }
                    let o = x * 4;
                    row_px[o] = to_byte(acc[2] * inv);
                    row_px[o + 1] = to_byte(acc[1] * inv);
                    row_px[o + 2] = to_byte(acc[0] * inv);
                    row_px[o + 3] = 255;
                    let centre = owner[(y * ss + ss / 2) * w + x * ss + ss / 2];
                    if centre != u32::MAX {
                        row_res[x] = residue_of(centre);
                    }
                }
            }
        });
    }
    Frame { width, height, bgra, residues }
}

#[allow(clippy::too_many_arguments)]
/// The triangle on screen, and the range of depths it spans (culled or not, so the
/// fog doesn't shift as faces turn away).
fn project_tri(t: &Tri, rot: &M3, center: V3, k: f32, cx: f32, cy: f32, w: usize, h: usize) -> (ProjTri, f32, f32) {
    let mut out = ProjTri { y0: 1, y1: 0, ..Default::default() };
    let mut s = [[0.0f32; 3]; 3];
    for (sv, p) in s.iter_mut().zip(t.v) {
        let v = mat_vec(rot, sub(p, center));
        *sv = [cx + v[0] * k, cy - v[1] * k, v[2] * k];
    }
    let zlo = s[0][2].min(s[1][2]).min(s[2][2]);
    let zhi = s[0][2].max(s[1][2]).max(s[2][2]);
    let [a, b, c] = s;
    let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    // The cartoon is a closed surface wound outward, so faces turned away (positive
    // area once y points down) are hidden behind others; skip them and slivers.
    if area > -1e-6 {
        return (out, zlo, zhi);
    }
    let xmin = a[0].min(b[0]).min(c[0]);
    let xmax = a[0].max(b[0]).max(c[0]);
    let ymin = a[1].min(b[1]).min(c[1]);
    let ymax = a[1].max(b[1]).max(c[1]);
    if xmax < 0.5 || ymax < 0.5 || xmin >= w as f32 - 0.5 || ymin >= h as f32 - 0.5 {
        return (out, zlo, zhi);
    }
    let x0 = (xmin - 0.5).ceil().max(0.0);
    let x1 = (xmax - 0.5).floor().min(w as f32 - 1.0);
    let y0 = (ymin - 0.5).ceil().max(0.0);
    let y1 = (ymax - 0.5).floor().min(h as f32 - 1.0);
    if x0 > x1 || y0 > y1 {
        return (out, zlo, zhi);
    }
    // Weight of vertex i: the edge opposite it, over the area.
    let inv = 1.0 / area;
    let edge = |p: V3, q: V3| [(p[1] - q[1]) * inv, (q[0] - p[0]) * inv, (p[0] * q[1] - p[1] * q[0]) * inv];
    let bary = [edge(b, c), edge(c, a), edge(a, b)];
    let mut z = [0.0; 3];
    for (v, wv) in bary.iter().enumerate() {
        for d in 0..3 {
            z[d] += wv[d] * s[v][2];
        }
    }
    out.bary = bary;
    out.z = z;
    out.n = t.n.map(|n| mat_vec(rot, n));
    out.x0 = x0 as u32;
    out.x1 = x1 as u32;
    out.y0 = y0 as u32;
    out.y1 = y1 as u32;
    (out, zlo, zhi)
}

fn raster_tri(t: &ProjTri, id: u32, top: usize, bottom: usize, w: usize, depth: &mut [f32], owner: &mut [u32]) {
    let y0 = (t.y0 as usize).max(top);
    let y1 = (t.y1 as usize).min(bottom - 1);
    for y in y0..=y1 {
        let py = y as f32 + 0.5;
        // Solve each weight >= 0 for the span of x it allows on this row.
        let (mut lo, mut hi) = (t.x0 as f32 + 0.5, t.x1 as f32 + 0.5);
        for e in &t.bary {
            let c = e[1] * py + e[2];
            if e[0].abs() < 1e-12 {
                if c < 0.0 {
                    lo = f32::INFINITY;
                }
            } else if e[0] > 0.0 {
                lo = lo.max(-c / e[0]);
            } else {
                hi = hi.min(-c / e[0]);
            }
        }
        if lo > hi {
            continue;
        }
        let xa = ((lo - 0.5).ceil().max(t.x0 as f32)) as usize;
        let xb = ((hi - 0.5).floor().min(t.x1 as f32)) as usize;
        if xa > xb {
            continue;
        }
        let row = (y - top) * w;
        let mut z = plane(&t.z, xa as f32 + 0.5, py);
        let dz = t.z[0];
        for x in xa..=xb {
            let ix = row + x;
            if z > depth[ix] {
                depth[ix] = z;
                owner[ix] = id;
            }
            z += dz;
        }
    }
}

fn raster_sphere(s: &ProjSphere, id: u32, top: usize, bottom: usize, w: usize, depth: &mut [f32], owner: &mut [u32]) {
    let y0 = ((s.y - s.r).floor().max(0.0) as usize).max(top);
    let y1 = ((s.y + s.r).ceil() as usize).min(bottom - 1);
    let r2 = s.r * s.r;
    for y in y0..=y1 {
        let dy = y as f32 + 0.5 - s.y;
        let half2 = r2 - dy * dy;
        if half2 <= 0.0 {
            continue;
        }
        let half = half2.sqrt();
        let x0 = (s.x - half).ceil().max(0.0) as usize;
        let x1 = ((s.x + half).floor().max(0.0) as usize).min(w - 1);
        let row = (y - top) * w;
        for x in x0..=x1 {
            let dx = x as f32 + 0.5 - s.x;
            let d2 = half2 - dx * dx;
            if d2 < 0.0 {
                continue;
            }
            let z = s.z + d2.sqrt();
            let ix = row + x;
            if z > depth[ix] {
                depth[ix] = z;
                owner[ix] = id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_spheres_win_and_are_pickable() {
        let spheres = [
            Sphere { c: [0.0, 0.0, 0.0], r: 2.0, color: [1.0, 0.0, 0.0], residue: 7 },
            Sphere { c: [0.0, 0.0, 5.0], r: 1.0, color: [0.0, 0.0, 1.0], residue: 9 },
        ];
        let cam = Camera { rot: IDENTITY, center: [0.0; 3], scale: 10.0, pan: (0.0, 0.0) };
        let f = render(&Scene { spheres: spheres.to_vec(), tris: Vec::new() }, &cam, 64, 64, 2, [0.0; 3], [0.0; 3]);
        assert_eq!(f.residue_at(32.0, 32.0), Some(9), "the front sphere covers the middle");
        assert_eq!(f.residue_at(32.0 + 15.0, 32.0), Some(7));
        assert_eq!(f.residue_at(1.0, 1.0), None);
        // The middle is blue-ish (BGRA: blue first) and the background black.
        let px = |x: usize, y: usize| &f.bgra[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert!(px(32, 30)[0] > px(32, 30)[2]);
        assert_eq!(px(0, 0), &[0, 0, 0, 255]);
    }

    #[test]
    fn triangles_depth_test_against_spheres() {
        let n = [0.0, 0.0, 1.0];
        let tri = Tri { v: [[-3.0, -3.0, 2.0], [3.0, -3.0, 2.0], [0.0, 3.0, 2.0]], n: [n; 3], color: [[0.0, 1.0, 0.0]; 3], residue: 4 };
        let sphere = Sphere { c: [0.0, 0.0, 0.0], r: 1.0, color: [1.0, 0.0, 0.0], residue: 7 };
        let cam = Camera { rot: IDENTITY, center: [0.0; 3], scale: 10.0, pan: (0.0, 0.0) };
        let f = render(&Scene { spheres: vec![sphere], tris: vec![tri] }, &cam, 64, 64, 2, [0.0; 3], [0.0; 3]);
        assert_eq!(f.residue_at(32.0, 32.0), Some(4), "the triangle is in front of the sphere");
        assert_eq!(f.residue_at(5.0, 5.0), None, "beside the triangle's apex");
        let px = &f.bgra[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 4];
        assert!(px[1] > px[2] && px[1] > px[0], "lit green: {px:?}");
    }

    #[test]
    fn cartoon_meshes_egfp() {
        use crate::pgx::StructureFormat;
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/2Y0G.pdb")).unwrap();
        let mol = Molecule::parse(&bytes, StructureFormat::Pdb).unwrap();
        assert!(mol.residues.iter().filter(|r| r.carbonyl.is_some()).count() > 200);
        let colors = residue_colors(&mol, Coloring::Secondary, false, &[]);
        let scene = build_scene(&mol, Style::Cartoon, &colors, &[], [1.0; 3]);
        assert!(scene.tris.len() > 10_000);
        let finite = |v: &V3| v.iter().all(|c| c.is_finite());
        for t in &scene.tris {
            assert!((t.residue as usize) < mol.residues.len());
            assert!(t.v.iter().all(finite) && t.n.iter().all(finite) && t.color.iter().all(finite));
        }
    }

    #[test]
    fn rotations_stay_orthonormal() {
        let mut r = IDENTITY;
        for i in 0..1000 {
            r = rotate(&r, 0.01 * i as f32, 0.013);
        }
        let rt = [[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]];
        let p = mat_mul(&r, &rt);
        for (i, row) in p.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn principal_axis_lies_along_x() {
        let pts: Vec<V3> = (0..50).map(|i| [i as f32 * 0.3, i as f32 * 0.3, (i % 3) as f32 * 0.1]).collect();
        let c = [7.35, 7.35, 0.1];
        let r = principal_axes(&pts, c);
        let along = mat_vec(&r, normalize([1.0, 1.0, 0.0]));
        assert!(along[0].abs() > 0.99, "{along:?}");
    }
}

/// `cargo test -- --ignored render_preview` writes `$RENDER_OUT/2y0g-*.png`: a look at
/// the real EGFP structure in each style, without starting the app.
#[cfg(test)]
mod preview {
    use super::*;
    use crate::pgx::StructureFormat;

    #[test]
    #[ignore]
    fn render_preview() {
        let dir = std::env::var("RENDER_OUT").expect("set RENDER_OUT to a directory");
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/2Y0G.pdb")).unwrap();
        let mol = Molecule::parse(&bytes, StructureFormat::Pdb).unwrap();
        let trace: Vec<V3> = mol.residues.iter().filter_map(|r| r.trace).map(|t| mol.atoms[t].pos).collect();
        let n = trace.len() as f32;
        let center = (0..3).map(|d| trace.iter().map(|p| p[d]).sum::<f32>() / n).collect::<Vec<_>>();
        let center = [center[0], center[1], center[2]];
        let rot = principal_axes(&trace, center);
        let bg = [0.12, 0.12, 0.14];
        let mut highlight = vec![false; mol.residues.len()];
        for (r, res) in mol.residues.iter().enumerate() {
            highlight[r] = (60..=70).contains(&res.seq_num);
        }
        for (name, style, coloring) in [
            ("cartoon-rainbow", Style::Cartoon, Coloring::Rainbow),
            ("cartoon-ss", Style::Cartoon, Coloring::Secondary),
            ("tube-rainbow", Style::Tube, Coloring::Rainbow),
            ("tube-ss", Style::Tube, Coloring::Secondary),
            ("atoms-rainbow", Style::Atoms, Coloring::Rainbow),
        ] {
            let colors = residue_colors(&mol, coloring, false, &[]);
            let scene = build_scene(&mol, style, &colors, &highlight, [1.0, 0.8, 0.2]);
            let zoom: f32 = std::env::var("RENDER_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(9.0);
            let cam = Camera { rot, center, scale: zoom, pan: (0.0, 0.0) };
            let t = std::time::Instant::now();
            let f = render(&scene, &cam, 800, 600, 2, bg, [0.03; 3]);
            eprintln!("{name}: {} spheres, {} triangles, {:?}", scene.spheres.len(), scene.tris.len(), t.elapsed());
            let rgba: Vec<u8> = f.bgra.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            image::save_buffer(format!("{dir}/2y0g-{name}.png"), &rgba, 800, 600, image::ColorType::Rgba8).unwrap();
        }
    }
}

/// `cargo test --release -- --ignored render_bench --nocapture`: milliseconds per frame
/// for the EGFP structure while it turns, at a typical panel size.
#[cfg(test)]
mod bench {
    use super::*;
    use crate::pgx::StructureFormat;

    #[test]
    #[ignore]
    fn render_bench() {
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/2Y0G.pdb")).unwrap();
        let mol = Molecule::parse(&bytes, StructureFormat::Pdb).unwrap();
        let trace: Vec<V3> = mol.residues.iter().filter_map(|r| r.trace).map(|t| mol.atoms[t].pos).collect();
        let n = trace.len() as f32;
        let center = [0, 1, 2].map(|d| trace.iter().map(|p| p[d]).sum::<f32>() / n);
        let (w, h) = (1600, 1000);
        for style in [Style::Cartoon, Style::Tube, Style::Atoms] {
            let colors = residue_colors(&mol, Coloring::Rainbow, false, &[]);
            let t = std::time::Instant::now();
            let scene = build_scene(&mol, style, &colors, &[], [1.0; 3]);
            let build = t.elapsed();
            for ss in [1, 2] {
                let mut rot = principal_axes(&trace, center);
                let frames = 20;
                let t = std::time::Instant::now();
                for _ in 0..frames {
                    rot = rotate(&rot, 0.05, 0.0);
                    let cam = Camera { rot, center, scale: h as f32 * 0.92 / 50.0, pan: (0.0, 0.0) };
                    std::hint::black_box(render(&scene, &cam, w, h, ss, [0.1; 3], [0.0; 3]));
                }
                eprintln!("{style:?} ss={ss}: {:.1} ms/frame (scene built in {build:?})", t.elapsed().as_secs_f64() * 1000.0 / frames as f64);
            }
        }
    }
}

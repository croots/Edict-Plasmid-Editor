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

/// Model-space spheres for `mol`. `highlight` residues are tinted with `accent`.
pub fn build_spheres(mol: &Molecule, style: Style, colors: &[V3], highlight: &[bool], accent: V3) -> Vec<Sphere> {
    let tint = |r: usize, c: V3| if highlight.get(r).copied().unwrap_or(false) { mix(c, accent, HIGHLIGHT_MIX) } else { c };
    let mut out = Vec::new();
    match style {
        Style::Atoms => {
            for a in &mol.atoms {
                let res = &mol.residues[a.residue];
                // Ligands keep element colours whatever the scheme.
                let base = if res.polymer { colors[a.residue] } else { element_color(&a.element) };
                out.push(Sphere { c: a.pos, r: element_radius(&a.element), color: tint(a.residue, base), residue: a.residue as u32 });
            }
        }
        Style::Tube => {
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
                            tube(&mut out, &run, mol, colors, &tint);
                            run.clear();
                        }
                    }
                    run.push((p, r));
                    prev = Some(r);
                }
                tube(&mut out, &run, mol, colors, &tint);
            }
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
    out
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

/// Draw `spheres` into a `width` × `height` image, supersampled `ss` × `ss`.
pub fn render(spheres: &[Sphere], cam: &Camera, width: usize, height: usize, ss: usize, bg: V3, outline: V3) -> Frame {
    let ss = ss.max(1);
    let (w, h) = (width * ss, height * ss);
    let n = w * h;
    let mut depth = vec![f32::NEG_INFINITY; n];
    let mut owner = vec![u32::MAX; n];
    let k = cam.scale * ss as f32;
    let (cx, cy) = (w as f32 / 2.0 + cam.pan.0 * ss as f32, h as f32 / 2.0 + cam.pan.1 * ss as f32);

    // Project, then splat each sphere's disc, keeping the nearest surface per pixel.
    let mut proj: Vec<(f32, f32, f32, f32)> = Vec::with_capacity(spheres.len());
    let (mut zmin, mut zmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for (i, s) in spheres.iter().enumerate() {
        let v = mat_vec(&cam.rot, sub(s.c, cam.center));
        let (sx, sy, sz, rp) = (cx + v[0] * k, cy - v[1] * k, v[2] * k, s.r * k);
        proj.push((sx, sy, sz, rp));
        zmin = zmin.min(sz - rp);
        zmax = zmax.max(sz + rp);
        if rp <= 0.0 || sx + rp < 0.0 || sy + rp < 0.0 || sx - rp >= w as f32 || sy - rp >= h as f32 {
            continue;
        }
        let y0 = (sy - rp).floor().max(0.0) as usize;
        let y1 = ((sy + rp).ceil() as usize).min(h - 1);
        let r2 = rp * rp;
        for y in y0..=y1 {
            let dy = y as f32 + 0.5 - sy;
            let half2 = r2 - dy * dy;
            if half2 <= 0.0 {
                continue;
            }
            let half = half2.sqrt();
            let x0 = (sx - half).ceil().max(0.0) as usize;
            let x1 = ((sx + half).floor().max(0.0) as usize).min(w - 1);
            let row = y * w;
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - sx;
                let d2 = half2 - dx * dx;
                if d2 < 0.0 {
                    continue;
                }
                let z = sz + d2.sqrt();
                let ix = row + x;
                if z > depth[ix] {
                    depth[ix] = z;
                    owner[ix] = i as u32;
                }
            }
        }
    }

    // Light from the upper left, in front.
    let light = normalize([-0.45, 0.55, 0.7]);
    let half_vec = normalize([light[0], light[1], light[2] + 1.0]);
    let zspan = (zmax - zmin).max(1e-3);
    // Neighbours this much nearer (in ångströms) draw an outline: the edges of strands
    // and helices that pass in front of others.
    let edge = 1.6 * k;
    let mut shaded = vec![bg; n];
    for y in 0..h {
        for x in 0..w {
            let ix = y * w + x;
            let o = owner[ix];
            if o == u32::MAX {
                continue;
            }
            let (sx, sy, _, rp) = proj[o as usize];
            let nx = (x as f32 + 0.5 - sx) / rp;
            let ny = -(y as f32 + 0.5 - sy) / rp;
            let nz = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
            let nrm = [nx, ny, nz];
            let diffuse = dot(nrm, light).max(0.0);
            let spec = dot(nrm, half_vec).max(0.0).powi(40) * 0.35;
            let base = spheres[o as usize].color;
            let lit = 0.3 + 0.7 * diffuse;
            let mut c = [base[0] * lit + spec, base[1] * lit + spec, base[2] * lit + spec];
            // Depth cue: the far side fades toward the background.
            let fog = ((zmax - depth[ix]) / zspan).clamp(0.0, 1.0);
            c = mix(c, bg, fog * 0.55);
            // Outline where the surface steps back or meets the background.
            let z = depth[ix];
            let step = ss;
            let behind = |nx: isize, ny: isize| {
                if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                    return true;
                }
                let j = ny as usize * w + nx as usize;
                owner[j] == u32::MAX || z - depth[j] > edge
            };
            let (xi, yi, s) = (x as isize, y as isize, step as isize);
            if behind(xi + s, yi) || behind(xi - s, yi) || behind(xi, yi + s) || behind(xi, yi - s) {
                c = mix(c, outline, 0.8);
            }
            shaded[ix] = c;
        }
    }

    // Downsample to the output size.
    let mut bgra = vec![0u8; width * height * 4];
    let mut residues = vec![NO_RESIDUE; width * height];
    let inv = 1.0 / (ss * ss) as f32;
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0.0f32; 3];
            for sy in 0..ss {
                let row = (y * ss + sy) * w + x * ss;
                for sx in 0..ss {
                    let c = shaded[row + sx];
                    acc[0] += c[0];
                    acc[1] += c[1];
                    acc[2] += c[2];
                }
            }
            let o = (y * width + x) * 4;
            bgra[o] = to_byte(acc[2] * inv);
            bgra[o + 1] = to_byte(acc[1] * inv);
            bgra[o + 2] = to_byte(acc[0] * inv);
            bgra[o + 3] = 255;
            let centre = owner[(y * ss + ss / 2) * w + x * ss + ss / 2];
            if centre != u32::MAX {
                residues[y * width + x] = spheres[centre as usize].residue;
            }
        }
    }
    Frame { width, height, bgra, residues }
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
        let f = render(&spheres, &cam, 64, 64, 2, [0.0; 3], [0.0; 3]);
        assert_eq!(f.residue_at(32.0, 32.0), Some(9), "the front sphere covers the middle");
        assert_eq!(f.residue_at(32.0 + 15.0, 32.0), Some(7));
        assert_eq!(f.residue_at(1.0, 1.0), None);
        // The middle is blue-ish (BGRA: blue first) and the background black.
        let px = |x: usize, y: usize| &f.bgra[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert!(px(32, 30)[0] > px(32, 30)[2]);
        assert_eq!(px(0, 0), &[0, 0, 0, 255]);
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
            ("tube-rainbow", Style::Tube, Coloring::Rainbow),
            ("tube-ss", Style::Tube, Coloring::Secondary),
            ("atoms-rainbow", Style::Atoms, Coloring::Rainbow),
        ] {
            let colors = residue_colors(&mol, coloring, false, &[]);
            let spheres = build_spheres(&mol, style, &colors, &highlight, [1.0, 0.8, 0.2]);
            let cam = Camera { rot, center, scale: 9.0, pan: (0.0, 0.0) };
            let t = std::time::Instant::now();
            let f = render(&spheres, &cam, 800, 600, 2, bg, [0.03; 3]);
            eprintln!("{name}: {} spheres, {:?}", spheres.len(), t.elapsed());
            let rgba: Vec<u8> = f.bgra.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            image::save_buffer(format!("{dir}/2y0g-{name}.png"), &rgba, 800, 600, image::ColorType::Rgba8).unwrap();
        }
    }
}

//! Secondary structure from backbone coordinates, for files that don't declare any
//! (predicted structures from ESMFold, AlphaFold and the like).
//!
//! This follows DSSP (Kabsch & Sander, 1983): backbone hydrogen bonds are found with
//! an electrostatic energy, two consecutive i→i+4 turns make an alpha helix, and a
//! ladder of two or more bridges makes a strand. 3₁₀ helices are drawn as helix too;
//! π helices and isolated bridges are left as coil, which is how cartoon views show them.

use crate::structure::Ss;

/// A residue's backbone: N, CA, C, O.
pub type Backbone = [[f32; 3]; 4];
const N: usize = 0;
const CA: usize = 1;
const C: usize = 2;
const O: usize = 3;

/// DSSP's bond threshold, kcal/mol.
const HBOND_MAX: f32 = -0.5;
/// Alpha carbons further apart than this can't be hydrogen bonded.
const CA_CUTOFF: f32 = 9.0;
/// A peptide bond (C to the next N) is about 1.33 Å; longer means a chain break.
const PEPTIDE_MAX: f32 = 2.5;

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = sub(a, b);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Secondary structure for residues given as (chain, backbone) in chain order; the
/// result is one entry per residue. Residues of one chain must be listed together.
pub fn assign(residues: &[(usize, Backbone)]) -> Vec<Ss> {
    let n = residues.len();
    let bb: Vec<Backbone> = residues.iter().map(|r| r.1).collect();
    // seg[k]: residues k and k+1 are joined by a peptide bond when seg matches.
    let mut seg = vec![0usize; n];
    for k in 1..n {
        let joined = residues[k].0 == residues[k - 1].0 && dist(bb[k - 1][C], bb[k][N]) < PEPTIDE_MAX;
        seg[k] = seg[k - 1] + usize::from(!joined);
    }
    // The amide hydrogen, placed 1 Å from N opposite the previous carbonyl.
    let h: Vec<Option<[f32; 3]>> = (0..n)
        .map(|k| {
            if k == 0 || seg[k - 1] != seg[k] {
                return None;
            }
            let co = sub(bb[k - 1][C], bb[k - 1][O]);
            let len = dist(bb[k - 1][C], bb[k - 1][O]).max(1e-3);
            Some([bb[k][N][0] + co[0] / len, bb[k][N][1] + co[1] / len, bb[k][N][2] + co[2] / len])
        })
        .collect();

    // hb[i] holds every j whose N-H donates to i's C=O ("i accepts from j").
    let mut hb: Vec<Vec<usize>> = vec![Vec::new(); n];
    // Bucket alpha carbons into a grid so this isn't quadratic on large structures.
    let cell = |p: [f32; 3]| [(p[0] / CA_CUTOFF).floor() as i32, (p[1] / CA_CUTOFF).floor() as i32, (p[2] / CA_CUTOFF).floor() as i32];
    let mut grid: std::collections::HashMap<[i32; 3], Vec<usize>> = std::collections::HashMap::new();
    for (k, b) in bb.iter().enumerate() {
        grid.entry(cell(b[CA])).or_default().push(k);
    }
    for i in 0..n {
        let c = cell(bb[i][CA]);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(near) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) else { continue };
                    for &j in near {
                        if j == i || (seg[i] == seg[j] && i.abs_diff(j) < 2) {
                            continue;
                        }
                        let Some(hj) = h[j] else { continue };
                        if dist(bb[i][CA], bb[j][CA]) > CA_CUTOFF {
                            continue;
                        }
                        let (o, cc, nn) = (bb[i][O], bb[i][C], bb[j][N]);
                        let e = 0.084 * 332.0
                            * (1.0 / dist(o, nn) + 1.0 / dist(cc, hj) - 1.0 / dist(o, hj) - 1.0 / dist(cc, nn));
                        if e < HBOND_MAX {
                            hb[i].push(j);
                        }
                    }
                }
            }
        }
    }
    // rev[j]: every i whose C=O j's N-H donates to.
    let mut rev: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, donors) in hb.iter().enumerate() {
        for &j in donors {
            rev[j].push(i);
        }
    }
    // bond(i, j): i's C=O accepts from j's N-H.
    let bond = |i: usize, j: usize| i < n && j < n && hb[i].contains(&j);
    let same = |a: usize, b: usize| a < n && b < n && seg[a] == seg[b];

    let mut ss = vec![Ss::Coil; n];
    // Alpha helices: 4-turns at i-1 and i make i..i+3 helical.
    let turn4 = |i: usize| same(i, i + 4) && bond(i, i + 4);
    for i in 1..n {
        if turn4(i - 1) && turn4(i) {
            for s in &mut ss[i..(i + 4).min(n)] {
                *s = Ss::Helix;
            }
        }
    }

    // Bridges between i and j (not near neighbours on one chain): parallel or not.
    let mut bridges: Vec<Vec<(usize, bool)>> = vec![Vec::new(); n];
    let inner = |k: usize| k >= 1 && same(k - 1, k) && same(k, k + 1);
    for i in 0..n {
        if !inner(i) {
            continue;
        }
        // Every bridge pattern below bonds j (or j±1) to i (or i±1), in one direction
        // or the other, so its partners are found from those residues' bonds.
        let mut cands: Vec<usize> = Vec::new();
        for k in [i - 1, i, i + 1] {
            for &j in hb[k].iter().chain(&rev[k]) {
                cands.extend([j.saturating_sub(1), j, j + 1]);
            }
        }
        cands.sort_unstable();
        cands.dedup();
        for j in cands {
            if j >= n || !inner(j) || (seg[i] == seg[j] && i.abs_diff(j) < 3) {
                continue;
            }
            let parallel = (bond(i - 1, j) && bond(j, i + 1)) || (bond(j - 1, i) && bond(i, j + 1));
            let anti = (bond(i, j) && bond(j, i)) || (bond(i - 1, j + 1) && bond(j - 1, i + 1));
            for (yes, p) in [(parallel, true), (anti, false)] {
                if yes && !bridges[i].contains(&(j, p)) {
                    bridges[i].push((j, p));
                }
            }
        }
    }
    // A strand residue is in a bridge that continues along a ladder to a neighbour.
    let mut strand = vec![false; n];
    for i in 0..n {
        for &(j, p) in &bridges[i] {
            let next = |a: usize, b: Option<usize>| b.is_some_and(|b| same(i, a) && bridges[a].contains(&(b, p)));
            let (up, down) = if p { (j.checked_add(1), j.checked_sub(1)) } else { (j.checked_sub(1), j.checked_add(1)) };
            if next(i + 1, up) || (i >= 1 && next(i - 1, down)) {
                strand[i] = true;
            }
        }
    }
    // Close single-residue gaps (beta bulges) within a strand.
    for i in 1..n.saturating_sub(1) {
        if !strand[i] && strand[i - 1] && strand[i + 1] && same(i - 1, i + 1) && !bridges[i - 1].is_empty() {
            strand[i] = true;
        }
    }
    for i in 0..n {
        if strand[i] && ss[i] == Ss::Coil {
            ss[i] = Ss::Strand;
        }
    }
    // 3₁₀ helices, the same way from 3-turns, where nothing else has been assigned:
    // the authors of deposited structures, and PyMOL, draw them as helix too.
    let turn3 = |i: usize| same(i, i + 3) && bond(i, i + 3);
    for i in 1..n {
        if turn3(i - 1) && turn3(i) && ss[i..(i + 3).min(n)].iter().all(|s| *s == Ss::Coil) {
            for s in &mut ss[i..(i + 3).min(n)] {
                *s = Ss::Helix;
            }
        }
    }
    ss
}

#[cfg(test)]
mod tests {
    use crate::pgx::StructureFormat;
    use crate::structure::{Molecule, Ss};

    /// GFP (2Y0G) with its HELIX/SHEET records removed comes out close to the authors'
    /// assignment: an eleven-stranded barrel with a few short helices.
    #[test]
    fn matches_the_authors_on_gfp() {
        let pdb = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/2Y0G.pdb")).unwrap();
        let authors = Molecule::parse(pdb.as_bytes(), StructureFormat::Pdb).unwrap();
        let bare: String = pdb.lines().filter(|l| !l.starts_with("HELIX") && !l.starts_with("SHEET")).map(|l| format!("{l}\n")).collect();
        let ours = Molecule::parse(bare.as_bytes(), StructureFormat::Pdb).unwrap();
        let polymer: Vec<(Ss, Ss)> =
            authors.residues.iter().zip(&ours.residues).filter(|(a, _)| a.polymer).map(|(a, o)| (a.ss, o.ss)).collect();
        let count = |f: &dyn Fn(&(Ss, Ss)) -> bool| polymer.iter().filter(|p| f(p)).count();
        let agree = count(&|(a, o)| a == o) as f32 / polymer.len() as f32;
        let (strand_a, strand_o) = (count(&|p| p.0 == Ss::Strand), count(&|p| p.1 == Ss::Strand));
        let (helix_a, helix_o) = (count(&|p| p.0 == Ss::Helix), count(&|p| p.1 == Ss::Helix));
        eprintln!("agree {agree:.3}; strand {strand_a} vs {strand_o}; helix {helix_a} vs {helix_o}");
        assert!(agree > 0.9, "agreement {agree}");
        assert!(strand_o as f32 > 0.8 * strand_a as f32);
        assert!(helix_o > 0);
    }

    /// A file that declares its secondary structure keeps it.
    #[test]
    fn declared_structure_wins() {
        let pdb = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/2Y0G.pdb")).unwrap();
        let only_helices: String = pdb.lines().filter(|l| !l.starts_with("SHEET")).map(|l| format!("{l}\n")).collect();
        let m = Molecule::parse(only_helices.as_bytes(), StructureFormat::Pdb).unwrap();
        assert!(m.residues.iter().all(|r| r.ss != Ss::Strand));
    }
}

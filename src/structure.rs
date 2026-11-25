//! Macromolecular structures: parsing PDB and mmCIF coordinates into a small model the
//! viewer draws, and linking protein chains to the CDS features that encode them.
//!
//! Only what a viewer needs is read: the first model's atoms (first alternate location
//! only, waters dropped), residues, chains and the author's secondary structure. Links
//! are recomputed from sequences rather than stored, so they survive edits: a chain is
//! tied to whichever CDS its residues align to best.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use anyhow::{bail, Result};

use crate::model::{self, Feature};
use crate::pgx::{self, ChainLinking, Structure, StructureFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ss {
    Coil,
    Helix,
    Strand,
}

#[derive(Clone, Debug)]
pub struct Atom {
    pub pos: [f32; 3],
    /// Upper-case element symbol, e.g. `C`, `FE`.
    pub element: String,
    pub residue: usize,
    pub b_factor: f32,
}

#[derive(Clone, Debug)]
pub struct Residue {
    pub chain: usize,
    pub name: String,
    /// Author numbering and insertion code, as shown to people.
    pub seq_num: i32,
    pub icode: char,
    /// Part of the chain's polymer (amino acids, nucleotides, modified residues).
    pub polymer: bool,
    /// One-letter code(s) in the chain sequence. Usually one letter; a chromophore
    /// formed from three residues (GFP's `CRO`) stands for all three.
    pub letters: String,
    pub ss: Ss,
    /// The atom the backbone trace passes through (`CA`, or `P` for nucleic acids).
    pub trace: Option<usize>,
    /// The backbone carbonyl oxygen (`O`) of an amino acid, which orients cartoon ribbons.
    pub carbonyl: Option<usize>,
    pub atoms: Range<usize>,
}

impl Residue {
    /// A polymer residue that isn't one of the standard amino acids or nucleotides:
    /// modified residues and chromophores, which the tube view draws as atoms too.
    pub fn is_modified(&self) -> bool {
        const STANDARD: [&str; 29] = [
            "ALA", "ARG", "ASN", "ASP", "CYS", "GLN", "GLU", "GLY", "HIS", "ILE", "LEU", "LYS", "MET", "PHE", "PRO", "SER",
            "THR", "TRP", "TYR", "VAL", "DA", "DC", "DG", "DT", "A", "C", "G", "U", "I",
        ];
        self.polymer && !STANDARD.contains(&self.name.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct Chain {
    pub id: String,
    pub residues: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Molecule {
    pub title: String,
    pub atoms: Vec<Atom>,
    pub residues: Vec<Residue>,
    pub chains: Vec<Chain>,
}

impl Molecule {
    pub fn parse(bytes: &[u8], format: StructureFormat) -> Result<Molecule> {
        let text = String::from_utf8_lossy(bytes);
        let mut raw = match format {
            StructureFormat::Pdb => parse_pdb(&text),
            StructureFormat::Mmcif => parse_cif(&text)?,
        };
        if raw.atoms.is_empty() {
            bail!("no atoms found");
        }
        raw.atoms.retain(|a| !is_water(&a.res_name));
        Ok(build(raw))
    }

    pub fn residue_label(&self, r: usize) -> String {
        let res = &self.residues[r];
        let chain = &self.chains[res.chain].id;
        let icode = if res.icode == ' ' { String::new() } else { res.icode.to_string() };
        format!("{chain}:{} {}{icode}", title_case(&res.name), res.seq_num)
    }
}

/// Guess the format from a file name, then the contents.
pub fn sniff_format(name: &str, bytes: &[u8]) -> Option<StructureFormat> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".cif") || lower.ends_with(".mmcif") {
        return Some(StructureFormat::Mmcif);
    }
    if lower.ends_with(".pdb") || lower.ends_with(".ent") {
        return Some(StructureFormat::Pdb);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    if head.lines().any(|l| l.starts_with("data_")) {
        return Some(StructureFormat::Mmcif);
    }
    if head.lines().any(|l| ["HEADER", "ATOM  ", "HETATM", "REMARK", "CRYST1", "MODEL "].iter().any(|k| l.starts_with(k))) {
        return Some(StructureFormat::Pdb);
    }
    None
}

// ----- parsing ---------------------------------------------------------------------

struct RawAtom {
    name: String,
    res_name: String,
    chain: String,
    seq_num: i32,
    icode: char,
    pos: [f32; 3],
    element: String,
    b_factor: f32,
    hetero: bool,
    /// mmCIF only: whether the atom belongs to the polymer (`label_seq_id` is set).
    polymer: Option<bool>,
}

/// A secondary-structure range in author numbering: chain, (start, icode), (end, icode).
type SsRange = (String, (i32, char), (i32, char), Ss);

#[derive(Default)]
struct Raw {
    title: String,
    atoms: Vec<RawAtom>,
    ss: Vec<SsRange>,
    /// Residue names in each chain's SEQRES, to tell modified residues from ligands.
    seqres: HashMap<String, HashSet<String>>,
}

fn is_water(name: &str) -> bool {
    matches!(name, "HOH" | "WAT" | "DOD" | "H2O" | "SOL")
}

fn col(line: &str, r: Range<usize>) -> &str {
    line.get(r.start.min(line.len())..r.end.min(line.len())).unwrap_or("").trim()
}

fn parse_pdb(text: &str) -> Raw {
    let mut raw = Raw::default();
    let mut models = 0;
    for line in text.lines() {
        let rec = col(line, 0..6);
        match rec {
            "MODEL" => {
                models += 1;
                if models > 1 {
                    break;
                }
            }
            "ENDMDL" => break,
            "TITLE" => {
                let t = col(line, 10..80);
                if !raw.title.is_empty() {
                    raw.title.push(' ');
                }
                raw.title.push_str(t);
            }
            "SEQRES" => {
                let set = raw.seqres.entry(col(line, 11..12).to_string()).or_default();
                set.extend(col(line, 19..80).split_whitespace().map(str::to_string));
            }
            "HELIX" => {
                let chain = col(line, 19..20).to_string();
                let start = (col(line, 21..25).parse().unwrap_or(0), icode(line, 25));
                let end = (col(line, 33..37).parse().unwrap_or(0), icode(line, 37));
                raw.ss.push((chain, start, end, Ss::Helix));
            }
            "SHEET" => {
                let chain = col(line, 21..22).to_string();
                let start = (col(line, 22..26).parse().unwrap_or(0), icode(line, 26));
                let end = (col(line, 33..37).parse().unwrap_or(0), icode(line, 37));
                raw.ss.push((chain, start, end, Ss::Strand));
            }
            "ATOM" | "HETATM" => {
                let alt = col(line, 16..17);
                if !alt.is_empty() && alt != "A" && alt != "1" {
                    continue;
                }
                let (Ok(x), Ok(y), Ok(z)) =
                    (col(line, 30..38).parse::<f32>(), col(line, 38..46).parse::<f32>(), col(line, 46..54).parse::<f32>())
                else {
                    continue;
                };
                let name = col(line, 12..16).to_string();
                let mut element = col(line, 76..78).to_ascii_uppercase();
                if element.is_empty() {
                    element = element_from_name(&name);
                }
                raw.atoms.push(RawAtom {
                    res_name: col(line, 17..20).to_string(),
                    chain: col(line, 21..22).to_string(),
                    seq_num: col(line, 22..26).parse().unwrap_or(0),
                    icode: icode(line, 26),
                    pos: [x, y, z],
                    element,
                    b_factor: col(line, 60..66).parse().unwrap_or(0.0),
                    hetero: rec == "HETATM",
                    polymer: None,
                    name,
                });
            }
            _ => {}
        }
    }
    raw
}

fn icode(line: &str, at: usize) -> char {
    line.get(at..at + 1).and_then(|s| s.chars().next()).filter(|c| !c.is_whitespace()).unwrap_or(' ')
}

fn element_from_name(name: &str) -> String {
    name.chars().find(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_default()
}

/// Split mmCIF text into tokens: whitespace-separated words, `'quoted'` and `"quoted"`
/// strings, and `;`-delimited text fields. Comments are skipped.
fn cif_tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut lines = text.split_inclusive('\n').peekable();
    let mut offset = 0;
    while let Some(line) = lines.next() {
        let start = offset;
        offset += line.len();
        if line.starts_with(';') {
            // Text field: runs to the next line starting with ';'.
            let body_start = start + 1;
            let mut body_end = offset;
            for l in lines.by_ref() {
                let ls = offset;
                offset += l.len();
                if l.starts_with(';') {
                    body_end = ls;
                    break;
                }
            }
            out.push(text[body_start..body_end].trim());
            continue;
        }
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_whitespace() {
                i += 1;
            } else if c == b'#' {
                break;
            } else if c == b'\'' || c == b'"' {
                // A quote closes only when followed by whitespace or the line end.
                let mut j = i + 1;
                while j < bytes.len() && !(bytes[j] == c && bytes.get(j + 1).is_none_or(|b| b.is_ascii_whitespace())) {
                    j += 1;
                }
                out.push(&line[i + 1..j.min(bytes.len())]);
                i = j + 1;
            } else {
                let mut j = i;
                while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                out.push(&line[i..j]);
                i = j;
            }
        }
    }
    out
}

/// The loops (and single items, as one-row tables) of the first data block, by category.
fn cif_tables(text: &str) -> HashMap<String, (Vec<String>, Vec<Vec<String>>)> {
    let tokens = cif_tokens(text);
    let mut tables: HashMap<String, (Vec<String>, Vec<Vec<String>>)> = HashMap::new();
    let mut i = 0;
    let mut blocks = 0;
    let split = |tag: &str| -> (String, String) {
        let tag = tag.trim_start_matches('_');
        match tag.split_once('.') {
            Some((c, f)) => (c.to_ascii_lowercase(), f.to_ascii_lowercase()),
            None => (tag.to_ascii_lowercase(), String::new()),
        }
    };
    while i < tokens.len() {
        let t = tokens[i];
        if t.starts_with("data_") {
            blocks += 1;
            if blocks > 1 {
                break;
            }
            i += 1;
        } else if t.eq_ignore_ascii_case("loop_") {
            i += 1;
            let mut fields = Vec::new();
            let mut cat = String::new();
            while i < tokens.len() && tokens[i].starts_with('_') {
                let (c, f) = split(tokens[i]);
                cat = c;
                fields.push(f);
                i += 1;
            }
            let mut rows = Vec::new();
            while i < tokens.len() && !tokens[i].starts_with('_') && !tokens[i].eq_ignore_ascii_case("loop_") && !tokens[i].starts_with("data_") {
                let end = (i + fields.len()).min(tokens.len());
                if end - i < fields.len() {
                    break;
                }
                rows.push(tokens[i..end].iter().map(|s| s.to_string()).collect());
                i = end;
            }
            if !fields.is_empty() {
                tables.insert(cat, (fields, rows));
            }
        } else if t.starts_with('_') {
            let (c, f) = split(t);
            let value = tokens.get(i + 1).copied().unwrap_or("").to_string();
            let entry = tables.entry(c).or_insert_with(|| (Vec::new(), vec![Vec::new()]));
            entry.0.push(f);
            if let Some(row) = entry.1.first_mut() {
                row.push(value);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    tables
}

fn parse_cif(text: &str) -> Result<Raw> {
    let tables = cif_tables(text);
    let mut raw = Raw::default();
    let get = |table: &str, row: &[String], fields: &[String], names: &[&str]| -> Option<String> {
        let _ = table;
        names.iter().find_map(|n| fields.iter().position(|f| f == n)).and_then(|ix| row.get(ix)).cloned()
    };
    if let Some((fields, rows)) = tables.get("struct") {
        if let Some(t) = rows.first().and_then(|r| get("struct", r, fields, &["title"])) {
            raw.title = t;
        }
    }
    let Some((fields, rows)) = tables.get("atom_site") else { bail!("no _atom_site table") };
    let mut first_model: Option<String> = None;
    for row in rows {
        let g = |names: &[&str]| get("atom_site", row, fields, names).filter(|v| v != "?" && v != ".");
        if let Some(m) = g(&["pdbx_pdb_model_num"]) {
            match &first_model {
                None => first_model = Some(m),
                Some(f) if *f != m => continue,
                _ => {}
            }
        }
        if let Some(alt) = g(&["label_alt_id"]) {
            if alt != "A" && alt != "1" {
                continue;
            }
        }
        let coord = |n: &str| g(&[n]).and_then(|v| v.parse::<f32>().ok());
        let (Some(x), Some(y), Some(z)) = (coord("cartn_x"), coord("cartn_y"), coord("cartn_z")) else { continue };
        let name = g(&["auth_atom_id", "label_atom_id"]).unwrap_or_default();
        let element = g(&["type_symbol"]).map(|e| e.to_ascii_uppercase()).unwrap_or_else(|| element_from_name(&name));
        let polymer = get("atom_site", row, fields, &["label_seq_id"]).map(|v| v != "." && v != "?");
        raw.atoms.push(RawAtom {
            res_name: g(&["auth_comp_id", "label_comp_id"]).unwrap_or_default(),
            chain: g(&["auth_asym_id", "label_asym_id"]).unwrap_or_default(),
            seq_num: g(&["auth_seq_id", "label_seq_id"]).and_then(|v| v.parse().ok()).unwrap_or(0),
            icode: g(&["pdbx_pdb_ins_code"]).and_then(|v| v.chars().next()).unwrap_or(' '),
            pos: [x, y, z],
            element,
            b_factor: coord("b_iso_or_equiv").unwrap_or(0.0),
            hetero: g(&["group_pdb"]).as_deref() == Some("HETATM"),
            polymer,
            name,
        });
    }
    let ss_ranges = |table: &str, kind: Option<Ss>, raw: &mut Raw| {
        let Some((fields, rows)) = tables.get(table) else { return };
        for row in rows {
            let g = |names: &[&str]| get(table, row, fields, names).filter(|v| v != "?" && v != ".");
            let ss = match kind {
                Some(k) => k,
                None => match g(&["conf_type_id"]) {
                    Some(t) if t.to_ascii_uppercase().starts_with("HELX") => Ss::Helix,
                    Some(t) if t.to_ascii_uppercase().starts_with("STRN") => Ss::Strand,
                    _ => continue,
                },
            };
            let chain = g(&["beg_auth_asym_id", "beg_label_asym_id"]).unwrap_or_default();
            let num = |names: &[&str]| g(names).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
            let ic = |names: &[&str]| g(names).and_then(|v| v.chars().next()).unwrap_or(' ');
            let start = (num(&["beg_auth_seq_id", "beg_label_seq_id"]), ic(&["pdbx_beg_pdb_ins_code"]));
            let end = (num(&["end_auth_seq_id", "end_label_seq_id"]), ic(&["pdbx_end_pdb_ins_code"]));
            raw.ss.push((chain, start, end, ss));
        }
    };
    ss_ranges("struct_conf", None, &mut raw);
    ss_ranges("struct_sheet_range", Some(Ss::Strand), &mut raw);
    Ok(raw)
}

// ----- model -----------------------------------------------------------------------

fn build(raw: Raw) -> Molecule {
    let mut mol = Molecule { title: raw.title.clone(), ..Default::default() };
    let mut chain_ix: HashMap<String, usize> = HashMap::new();
    // Group consecutive atoms into residues; residues of a chain may be interleaved
    // with ligands in the file, so gather per chain first and keep file order within.
    let mut per_chain: Vec<Vec<(String, i32, char, Vec<RawAtom>)>> = Vec::new();
    for a in raw.atoms {
        let c = *chain_ix.entry(a.chain.clone()).or_insert_with(|| {
            per_chain.push(Vec::new());
            per_chain.len() - 1
        });
        let list = &mut per_chain[c];
        match list.last_mut() {
            Some((name, num, ic, atoms)) if *name == a.res_name && *num == a.seq_num && *ic == a.icode => atoms.push(a),
            _ => list.push((a.res_name.clone(), a.seq_num, a.icode, vec![a])),
        }
    }
    let mut chain_ids: Vec<(String, usize)> = chain_ix.into_iter().collect();
    chain_ids.sort_by_key(|(_, ix)| *ix);
    for ((id, _), residues) in chain_ids.into_iter().zip(per_chain) {
        let ci = mol.chains.len();
        let r0 = mol.residues.len();
        let seqres = raw.seqres.get(&id);
        for (name, num, icode, atoms) in residues {
            let polymer = match atoms[0].polymer {
                Some(p) => p,
                None => !atoms[0].hetero || seqres.is_some_and(|s| s.contains(&name)) || atoms.iter().any(|a| a.name == "CA") && one_letter(&name).is_some(),
            };
            let a0 = mol.atoms.len();
            let ri = mol.residues.len();
            let mut trace = None;
            let mut carbonyl = None;
            for a in &atoms {
                let ix = mol.atoms.len();
                if polymer && trace.is_none() && (a.name == "CA" || a.name == "P") {
                    trace = Some(ix);
                }
                if polymer && carbonyl.is_none() && a.name == "O" {
                    carbonyl = Some(ix);
                }
                mol.atoms.push(Atom { pos: a.pos, element: a.element.clone(), residue: ri, b_factor: a.b_factor });
            }
            if polymer && trace.is_none() {
                // Modified residues such as chromophores name their alpha carbons CA1, CA2…
                // the middle one sits closest to where the chain runs.
                let cas: Vec<usize> = atoms.iter().enumerate().filter(|(_, a)| a.name.starts_with("CA")).map(|(i, _)| a0 + i).collect();
                trace = cas.get(cas.len() / 2).copied().or_else(|| atoms.iter().position(|a| a.name == "C4'").map(|i| a0 + i));
            }
            let letters = if polymer { one_letter(&name).unwrap_or("X").to_string() } else { String::new() };
            mol.residues.push(Residue {
                chain: ci,
                name,
                seq_num: num,
                icode,
                polymer,
                letters,
                ss: Ss::Coil,
                trace,
                carbonyl,
                atoms: a0..mol.atoms.len(),
            });
        }
        mol.chains.push(Chain { id, residues: r0..mol.residues.len() });
    }
    for (chain, start, end, ss) in &raw.ss {
        let Some(c) = mol.chains.iter().find(|c| c.id == *chain) else { continue };
        let range = c.residues.clone();
        let mut inside = false;
        for r in &mut mol.residues[range] {
            let key = (r.seq_num, r.icode);
            if key == *start {
                inside = true;
            }
            if inside && r.polymer {
                r.ss = *ss;
            }
            if key == *end {
                break;
            }
        }
    }
    mol
}

/// One-letter codes for standard residues and common modified ones. Chromophores
/// formed from three residues give all three.
fn one_letter(name: &str) -> Option<&'static str> {
    Some(match name {
        "ALA" => "A",
        "ARG" => "R",
        "ASN" => "N",
        "ASP" => "D",
        "CYS" => "C",
        "GLN" => "Q",
        "GLU" => "E",
        "GLY" => "G",
        "HIS" => "H",
        "ILE" => "I",
        "LEU" => "L",
        "LYS" => "K",
        "MET" => "M",
        "PHE" => "F",
        "PRO" => "P",
        "SER" => "S",
        "THR" => "T",
        "TRP" => "W",
        "TYR" => "Y",
        "VAL" => "V",
        "SEC" => "U",
        "PYL" => "O",
        "MSE" => "M",
        "SEP" => "S",
        "TPO" => "T",
        "PTR" => "Y",
        "HYP" => "P",
        "MLY" => "K",
        "CSO" | "CME" | "CSD" => "C",
        // Fluorescent-protein chromophores.
        "CRO" => "TYG",
        "GYS" => "SYG",
        "CR2" => "GYG",
        "CRQ" => "QYG",
        "CRF" => "TWG",
        "CH6" => "MYG",
        "NRQ" => "MYG",
        "CR8" => "HYG",
        "DA" | "A" => "A",
        "DC" | "C" => "C",
        "DG" | "G" => "G",
        "DT" => "T",
        "U" => "U",
        _ => return None,
    })
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase(),
        None => String::new(),
    }
}

// ----- linking chains to CDS features ----------------------------------------------

/// A protein chain tied to the CDS that encodes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainLink {
    pub chain: usize,
    pub feature: usize,
    /// For each residue of the molecule: the sequence span `[start, end)` of the codons
    /// it was translated from, when it aligned to the CDS.
    pub spans: Vec<(usize, Option<(usize, usize)>)>,
    pub identity: f32,
    pub aligned: usize,
}

/// Translate a feature with its own genetic code; see `genetic_code::translate`.
pub fn translate(seq: &[u8], f: &Feature) -> (Vec<u8>, Vec<[usize; 3]>) {
    crate::genetic_code::translate(seq, f)
}

pub fn codon_to_aa(c: [u8; 3]) -> u8 {
    const AAS: &[u8; 64] = b"KNKNTTTTRSRSIIMIQHQHPPPPRRRRLLLLEDEDAAAAGGGGVVVV*Y*YSSSS*CWCLFLF";
    let ix = |b: u8| match b.to_ascii_uppercase() {
        b'A' => Some(0),
        b'C' => Some(1),
        b'G' => Some(2),
        b'T' | b'U' => Some(3),
        _ => None,
    };
    match (ix(c[0]), ix(c[1]), ix(c[2])) {
        (Some(a), Some(b), Some(d)) => AAS[a * 16 + b * 4 + d],
        _ => b'X',
    }
}

/// Smith–Waterman local alignment; returns pairs `(i in a, j in b)` of aligned letters.
fn local_align(a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    const MATCH: i32 = 3;
    const MISMATCH: i32 = -1;
    const GAP: i32 = -3;
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let w = m + 1;
    let mut h = vec![0i32; (n + 1) * w];
    let mut best = (0, 0, 0);
    for i in 1..=n {
        for j in 1..=m {
            let s = if a[i - 1] == b[j - 1] && a[i - 1] != b'X' { MATCH } else { MISMATCH };
            let v = (h[(i - 1) * w + j - 1] + s).max(h[(i - 1) * w + j] + GAP).max(h[i * w + j - 1] + GAP).max(0);
            h[i * w + j] = v;
            if v > best.0 {
                best = (v, i, j);
            }
        }
    }
    let (mut i, mut j) = (best.1, best.2);
    let mut pairs = Vec::new();
    while i > 0 && j > 0 && h[i * w + j] > 0 {
        let v = h[i * w + j];
        let s = if a[i - 1] == b[j - 1] && a[i - 1] != b'X' { MATCH } else { MISMATCH };
        if v == h[(i - 1) * w + j - 1] + s {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if v == h[(i - 1) * w + j] + GAP {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// Tie each protein chain to the CDS it aligns to best, if any aligns well.
pub fn link_chains(mol: &Molecule, seq: &[u8], features: &[Feature]) -> Vec<ChainLink> {
    let cds: Vec<(usize, Vec<u8>, Vec<[usize; 3]>)> = features
        .iter()
        .enumerate()
        .filter(|(_, f)| f.kind.eq_ignore_ascii_case("CDS"))
        .map(|(i, f)| {
            let (p, c) = translate(seq, f);
            (i, p, c)
        })
        .filter(|(_, p, _)| p.len() >= 10)
        .collect();
    let mut links = Vec::new();
    for ci in protein_chains(mol) {
        let (letters, owner) = chain_letters(mol, ci);
        let mut best: Option<ChainLink> = None;
        for (fi, protein, codons) in &cds {
            let Some(link) = align_chain(mol, ci, &letters, &owner, *fi, protein, codons) else { continue };
            if !link.convincing(letters.len(), protein.len()) {
                continue;
            }
            if best.as_ref().is_none_or(|b| link.score() > b.score()) {
                best = Some(link);
            }
        }
        links.extend(best);
    }
    links
}

/// The links a structure's chains get from features naming them in a `/structure`
/// qualifier, however well they align.
pub fn bound_links(mol: &Molecule, seq: &[u8], features: &[Feature], structure_id: &str) -> Vec<ChainLink> {
    let mut links = Vec::new();
    for (fi, f) in features.iter().enumerate() {
        for (sid, chain) in f.structure_refs() {
            if sid != structure_id {
                continue;
            }
            let Some(ci) = mol.chains.iter().position(|c| c.id == chain) else { continue };
            if let Some(link) = link_chain_to_feature(mol, ci, seq, fi, f) {
                links.push(link);
            }
        }
    }
    links
}

/// A chain's one-letter sequence, and the residue each letter came from.
pub fn chain_letters(mol: &Molecule, ci: usize) -> (Vec<u8>, Vec<usize>) {
    let mut letters = Vec::new();
    let mut owner = Vec::new();
    for r in mol.chains[ci].residues.clone() {
        for l in mol.residues[r].letters.bytes() {
            letters.push(l);
            owner.push(r);
        }
    }
    (letters, owner)
}

/// Chains long enough, and protein enough, to be worth aligning to the sequence.
pub fn protein_chains(mol: &Molecule) -> Vec<usize> {
    (0..mol.chains.len())
        .filter(|&ci| {
            let (letters, _) = chain_letters(mol, ci);
            let protein_like = letters.iter().filter(|l| !b"ACGTUX".contains(l)).count() * 2 > letters.len();
            letters.len() >= 10 && protein_like
        })
        .collect()
}

/// Align chain `ci` to feature `fi`'s translation, without judging how well it fits.
pub fn link_chain_to_feature(mol: &Molecule, ci: usize, seq: &[u8], fi: usize, f: &Feature) -> Option<ChainLink> {
    let (letters, owner) = chain_letters(mol, ci);
    let (protein, codons) = translate(seq, f);
    align_chain(mol, ci, &letters, &owner, fi, &protein, &codons)
}

impl ChainLink {
    /// Aligned length weighted by identity, for ranking candidate features.
    pub fn score(&self) -> f32 {
        self.aligned as f32 * self.identity
    }

    /// Close enough to call this feature the chain's gene: mostly identical, and most
    /// of the shorter of the two aligned, so a shared tag alone doesn't count.
    pub fn convincing(&self, chain_len: usize, protein_len: usize) -> bool {
        let same = (self.identity * self.aligned as f32).round() as usize;
        self.aligned >= 10 && self.identity >= 0.6 && same * 2 >= chain_len.min(protein_len)
    }
}

fn align_chain(
    mol: &Molecule,
    ci: usize,
    letters: &[u8],
    owner: &[usize],
    fi: usize,
    protein: &[u8],
    codons: &[[usize; 3]],
) -> Option<ChainLink> {
    let pairs = local_align(letters, protein);
    if pairs.is_empty() {
        return None;
    }
    let same = pairs.iter().filter(|&&(i, j)| letters[i] == protein[j]).count();
    let identity = same as f32 / pairs.len() as f32;
    let mut spans: HashMap<usize, (usize, usize)> = HashMap::new();
    for &(i, j) in &pairs {
        let c = codons[j];
        let (lo, hi) = (c.iter().min().copied().unwrap_or(0), c.iter().max().copied().unwrap_or(0) + 1);
        let e = spans.entry(owner[i]).or_insert((lo, hi));
        e.0 = e.0.min(lo);
        e.1 = e.1.max(hi);
    }
    let mut spans: Vec<(usize, Option<(usize, usize)>)> = mol.chains[ci].residues.clone().map(|r| (r, spans.get(&r).copied())).collect();
    spans.sort_by_key(|s| s.0);
    Some(ChainLink { chain: ci, feature: fi, spans, identity, aligned: pairs.len() })
}

/// Where in the sequence a chain is encoded, found by aligning it to all six reading
/// frames: what a new feature for it would cover.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    /// Sorted, half-open; two segments when it runs through the origin.
    pub segments: Vec<(usize, usize)>,
    pub strand: i8,
    pub identity: f32,
    pub aligned: usize,
}

/// The stretch of `seq`, in any frame on either strand, that best encodes chain `ci`,
/// if it does convincingly. On a circular sequence it may run through the origin.
pub fn propose_feature(mol: &Molecule, ci: usize, seq: &[u8], circular: bool) -> Option<Proposal> {
    let (letters, _) = chain_letters(mol, ci);
    let len = seq.len();
    if letters.len() < 10 || len < 30 {
        return None;
    }
    // Run a circular sequence on past the origin, far enough for the whole chain.
    let mut ext = seq.to_vec();
    if circular {
        ext.extend_from_slice(&seq[..len.min(letters.len() * 3 + 3)]);
    }
    let rc = model::reverse_complement(&ext);
    let n = ext.len();

    // Score every frame cheaply first; trace back only the best.
    let mut best: Option<(i32, i8, usize, Vec<u8>)> = None;
    for (strand, strand_seq) in [(1i8, &ext), (-1i8, &rc)] {
        for frame in 0..3 {
            let protein: Vec<u8> =
                strand_seq[frame.min(n)..].chunks_exact(3).map(|c| codon_to_aa([c[0], c[1], c[2]])).collect();
            let (score, _, _) = local_best(&letters, &protein);
            if best.as_ref().is_none_or(|b| score > b.0) {
                best = Some((score, strand, frame, protein));
            }
        }
    }
    let (_, strand, frame, protein) = best?;
    let (_, _, j_end) = local_best(&letters, &protein);
    // The alignment can't be much longer than the chain, so align against a window.
    let lo = j_end.saturating_sub(letters.len() * 2 + 10);
    let pairs = local_align(&letters, &protein[lo..j_end]);
    let (first, last) = (pairs.first()?.1 + lo, pairs.last()?.1 + lo);
    let same = pairs.iter().filter(|&&(i, j)| letters[i] == protein[j + lo]).count();
    let identity = same as f32 / pairs.len() as f32;
    if pairs.len() < 10 || identity < 0.6 || same * 2 < letters.len() {
        return None;
    }
    // Codons first..=last, as a range of `ext`.
    let (a, b) = (frame + 3 * first, frame + 3 * last + 3);
    let (a, b) = if strand > 0 { (a, b) } else { (n - b, n - a) };
    let (a, b) = if a >= len { (a - len, b - len) } else { (a, b) };
    if b - a > len {
        return None;
    }
    let segments = if b <= len { vec![(a, b)] } else { vec![(0, b - len), (a, len)] };
    Some(Proposal { segments, strand, identity, aligned: pairs.len() })
}

/// Smith–Waterman score only, in linear memory: (best score, end in a, end in b),
/// ends exclusive.
fn local_best(a: &[u8], b: &[u8]) -> (i32, usize, usize) {
    const MATCH: i32 = 3;
    const MISMATCH: i32 = -1;
    const GAP: i32 = -3;
    let m = b.len();
    let mut prev = vec![0i32; m + 1];
    let mut cur = vec![0i32; m + 1];
    let mut best = (0, 0, 0);
    for i in 1..=a.len() {
        cur[0] = 0;
        for j in 1..=m {
            let s = if a[i - 1] == b[j - 1] && a[i - 1] != b'X' { MATCH } else { MISMATCH };
            let v = (prev[j - 1] + s).max(prev[j] + GAP).max(cur[j - 1] + GAP).max(0);
            cur[j] = v;
            if v > best.0 {
                best = (v, i, j);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    best
}

/// A structure file read from disk and checked.
pub struct StructureFile {
    pub name: String,
    pub stem: String,
    pub format: StructureFormat,
    pub data: Vec<u8>,
    pub mol: Molecule,
}

/// Files bigger than this are refused: a whole ribosome is ~30 MB of mmCIF.
const MAX_FILE: u64 = 64 << 20;

impl StructureFile {
    /// Read and parse a structure file. Slow for big files: run it off the main thread.
    pub fn read(path: &std::path::Path) -> Result<StructureFile, String> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let size = std::fs::metadata(path).map_err(|e| format!("Could not read {name}: {e}"))?.len();
        if size > MAX_FILE {
            return Err(format!("{name} is too large to attach ({} MB)", size >> 20));
        }
        let data = std::fs::read(path).map_err(|e| format!("Could not read {name}: {e}"))?;
        let format = sniff_format(&name, &data).ok_or_else(|| format!("{name} isn't a PDB or mmCIF file"))?;
        let mol = Molecule::parse(&data, format).map_err(|e| format!("Could not read {name}: {e:#}"))?;
        Ok(StructureFile { name, stem, format, data, mol })
    }

    /// The structure to attach, with an id not among `taken`.
    pub fn to_structure(&self, taken: &[Structure], linking: ChainLinking) -> Structure {
        let stem = &self.stem;
        // Four characters starting with a digit is a PDB entry; say where it came from.
        let pdb = stem.len() == 4 && stem.chars().all(|c| c.is_ascii_alphanumeric()) && stem.starts_with(|c: char| c.is_ascii_digit());
        Structure {
            id: pgx::structure_id(stem, taken),
            name: self.name.clone(),
            format: self.format,
            source: pdb.then(|| format!("PDB {}", stem.to_ascii_uppercase())),
            linking,
            data: self.data.clone().into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDB: &str = "\
TITLE     TINY TEST
SEQRES   1 A    4  MET CRO VAL LYS
HELIX    1   1 MET A    1  VAL A    3  5                                   3
ATOM      1  N   MET A   1      0.000   0.000   0.000  1.00 10.00            N
ATOM      2  CA  MET A   1      1.000   0.000   0.000  1.00 10.00            C
HETATM    3  CA1 CRO A   2      2.000   0.000   0.000  1.00 20.00            C
HETATM    4  CA2 CRO A   2      3.000   0.000   0.000  1.00 20.00            C
HETATM    5  CA3 CRO A   2      4.000   0.000   0.000  1.00 20.00            C
ATOM      6  CA AVAL A   3      5.000   0.000   0.000  0.50 30.00            C
ATOM      7  CA BVAL A   3      9.000   0.000   0.000  0.50 30.00            C
ATOM      8  CA  LYS A   4      6.000   0.000   0.000  1.00 40.00            C
HETATM    9  O   HOH A 101      7.000   0.000   0.000  1.00 50.00            O
HETATM   10 FE   HEM A 201      8.000   0.000   0.000  1.00 50.00           FE
";

    #[test]
    fn pdb_residues_chromophore_and_altlocs() {
        let m = Molecule::parse(PDB.as_bytes(), StructureFormat::Pdb).unwrap();
        assert_eq!(m.title, "TINY TEST");
        assert_eq!(m.chains.len(), 1);
        let names: Vec<&str> = m.residues.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["MET", "CRO", "VAL", "LYS", "HEM"]);
        assert_eq!(m.residues[1].letters, "TYG");
        assert!(m.residues[1].polymer && !m.residues[4].polymer);
        // The chromophore traces through its middle alpha carbon.
        assert_eq!(m.atoms[m.residues[1].trace.unwrap()].pos[0], 3.0);
        // Only the first alternate location is kept.
        assert_eq!(m.residues[2].atoms.len(), 1);
        assert_eq!(m.residues[0].ss, Ss::Helix);
        assert_eq!(m.residues[2].ss, Ss::Helix);
        assert_eq!(m.residues[3].ss, Ss::Coil);
        assert_eq!(m.atoms.last().unwrap().element, "FE");
    }

    #[test]
    fn cif_atom_site_and_quotes() {
        let cif = "data_TEST
_struct.title 'It's a title'
loop_
_atom_site.group_PDB
_atom_site.id
_atom_site.type_symbol
_atom_site.label_atom_id
_atom_site.label_alt_id
_atom_site.label_comp_id
_atom_site.label_asym_id
_atom_site.label_seq_id
_atom_site.Cartn_x
_atom_site.Cartn_y
_atom_site.Cartn_z
_atom_site.B_iso_or_equiv
_atom_site.auth_seq_id
_atom_site.auth_asym_id
_atom_site.pdbx_PDB_model_num
ATOM 1 C CA . GLY A 1 1.0 2.0 3.0 10.0 5 A 1
ATOM 2 C \"C4'\" . DA B 1 1.0 2.0 3.0 10.0 1 B 1
HETATM 3 O O . HOH C . 0 0 0 1.0 100 C 1
ATOM 4 C CA . GLY A 1 9.0 9.0 9.0 10.0 5 A 2
#
loop_
_struct_conf.conf_type_id
_struct_conf.beg_auth_asym_id
_struct_conf.beg_auth_seq_id
_struct_conf.end_auth_seq_id
HELX_P A 5 5
";
        let m = Molecule::parse(cif.as_bytes(), StructureFormat::Mmcif).unwrap();
        assert_eq!(m.title, "It's a title");
        assert_eq!(m.chains.len(), 2);
        assert_eq!(m.atoms.len(), 2, "water and the second model are dropped");
        assert_eq!(m.residues[0].seq_num, 5);
        assert_eq!(m.residues[0].ss, Ss::Helix);
        assert!(m.residues[1].trace.is_some(), "nucleotides trace through C4' without a P");
    }

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff_format("x.CIF", b""), Some(StructureFormat::Mmcif));
        assert_eq!(sniff_format("x", b"data_1ABC\n"), Some(StructureFormat::Mmcif));
        assert_eq!(sniff_format("x", b"HEADER    PROTEIN\n"), Some(StructureFormat::Pdb));
        assert_eq!(sniff_format("x", b"hello"), None);
    }

    fn feature(segments: Vec<(usize, usize)>, strand: i8) -> Feature {
        Feature { kind: "CDS".into(), label: "x".into(), segments, strand, source: None }
    }

    #[test]
    fn translates_both_strands() {
        let seq = b"ATGAAATTTTAA";
        let (p, c) = translate(seq, &feature(vec![(0, 12)], 1));
        assert_eq!(p, b"MKF*");
        assert_eq!(c[1], [3, 4, 5]);
        let rc = model::reverse_complement(seq);
        let (p, c) = translate(&rc, &feature(vec![(0, 12)], -1));
        assert_eq!(p, b"MKF*");
        assert_eq!(c[0], [11, 10, 9]);
    }

    #[test]
    fn links_a_tagged_chain_to_its_cds() {
        // A 30-residue protein, and a structure of it with a His-tag and one mutation.
        let protein = "MSKGEELFTGVVPILVELDGDVNGHKFSVS";
        let codon = |a: char| match a {
            'M' => "ATG", 'S' => "TCT", 'K' => "AAA", 'G' => "GGT", 'E' => "GAA", 'L' => "CTG", 'F' => "TTC",
            'T' => "ACC", 'V' => "GTG", 'P' => "CCG", 'I' => "ATC", 'D' => "GAT", 'N' => "AAC", 'H' => "CAC",
            _ => unreachable!(),
        };
        let mut seq = b"CCCC".to_vec();
        for a in protein.chars() {
            seq.extend_from_slice(codon(a).as_bytes());
        }
        seq.extend_from_slice(b"TAAGGG");
        let cds = feature(vec![(4, seq.len() - 3)], 1);
        let mut pdb = String::new();
        let structure = format!("HHHHHH{}", protein.replace("PIL", "PAL"));
        for (i, a) in structure.chars().enumerate() {
            let name = match a {
                'M' => "MET", 'S' => "SER", 'K' => "LYS", 'G' => "GLY", 'E' => "GLU", 'L' => "LEU", 'F' => "PHE",
                'T' => "THR", 'V' => "VAL", 'P' => "PRO", 'I' => "ILE", 'D' => "ASP", 'N' => "ASN", 'H' => "HIS",
                'A' => "ALA", _ => unreachable!(),
            };
            pdb.push_str(&format!(
                "ATOM  {:>5}  CA  {name} A{:>4}    {:>8.3}{:>8.3}{:>8.3}  1.00 10.00           C\n",
                i + 1,
                i + 1,
                i as f32,
                0.0,
                0.0
            ));
        }
        let m = Molecule::parse(pdb.as_bytes(), StructureFormat::Pdb).unwrap();
        let links = link_chains(&m, &seq, &[cds]);
        assert_eq!(links.len(), 1);
        let l = &links[0];
        assert_eq!(l.feature, 0);
        // The tag has no codons; the first Met maps to the CDS start.
        assert_eq!(l.spans[0].1, None);
        assert_eq!(l.spans[6].1, Some((4, 7)));
        assert!(l.identity > 0.9);
    }
}

#[cfg(test)]
mod sample_tests {
    use super::*;
    use crate::model::Document;
    use std::path::Path;

    fn samples() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/samples"))
    }

    #[test]
    fn egfp_structure_links_to_the_egfp_cds() {
        let doc = Document::open(&samples().join("pEGFP-N1.gb")).unwrap();
        let bytes = std::fs::read(samples().join("2Y0G.pdb")).unwrap();
        let m = Molecule::parse(&bytes, StructureFormat::Pdb).unwrap();
        assert_eq!(m.chains.len(), 1);
        assert!(m.residues.iter().any(|r| r.name == "CRO" && r.polymer));
        assert!(m.residues.iter().filter(|r| r.ss == Ss::Strand).count() > 100, "GFP is a beta barrel");

        let links = link_chains(&m, &doc.seq, &doc.features);
        assert_eq!(links.len(), 1);
        let l = &links[0];
        let f = &doc.features[l.feature];
        assert_eq!(f.segments, vec![(678, 1398)], "{}", f.label);
        assert!(l.identity > 0.95, "{}", l.identity);
        // Lys3 of the structure is Lys3 of EGFP (after the His-tag, numbering follows GFP).
        let lys3 = m.residues.iter().position(|r| r.seq_num == 3).unwrap();
        let span = l.spans.iter().find(|s| s.0 == lys3).unwrap().1.unwrap();
        assert_eq!(&doc.seq[span.0..span.1], b"AAG");
        // The chromophore covers Thr65-Tyr66-Gly67's nine bases.
        let cro = m.residues.iter().position(|r| r.name == "CRO").unwrap();
        let span = l.spans.iter().find(|s| s.0 == cro).unwrap().1.unwrap();
        assert_eq!(span.1 - span.0, 9);
    }
}

/// `cargo test -- --ignored write_egfp_sample` rebuilds `samples/pEGFP-N1.pgx`: the
/// GenBank plasmid with the 2Y0G EGFP structure attached.
#[cfg(test)]
mod make_samples {
    use crate::model::{Document, Format};
    use crate::pgx::{ChainLinking, Structure, StructureFormat};
    use std::path::Path;

    #[test]
    #[ignore]
    fn write_egfp_sample() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/samples"));
        let mut doc = Document::open(&dir.join("pEGFP-N1.gb")).unwrap();
        let data = std::fs::read(dir.join("2Y0G.pdb")).unwrap();
        doc.add_structure(Structure {
            id: "2y0g".into(),
            name: "2Y0G.pdb".into(),
            format: StructureFormat::Pdb,
            source: Some("PDB 2Y0G".into()),
            linking: ChainLinking::Auto,
            data: data.into(),
        });
        let bytes = doc.serialize_as(Format::Native).unwrap();
        std::fs::write(dir.join("pEGFP-N1.pgx"), bytes).unwrap();
        let back = Document::open(&dir.join("pEGFP-N1.pgx")).unwrap();
        assert_eq!(back.structures.len(), 1);
    }
}

//! What the results view shows and does, without the UI: sorting the hit table,
//! laying out an alignment as NCBI prints it, and turning a match into a feature.

use super::{format_evalue, BlastResults, Hit, Hsp, Program};
use crate::locks::Lock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Description,
    Organism,
    MaxScore,
    TotalScore,
    QueryCover,
    Evalue,
    Identity,
    Length,
    Accession,
}

impl Column {
    pub const ALL: [Column; 9] = [
        Column::Description,
        Column::Organism,
        Column::MaxScore,
        Column::TotalScore,
        Column::QueryCover,
        Column::Evalue,
        Column::Identity,
        Column::Length,
        Column::Accession,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Description => "Description",
            Column::Organism => "Scientific name",
            Column::MaxScore => "Max score",
            Column::TotalScore => "Total score",
            Column::QueryCover => "Query cover",
            Column::Evalue => "E-value",
            Column::Identity => "Per. ident",
            Column::Length => "Acc. len",
            Column::Accession => "Accession",
        }
    }

    /// The order a first click sorts in: best first.
    pub fn default_ascending(self) -> bool {
        matches!(self, Column::Description | Column::Organism | Column::Evalue | Column::Accession)
    }

    pub fn cell(self, hit: &Hit, query_len: usize) -> String {
        match self {
            Column::Description => hit.title(),
            Column::Organism => hit.sciname(),
            Column::MaxScore => format!("{:.0}", hit.max_score()),
            Column::TotalScore => format!("{:.0}", hit.total_score()),
            Column::QueryCover => format!("{:.0}%", hit.query_cover(query_len)),
            Column::Evalue => format_evalue(hit.evalue()),
            Column::Identity => format!("{:.2}%", hit.identity_pct()),
            Column::Length => crate::editor::thousands(hit.len),
            Column::Accession => hit.accession_version(),
        }
    }
}

/// The hits' indices in table order.
pub fn sorted(hits: &[Hit], by: Column, ascending: bool, query_len: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..hits.len()).collect();
    let num = |h: &Hit| -> f64 {
        match by {
            Column::MaxScore => h.max_score(),
            Column::TotalScore => h.total_score(),
            Column::QueryCover => h.query_cover(query_len),
            Column::Evalue => h.evalue(),
            Column::Identity => h.identity_pct(),
            Column::Length => h.len as f64,
            _ => 0.0,
        }
    };
    order.sort_by(|&a, &b| {
        let (x, y) = (&hits[a], &hits[b]);
        let o = match by {
            Column::Description => x.title().to_lowercase().cmp(&y.title().to_lowercase()),
            Column::Organism => x.sciname().to_lowercase().cmp(&y.sciname().to_lowercase()),
            Column::Accession => x.accession_version().cmp(&y.accession_version()),
            _ => num(x).total_cmp(&num(y)),
        };
        // Ties keep NCBI's order.
        let o = if ascending { o } else { o.reverse() };
        o.then(a.cmp(&b))
    });
    order
}

/// One block of an alignment: three lines with coordinates, NCBI style.
#[derive(Debug, PartialEq)]
pub struct Block {
    pub query: (usize, String, usize),
    pub midline: String,
    pub subject: (usize, String, usize),
}

pub const BLOCK: usize = 60;

/// Walks one sequence of an alignment, counting positions.
struct Walker {
    at: i64,
    dir: i64,
    /// Bases per residue: 3 when the row is a translation of nucleotides.
    unit: i64,
}

impl Walker {
    fn new(from: usize, to: usize, dir: i64, unit: i64) -> Walker {
        let start = if dir > 0 { from.min(to) } else { from.max(to) };
        Walker { at: start as i64, dir, unit }
    }

    /// The coordinates of the next `n` residues, and step past them.
    fn take(&mut self, n: i64) -> (usize, usize) {
        let start = self.at;
        if n == 0 {
            return (start.max(0) as usize, (start - self.dir).max(0) as usize);
        }
        let end = start + self.dir * (self.unit * n - 1);
        self.at = end + self.dir;
        (start.max(0) as usize, end.max(0) as usize)
    }
}

/// Lay `hsp` out in blocks of 60 columns. Translated rows count nucleotides, three
/// per residue, and run backwards in reverse frames.
pub fn blocks(hsp: &Hsp, program: Program) -> Vec<Block> {
    let minus = |s: &Option<String>| s.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("minus"));
    let (q_dir, q_unit) = match program {
        Program::Blastx | Program::Tblastx => (if hsp.query_frame.unwrap_or(1) < 0 { -1 } else { 1 }, 3),
        _ => (if minus(&hsp.query_strand) { -1 } else { 1 }, 1),
    };
    let (h_dir, h_unit) = match program {
        Program::Blastx => (1, 1),
        Program::Tblastx => (if hsp.hit_frame.unwrap_or(1) < 0 { -1 } else { 1 }, 3),
        _ => (if minus(&hsp.hit_strand) { -1 } else { 1 }, 1),
    };
    let mut q = Walker::new(hsp.query_from, hsp.query_to, q_dir, q_unit);
    let mut h = Walker::new(hsp.hit_from, hsp.hit_to, h_dir, h_unit);
    let qs: Vec<char> = hsp.qseq.chars().collect();
    let hs: Vec<char> = hsp.hseq.chars().collect();
    let ms: Vec<char> = hsp.midline.chars().collect();
    let cols = qs.len().max(hs.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < cols {
        let j = (i + BLOCK).min(cols);
        let slice = |v: &[char]| -> String { v.get(i..j.min(v.len())).map(|s| s.iter().collect()).unwrap_or_default() };
        let (qt, ht) = (slice(&qs), slice(&hs));
        let mut mt = slice(&ms);
        // Pad a short midline so the columns line up.
        let width = qt.chars().count();
        while mt.chars().count() < width {
            mt.push(' ');
        }
        let residues = |s: &str| s.chars().filter(|c| *c != '-').count() as i64;
        let (qa, qb) = q.take(residues(&qt));
        let (ha, hb) = h.take(residues(&ht));
        out.push(Block { query: (qa, qt, qb), midline: mt, subject: (ha, ht, hb) });
        i = j;
    }
    out
}

/// The three lines of a block, with coordinates padded to `w` digits.
pub fn block_lines(b: &Block, w: usize) -> [String; 3] {
    [
        format!("Query  {:<w$}  {}  {}", b.query.0, b.query.1, b.query.2),
        format!("       {:<w$}  {}", "", b.midline),
        format!("Sbjct  {:<w$}  {}  {}", b.subject.0, b.subject.1, b.subject.2),
    ]
}

/// The widest start coordinate, so a whole alignment lines up.
pub fn coord_width(blocks: &[Block]) -> usize {
    blocks.iter().map(|b| b.query.0.to_string().len().max(b.subject.0.to_string().len())).max().unwrap_or(1)
}

/// The header lines NCBI prints above an alignment.
pub fn hsp_summary(hsp: &Hsp, program: Program) -> Vec<String> {
    let mut first = format!("Score {:.1} bits ({:.0}), Expect {}", hsp.bit_score, hsp.score, format_evalue(hsp.evalue));
    if program == Program::Blastx {
        first.push_str(", composition-adjusted");
    }
    let pct = |n: usize| if hsp.align_len == 0 { 0 } else { (100.0 * n as f64 / hsp.align_len as f64).round() as usize };
    let mut second = format!("Identities {}/{} ({}%)", hsp.identity, hsp.align_len, pct(hsp.identity));
    if program.translated() {
        if let Some(p) = hsp.positive {
            second.push_str(&format!(", Positives {}/{} ({}%)", p, hsp.align_len, pct(p)));
        }
    }
    second.push_str(&format!(", Gaps {}/{} ({}%)", hsp.gaps, hsp.align_len, pct(hsp.gaps)));
    let frame = |f: i32| if f > 0 { format!("+{f}") } else { f.to_string() };
    let third = match program {
        Program::Blastx => format!("Frame {}", frame(hsp.query_frame.unwrap_or(1))),
        Program::Tblastx => format!("Frame {}/{}", frame(hsp.query_frame.unwrap_or(1)), frame(hsp.hit_frame.unwrap_or(1))),
        _ => format!(
            "Strand {}/{}",
            hsp.query_strand.as_deref().unwrap_or("Plus"),
            hsp.hit_strand.as_deref().unwrap_or("Plus")
        ),
    };
    vec![first, second, third]
}

/// Everything the Create Feature dialog needs for a match.
#[derive(Debug, PartialEq)]
pub struct FeatureDraft {
    pub segments: Vec<(usize, usize)>,
    pub strand: i8,
    pub kind: String,
    pub name: String,
    pub qualifiers: Vec<(String, String)>,
}

/// A hit's title without NCBI's trailing " [Organism]", cut to a label's length.
pub fn short_title(title: &str) -> String {
    let t = title.trim();
    let t = match t.rfind(" [") {
        Some(i) if t.ends_with(']') => &t[..i],
        _ => t,
    };
    let t = t.split(">").next().unwrap_or(t).trim();
    if t.chars().count() > 60 {
        let cut: String = t.chars().take(57).collect();
        format!("{}…", cut.trim_end())
    } else {
        t.to_string()
    }
}

/// Which INSDC `/inference` database an accession comes from.
fn inference_db(accession: &str, database: &str) -> &'static str {
    if database.eq_ignore_ascii_case("swissprot") {
        "UniProtKB"
    } else if accession.len() > 3 && accession.as_bytes()[2] == b'_' {
        "RefSeq"
    } else {
        "INSD"
    }
}

/// The query positions `lo..=hi` (1-based) as segments of a sequence `len` long,
/// given where the search's region now starts. Split at the origin if it wraps.
pub fn segments(lock: &Lock, lo: usize, hi: usize, len: usize) -> Vec<(usize, usize)> {
    if len == 0 || hi < lo || lo == 0 {
        return Vec::new();
    }
    let a = (lock.start + lo - 1) % len;
    let n = hi - lo + 1;
    let mut segs = if a + n <= len { vec![(a, a + n)] } else { vec![(a, len), (0, a + n - len)] };
    segs.sort_unstable();
    segs
}

/// The feature a match describes: where it is in the sequence, which way it runs,
/// and qualifiers saying where it came from. `date` is YYYY-MM-DD.
pub fn draft(results: &BlastResults, hit: &Hit, hsp: &Hsp, lock: &Lock, len: usize, author: Option<&str>, date: &str) -> FeatureDraft {
    let program = results.options.program;
    let (lo, hi) = hsp.query_span();
    let segments = segments(lock, lo, hi, len);
    let strand = hsp.strand();
    let acc = hit.accession_version();
    let title = hit.title();
    let name = short_title(&title);
    let organism = hit.sciname();
    let db = results.database.as_str();

    let mut qualifiers = Vec::new();
    let kind = if program == Program::Blastx {
        qualifiers.push(("product".to_string(), name.clone()));
        qualifiers.push(("codon_start".to_string(), "1".to_string()));
        qualifiers.push(("transl_table".to_string(), results.options.genetic_code.to_string()));
        "CDS"
    } else {
        "misc_feature"
    };
    let what = if program.subject_aa() { "AA" } else { "DNA" };
    qualifiers.push(("inference".to_string(), format!("similar to {what} sequence:{}:{acc}", inference_db(&acc, db))));

    let unit = if program.translated() { "aa" } else { "bp" };
    let mut note = format!("Identified by {} against {db} on {date}", program_name(program));
    if let Some(a) = author.filter(|a| !a.trim().is_empty()) {
        note.push_str(&format!(" by {}", a.trim()));
    }
    note.push_str(&format!(": {} ({acc})", title.trim()));
    if !organism.is_empty() {
        note.push_str(&format!(", {organism}"));
    }
    note.push_str(&format!(
        ", {:.1}% identity over {} {unit}, E-value {}.",
        hsp.identity_pct(),
        crate::editor::thousands(hsp.align_len),
        format_evalue(hsp.evalue)
    ));
    qualifiers.push(("note".to_string(), note));
    FeatureDraft { segments, strand, kind: kind.into(), name, qualifiers }
}

/// How the note names the program: "BLASTN (megablast)" reads oddly mid-sentence.
fn program_name(p: Program) -> &'static str {
    match p {
        Program::Megablast => "megablast",
        Program::DcMegablast => "discontiguous megablast",
        Program::Blastn => "BLASTN",
        Program::Blastx => "BLASTX",
        Program::Tblastx => "TBLASTX",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blast::{Description, Options};

    fn hit(title: &str, evalue: f64, bits: f64) -> Hit {
        Hit {
            description: vec![Description {
                id: "ref|NP_414542.1|".into(),
                accession: "NP_414542".into(),
                title: title.into(),
                taxid: Some(511145),
                sciname: Some("Escherichia coli str. K-12 substr. MG1655".into()),
            }],
            len: 300,
            hsps: vec![Hsp {
                bit_score: bits,
                score: bits * 2.0,
                evalue,
                identity: 237,
                align_len: 240,
                query_from: 4,
                query_to: 723,
                query_frame: Some(1),
                ..Default::default()
            }],
        }
    }

    #[test]
    fn sorting() {
        let hits = vec![hit("b", 1e-5, 50.0), hit("a", 1e-50, 200.0), hit("c", 1e-5, 60.0)];
        assert_eq!(sorted(&hits, Column::Evalue, true, 100), vec![1, 0, 2]);
        assert_eq!(sorted(&hits, Column::MaxScore, false, 100), vec![1, 2, 0]);
        assert_eq!(sorted(&hits, Column::Description, true, 100), vec![1, 0, 2]);
    }

    #[test]
    fn nucleotide_blocks_count_both_ways() {
        let hsp = Hsp {
            query_from: 1,
            query_to: 70,
            hit_from: 500,
            hit_to: 432,
            hit_strand: Some("Minus".into()),
            qseq: "A".repeat(70),
            hseq: format!("{}-{}", "A".repeat(59), "A".repeat(10)),
            midline: "|".repeat(70),
            ..Default::default()
        };
        let b = blocks(&hsp, Program::Blastn);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].query.0, b[0].query.2, b[0].subject.0, b[0].subject.2), (1, 60, 500, 442));
        assert_eq!((b[1].query.0, b[1].query.2, b[1].subject.0, b[1].subject.2), (61, 70, 441, 432));
        let lines = block_lines(&b[0], coord_width(&b));
        assert!(lines[0].starts_with("Query  1    AAAA"), "{}", lines[0]);
        assert!(lines[2].ends_with("  442"), "{}", lines[2]);
    }

    #[test]
    fn translated_blocks_step_by_three() {
        // blastx, frame -2: the query runs backwards, three bases a residue.
        let hsp = Hsp {
            query_from: 5,
            query_to: 202,
            query_frame: Some(-2),
            hit_from: 1,
            hit_to: 66,
            qseq: "M".repeat(66),
            hseq: "M".repeat(66),
            midline: "M".repeat(66),
            ..Default::default()
        };
        let b = blocks(&hsp, Program::Blastx);
        assert_eq!((b[0].query.0, b[0].query.2), (202, 23));
        assert_eq!((b[1].query.0, b[1].query.2), (22, 5));
        assert_eq!((b[0].subject.0, b[0].subject.2, b[1].subject.0, b[1].subject.2), (1, 60, 61, 66));
        assert_eq!(hsp_summary(&hsp, Program::Blastx)[2], "Frame -2");
    }

    #[test]
    fn segments_map_through_the_origin() {
        // The region started at base 90 of 100 and wraps; query 5..=20 is 94..100 + 0..10.
        let lock = Lock { id: 1, start: 90, end: 20 };
        assert_eq!(segments(&lock, 5, 20, 100), vec![(0, 10), (94, 100)]);
        assert_eq!(segments(&Lock { id: 1, start: 10, end: 50 }, 1, 40, 100), vec![(10, 50)]);
    }

    #[test]
    fn blastx_draft() {
        let mut o = Options::new(Program::Blastx);
        o.genetic_code = 11;
        let h = hit("thr operon leader peptide [Escherichia coli str. K-12 substr. MG1655]", 3.2e-150, 480.0);
        let results = BlastResults {
            rid: "R".into(),
            options: o,
            query_len: 800,
            database: "nr".into(),
            program: "blastx".into(),
            hits: vec![h.clone()],
            message: None,
            bases_hash: 0,
        };
        let lock = Lock { id: 1, start: 100, end: 900 };
        let d = draft(&results, &h, &h.hsps[0], &lock, 5000, Some("Ada"), "2026-11-23");
        assert_eq!(d.segments, vec![(103, 823)]);
        assert_eq!((d.kind.as_str(), d.strand, d.name.as_str()), ("CDS", 1, "thr operon leader peptide"));
        let q = |k: &str| d.qualifiers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(q("transl_table"), "11");
        assert_eq!(q("inference"), "similar to AA sequence:RefSeq:NP_414542.1");
        assert_eq!(
            q("note"),
            "Identified by BLASTX against nr on 2026-11-23 by Ada: thr operon leader peptide [Escherichia coli str. K-12 \
             substr. MG1655] (NP_414542.1), Escherichia coli str. K-12 substr. MG1655, 98.8% identity over 240 aa, E-value 3e-150."
        );
    }
}

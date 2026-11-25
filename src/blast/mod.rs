//! BLAST searches of a region at NCBI, through the BLAST URL API
//! (https://blast.ncbi.nlm.nih.gov/doc/blast-help/developerinfo.html).
//!
//! Following NCBI's usage rules: one search at a time (the task queue's "ncbi"
//! service), at least 10 seconds between any two requests, no more than one status
//! check per search a minute, the first not before NCBI's estimate (RTOE), and every
//! request names this tool and the user's email.

pub mod dialog;
pub mod results;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Deserialize;

use crate::tasks::{Output, TaskCtx, TaskRunner};

pub const ENDPOINT: &str = "https://blast.ncbi.nlm.nih.gov/Blast.cgi";
pub const TOOL: &str = "edict";
/// NCBI: no more than one request every 10 seconds.
const REQUEST_GAP: Duration = Duration::from_secs(10);
/// NCBI: don't check on a search more than once a minute.
const POLL_GAP: Duration = Duration::from_secs(60);
/// Give up on a search after this long.
const GIVE_UP: Duration = Duration::from_secs(3 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Megablast,
    DcMegablast,
    Blastn,
    Blastx,
    Tblastx,
}

impl Program {
    pub const ALL: [Program; 5] = [Program::Megablast, Program::DcMegablast, Program::Blastn, Program::Blastx, Program::Tblastx];

    pub fn id(self) -> &'static str {
        match self {
            Program::Megablast => "megablast",
            Program::DcMegablast => "dc-megablast",
            Program::Blastn => "blastn",
            Program::Blastx => "blastx",
            Program::Tblastx => "tblastx",
        }
    }

    pub fn from_id(s: &str) -> Option<Program> {
        Program::ALL.into_iter().find(|p| p.id() == s)
    }

    /// For titles and notes: "BLASTN (megablast)".
    pub fn title(self) -> &'static str {
        match self {
            Program::Megablast => "BLASTN (megablast)",
            Program::DcMegablast => "BLASTN (discontiguous megablast)",
            Program::Blastn => "BLASTN",
            Program::Blastx => "BLASTX",
            Program::Tblastx => "TBLASTX",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Program::Megablast => "Highly similar sequences",
            Program::DcMegablast => "More dissimilar sequences",
            Program::Blastn => "Somewhat similar sequences",
            Program::Blastx => "Translated query vs. proteins",
            Program::Tblastx => "Translated query vs. translated nucleotides",
        }
    }

    /// The query is translated: alignments are amino acids.
    pub fn translated(self) -> bool {
        matches!(self, Program::Blastx | Program::Tblastx)
    }

    /// Searches proteins rather than nucleotides.
    pub fn protein_db(self) -> bool {
        self == Program::Blastx
    }

    /// Hits are proteins: lengths in aa.
    pub fn subject_aa(self) -> bool {
        self == Program::Blastx
    }

    pub fn gapped(self) -> bool {
        self != Program::Tblastx
    }

    pub fn databases(self) -> &'static [(&'static str, &'static str)] {
        if self.protein_db() {
            &[
                ("clustered_nr", "ClusteredNR (faster; one hit per cluster)"),
                ("nr", "Non-redundant protein sequences"),
                ("refseq_select", "RefSeq Select proteins"),
                ("refseq_protein", "Reference proteins"),
                ("swissprot", "UniProtKB/Swiss-Prot"),
                ("pdb", "Protein Data Bank"),
            ]
        } else {
            &[
                ("core_nt", "Core nucleotide"),
                ("nt", "Nucleotide collection"),
                ("refseq_select_rna", "RefSeq Select RNA"),
                ("refseq_rna", "RefSeq RNA"),
                ("pdbnt", "PDB nucleotide"),
            ]
        }
    }

    pub fn word_sizes(self) -> &'static [u32] {
        match self {
            Program::Megablast => &[16, 20, 24, 28, 32, 48, 64, 128, 256],
            Program::DcMegablast => &[11, 12],
            Program::Blastn => &[7, 11, 15],
            Program::Blastx => &[2, 3, 5, 6],
            Program::Tblastx => &[2, 3],
        }
    }

    pub fn default_word(self) -> u32 {
        match self {
            Program::Megablast => 28,
            Program::DcMegablast => 11,
            Program::Blastn => 11,
            Program::Blastx => 5,
            Program::Tblastx => 3,
        }
    }

    pub fn default_scores(self) -> (i32, i32) {
        match self {
            Program::Megablast => (1, -2),
            _ => (2, -3),
        }
    }

    pub fn default_gaps(self) -> &'static str {
        match self {
            Program::Megablast => "linear",
            Program::DcMegablast | Program::Blastn => "5 2",
            Program::Blastx => "11 1",
            Program::Tblastx => "",
        }
    }
}

/// Match/mismatch scores NCBI offers.
pub const SCORES: &[(i32, i32)] = &[(1, -2), (1, -3), (1, -4), (2, -3), (4, -5), (1, -1)];
pub const NUCL_GAPS: &[&str] = &["linear", "5 2", "2 2", "1 2", "0 2", "3 1", "2 1", "1 1"];
pub const PROT_GAPS: &[&str] = &["11 1", "10 1", "9 1", "8 1", "13 2", "12 2", "11 2", "10 2", "9 2"];
pub const MATRICES: &[&str] = &["BLOSUM62", "BLOSUM45", "BLOSUM50", "BLOSUM80", "BLOSUM90", "PAM30", "PAM70"];
pub const MAX_TARGETS: &[u32] = &[10, 50, 100, 250, 500, 1000, 5000];
pub const COMPOSITION: &[(u8, &str)] = &[
    (0, "None"),
    (1, "Composition-based statistics"),
    (2, "Conditional compositional score matrix adjustment"),
    (3, "Universal compositional score matrix adjustment"),
];
/// tblastx searches translate both query and database in six frames: slow at NCBI.
pub const TBLASTX_LONG: usize = 1000;

/// Everything the BLAST form asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub program: Program,
    pub database: String,
    /// Organisms to search within, one per line or comma: names or taxids.
    pub organisms: String,
    pub exclude_organisms: String,
    pub exclude_models: bool,
    pub exclude_uncultured: bool,
    pub expect: String,
    pub max_targets: u32,
    pub word_size: u32,
    pub scores: (i32, i32),
    pub gaps: String,
    pub matrix: String,
    pub composition: u8,
    pub genetic_code: u8,
    pub low_complexity: bool,
    pub species_repeats: bool,
    pub mask_lookup_only: bool,
    pub mask_lowercase: bool,
    pub short_query: bool,
    pub title: String,
}

impl Options {
    pub fn new(program: Program) -> Options {
        Options {
            program,
            database: program.databases()[0].0.to_string(),
            organisms: String::new(),
            exclude_organisms: String::new(),
            exclude_models: false,
            exclude_uncultured: false,
            expect: "0.05".into(),
            max_targets: 100,
            word_size: program.default_word(),
            scores: program.default_scores(),
            gaps: program.default_gaps().into(),
            matrix: "BLOSUM62".into(),
            composition: if program == Program::Blastx { 2 } else { 0 },
            genetic_code: 1,
            low_complexity: true,
            species_repeats: false,
            mask_lookup_only: !program.translated(),
            mask_lowercase: false,
            short_query: true,
            title: String::new(),
        }
    }

    /// Switch program, taking its defaults for what depends on it and keeping the rest.
    pub fn set_program(&mut self, p: Program) {
        if p == self.program {
            return;
        }
        let was = self.program;
        let fresh = Options::new(p);
        self.word_size = fresh.word_size;
        self.scores = fresh.scores;
        self.gaps = fresh.gaps;
        self.composition = fresh.composition;
        self.mask_lookup_only = fresh.mask_lookup_only;
        if was.protein_db() != p.protein_db() {
            self.database = fresh.database;
        }
        if p.translated() && !was.translated() || !p.translated() && was.translated() {
            self.matrix = fresh.matrix;
        }
        self.program = p;
    }

    /// The options remembered in settings (not the title).
    pub fn to_map(&self) -> BTreeMap<String, String> {
        let b = |v: bool| if v { "true" } else { "false" }.to_string();
        let mut m = BTreeMap::new();
        m.insert("program".into(), self.program.id().into());
        m.insert(if self.program.protein_db() { "protein_database" } else { "database" }.into(), self.database.clone());
        m.insert("organisms".into(), self.organisms.replace('\n', ","));
        m.insert("exclude_organisms".into(), self.exclude_organisms.replace('\n', ","));
        m.insert("exclude_models".into(), b(self.exclude_models));
        m.insert("exclude_uncultured".into(), b(self.exclude_uncultured));
        m.insert("expect".into(), self.expect.clone());
        m.insert("max_targets".into(), self.max_targets.to_string());
        m.insert("matrix".into(), self.matrix.clone());
        m.insert("genetic_code".into(), self.genetic_code.to_string());
        m.insert("low_complexity".into(), b(self.low_complexity));
        m.insert("species_repeats".into(), b(self.species_repeats));
        m.insert("mask_lowercase".into(), b(self.mask_lowercase));
        m.insert("short_query".into(), b(self.short_query));
        m
    }

    /// Remembered options; program-specific scoring starts at the program's defaults.
    pub fn from_map(m: &BTreeMap<String, String>) -> Options {
        let program = m.get("program").and_then(|p| Program::from_id(p)).unwrap_or(Program::Megablast);
        let mut o = Options::new(program);
        let flag = |k: &str, d: bool| m.get(k).map_or(d, |v| v == "true");
        let key = if program.protein_db() { "protein_database" } else { "database" };
        if let Some(db) = m.get(key).filter(|v| !v.trim().is_empty()) {
            o.database = db.clone();
        }
        o.organisms = m.get("organisms").cloned().unwrap_or_default();
        o.exclude_organisms = m.get("exclude_organisms").cloned().unwrap_or_default();
        o.exclude_models = flag("exclude_models", o.exclude_models);
        o.exclude_uncultured = flag("exclude_uncultured", o.exclude_uncultured);
        if let Some(e) = m.get("expect").filter(|e| e.trim().parse::<f64>().is_ok()) {
            o.expect = e.clone();
        }
        if let Some(n) = m.get("max_targets").and_then(|n| n.parse().ok()).filter(|n| MAX_TARGETS.contains(n)) {
            o.max_targets = n;
        }
        if let Some(x) = m.get("matrix").filter(|x| MATRICES.contains(&x.as_str())) {
            o.matrix = x.clone();
        }
        if let Some(g) = m.get("genetic_code").and_then(|g| g.parse().ok()).filter(|g| crate::genetic_code::Code::get(*g).is_some()) {
            o.genetic_code = g;
        }
        o.low_complexity = flag("low_complexity", o.low_complexity);
        o.species_repeats = flag("species_repeats", o.species_repeats);
        o.mask_lowercase = flag("mask_lowercase", o.mask_lowercase);
        o.short_query = flag("short_query", o.short_query);
        o
    }

    /// The Entrez query restricting the search to (or away from) organisms.
    pub fn entrez_query(&self) -> Option<String> {
        let term = |o: &str| {
            let o = o.trim().trim_matches('"');
            if !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()) {
                format!("txid{o}[Organism:exp]")
            } else {
                format!("\"{o}\"[Organism]")
            }
        };
        let list = |s: &str| -> Vec<String> { s.split([',', ';', '\n']).filter(|o| !o.trim().is_empty()).map(term).collect() };
        let include = list(&self.organisms);
        let exclude = list(&self.exclude_organisms);
        let mut q = match include.len() {
            0 => String::new(),
            1 => include[0].clone(),
            _ => format!("({})", include.join(" OR ")),
        };
        if !exclude.is_empty() {
            if q.is_empty() {
                q = "all[filter]".into();
            }
            for e in exclude {
                q.push_str(&format!(" NOT {e}"));
            }
        }
        (!q.is_empty()).then_some(q)
    }

    /// What's wrong with the options, if anything.
    pub fn problem(&self) -> Option<String> {
        if self.database.trim().is_empty() || self.database.chars().any(char::is_whitespace) {
            return Some("Choose a database".into());
        }
        match self.expect.trim().parse::<f64>() {
            Ok(e) if e > 0.0 => {}
            _ => return Some("The E-value threshold must be a positive number".into()),
        }
        None
    }

    /// The form fields of a Put request for `query` (bases).
    pub fn put_form(&self, query: &str, email: &str) -> Vec<(&'static str, String)> {
        let p = self.program;
        let mut f: Vec<(&'static str, String)> = vec![("CMD", "Put".into())];
        let program = match p {
            Program::Megablast | Program::DcMegablast | Program::Blastn => "blastn",
            Program::Blastx => "blastx",
            Program::Tblastx => "tblastx",
        };
        f.push(("PROGRAM", program.into()));
        match p {
            Program::Megablast => f.push(("MEGABLAST", "on".into())),
            Program::DcMegablast => {
                // Discontiguous megablast: a coding template of length 18.
                f.push(("TEMPLATE_TYPE", "0".into()));
                f.push(("TEMPLATE_LENGTH", "18".into()));
            }
            _ => {}
        }
        f.push(("DATABASE", self.database.trim().into()));
        f.push(("QUERY", query.into()));
        f.push(("EXPECT", self.expect.trim().into()));
        f.push(("HITLIST_SIZE", self.max_targets.to_string()));
        f.push(("DESCRIPTIONS", self.max_targets.to_string()));
        f.push(("ALIGNMENTS", self.max_targets.to_string()));
        f.push(("WORD_SIZE", self.word_size.to_string()));
        if p.translated() {
            f.push(("MATRIX", self.matrix.clone()));
            f.push(("QUERY_GENETIC_CODE", self.genetic_code.to_string()));
            f.push(("GENETIC_CODE", self.genetic_code.to_string()));
        } else {
            f.push(("NUCL_REWARD", self.scores.0.to_string()));
            f.push(("NUCL_PENALTY", self.scores.1.to_string()));
        }
        if p.gapped() {
            let gaps = if self.gaps == "linear" { "0 0".to_string() } else { self.gaps.clone() };
            f.push(("GAPCOSTS", gaps));
        }
        if p == Program::Blastx {
            f.push(("COMPOSITION_BASED_STATISTICS", self.composition.to_string()));
        }
        // FILTER: L low complexity, R human repeats; m masks for the lookup table only.
        let mut filter = String::new();
        if self.mask_lookup_only && (self.low_complexity || self.species_repeats) {
            filter.push('m');
        }
        if self.low_complexity {
            filter.push('L');
        }
        if self.species_repeats && !p.translated() {
            filter.push('R');
        }
        f.push(("FILTER", if filter.is_empty() { "F".into() } else { filter }));
        if self.mask_lowercase {
            f.push(("LCASE_MASK", "on".into()));
        }
        if self.short_query {
            f.push(("SHORT_QUERY_ADJUST", "on".into()));
        }
        if let Some(q) = self.entrez_query() {
            f.push(("ENTREZ_QUERY", q));
        }
        if self.exclude_models {
            f.push(("EXCLUDE_MODELS", "on".into()));
        }
        if self.exclude_uncultured {
            f.push(("EXCLUDE_SEQ_UNCULT", "on".into()));
        }
        if !self.title.trim().is_empty() {
            f.push(("JOB_TITLE", self.title.trim().into()));
        }
        f.push(("tool", TOOL.into()));
        f.push(("email", email.into()));
        f
    }
}

/// From a Put response: the search's RID and NCBI's estimate of how many seconds it
/// will take.
pub fn parse_put(text: &str) -> Result<(String, u64)> {
    let block = text
        .split("QBlastInfoBegin")
        .nth(1)
        .and_then(|b| b.split("QBlastInfoEnd").next())
        .ok_or_else(|| anyhow!("NCBI didn't start the search: {}", page_message(text)))?;
    let field = |name: &str| {
        block.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == name).then(|| v.trim().to_string())
        })
    };
    let rid = field("RID").filter(|r| !r.is_empty()).ok_or_else(|| anyhow!("NCBI's reply has no RID"))?;
    let rtoe = field("RTOE").and_then(|t| t.parse().ok()).unwrap_or(30);
    Ok((rid, rtoe))
}

#[derive(Debug, PartialEq)]
pub enum SearchStatus {
    Waiting,
    Ready { hits: bool },
    Failed,
    Unknown,
}

/// From a SearchInfo response.
pub fn parse_status(text: &str) -> SearchStatus {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            (k.trim() == name).then(|| v.trim().to_string())
        })
    };
    match field("Status").as_deref() {
        Some("WAITING") => SearchStatus::Waiting,
        Some("READY") => SearchStatus::Ready { hits: field("ThereAreHits").as_deref() == Some("yes") },
        Some("FAILED") => SearchStatus::Failed,
        _ => SearchStatus::Unknown,
    }
}

/// The human-readable part of an HTML page, for error messages.
fn page_message(text: &str) -> String {
    // NCBI's error pages put the problem in an element with class "error".
    let from = text.find("class=\"error").map(|i| &text[i..]).unwrap_or(text);
    let mut out = String::new();
    let mut tag = false;
    for c in from.chars() {
        match c {
            '<' => tag = true,
            '>' => {
                tag = false;
                out.push(' ');
            }
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = out.split_whitespace().collect();
    let msg = words.join(" ");
    let msg = msg.trim_start_matches("class=\"error\"").trim();
    if msg.is_empty() {
        "no explanation given".into()
    } else {
        msg.chars().take(300).collect()
    }
}

// ----- Results (JSON2_S) -----

#[derive(Deserialize, Default)]
struct Json2 {
    #[serde(rename = "BlastOutput2", default)]
    outputs: Vec<Json2Output>,
}

#[derive(Deserialize, Default)]
struct Json2Output {
    #[serde(default)]
    report: Report,
}

#[derive(Deserialize, Default)]
struct Report {
    #[serde(default)]
    program: String,
    #[serde(default)]
    search_target: SearchTarget,
    #[serde(default)]
    results: ReportResults,
}

#[derive(Deserialize, Default)]
struct SearchTarget {
    #[serde(default)]
    db: String,
}

#[derive(Deserialize, Default)]
struct ReportResults {
    #[serde(default)]
    search: Search,
}

#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    query_len: usize,
    #[serde(default)]
    hits: Vec<Hit>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub struct Hit {
    #[serde(default)]
    pub description: Vec<Description>,
    #[serde(default)]
    pub len: usize,
    #[serde(default)]
    pub hsps: Vec<Hsp>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub struct Description {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub accession: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub taxid: Option<u64>,
    #[serde(default)]
    pub sciname: Option<String>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub struct Hsp {
    #[serde(default)]
    pub bit_score: f64,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub evalue: f64,
    #[serde(default)]
    pub identity: usize,
    #[serde(default)]
    pub positive: Option<usize>,
    #[serde(default)]
    pub query_from: usize,
    #[serde(default)]
    pub query_to: usize,
    #[serde(default)]
    pub query_strand: Option<String>,
    #[serde(default)]
    pub query_frame: Option<i32>,
    #[serde(default)]
    pub hit_from: usize,
    #[serde(default)]
    pub hit_to: usize,
    #[serde(default)]
    pub hit_strand: Option<String>,
    #[serde(default)]
    pub hit_frame: Option<i32>,
    #[serde(default)]
    pub align_len: usize,
    #[serde(default)]
    pub gaps: usize,
    #[serde(default)]
    pub qseq: String,
    #[serde(default)]
    pub hseq: String,
    #[serde(default)]
    pub midline: String,
}

impl Hsp {
    /// The query bases it covers, 1-based inclusive, low to high.
    pub fn query_span(&self) -> (usize, usize) {
        (self.query_from.min(self.query_to), self.query_from.max(self.query_to))
    }


    pub fn identity_pct(&self) -> f64 {
        if self.align_len == 0 {
            0.0
        } else {
            100.0 * self.identity as f64 / self.align_len as f64
        }
    }

    fn minus(s: &Option<String>) -> bool {
        s.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("minus"))
    }

    /// Which way the match runs along the query: -1 when the subject matches the
    /// query's reverse complement (or a reverse frame).
    pub fn strand(&self) -> i8 {
        let mut rev = Self::minus(&self.hit_strand) != Self::minus(&self.query_strand);
        if let Some(f) = self.query_frame.filter(|f| *f != 0) {
            rev = f < 0;
            // tblastx: both translated; a reverse subject frame flips it again.
            if self.hit_frame.is_some_and(|h| h < 0) {
                rev = !rev;
            }
        }
        if rev {
            -1
        } else {
            1
        }
    }
}

impl Hit {
    pub fn first(&self) -> Option<&Description> {
        self.description.first()
    }


    /// The accession with its version, as `/inference` wants ("NC_000913.3").
    pub fn accession_version(&self) -> String {
        let Some(d) = self.first() else { return String::new() };
        d.id.split('|')
            .find(|part| part.starts_with(&d.accession) && part[d.accession.len()..].starts_with('.'))
            .map(str::to_string)
            .or_else(|| (d.id.contains('.') && !d.id.contains('|')).then(|| d.id.clone()))
            .unwrap_or_else(|| d.accession.clone())
    }

    pub fn title(&self) -> String {
        self.first().map(|d| d.title.clone()).unwrap_or_default()
    }

    pub fn sciname(&self) -> String {
        self.first().and_then(|d| d.sciname.clone()).unwrap_or_default()
    }

    pub fn max_score(&self) -> f64 {
        self.hsps.iter().map(|h| h.bit_score).fold(0.0, f64::max)
    }

    pub fn total_score(&self) -> f64 {
        self.hsps.iter().map(|h| h.bit_score).sum()
    }

    pub fn evalue(&self) -> f64 {
        self.hsps.iter().map(|h| h.evalue).fold(f64::INFINITY, f64::min)
    }

    /// The share of the query any HSP covers, in percent.
    pub fn query_cover(&self, query_len: usize) -> f64 {
        if query_len == 0 {
            return 0.0;
        }
        let mut spans: Vec<(usize, usize)> = self.hsps.iter().map(|h| h.query_span()).collect();
        spans.sort();
        let mut covered = 0;
        let mut reach = 0;
        for (a, b) in spans {
            let a = a.max(reach + 1);
            if b >= a {
                covered += b - a + 1;
            }
            reach = reach.max(b);
        }
        100.0 * covered.min(query_len) as f64 / query_len as f64
    }

    /// The best HSP's identity.
    pub fn identity_pct(&self) -> f64 {
        self.best().map_or(0.0, Hsp::identity_pct)
    }

    pub fn best(&self) -> Option<&Hsp> {
        self.hsps.iter().max_by(|a, b| a.bit_score.total_cmp(&b.bit_score))
    }
}

/// A finished search.
#[derive(Debug)]
pub struct BlastResults {
    pub rid: String,
    pub options: Options,
    pub query_len: usize,
    pub database: String,
    pub program: String,
    pub hits: Vec<Hit>,
    pub message: Option<String>,
    /// Hash of the region's bases when the search was sent.
    pub bases_hash: u64,
}

impl BlastResults {
    /// NCBI's own report for the search (kept there for about 36 hours).
    pub fn report_url(&self) -> String {
        format!("{ENDPOINT}?CMD=Get&RID={}", self.rid)
    }
}

pub fn record_url(accession: &str, protein: bool) -> String {
    let db = if protein { "protein" } else { "nuccore" };
    format!("https://www.ncbi.nlm.nih.gov/{db}/{accession}")
}

/// A JSON2_S report: query length, database, program, hits and any message.
pub type Parsed = (usize, String, String, Vec<Hit>, Option<String>);

/// Parse a JSON2_S report.
pub fn parse_json2s(text: &str) -> Result<Parsed> {
    let j: Json2 = serde_json::from_str(text).context("NCBI's results aren't the JSON expected")?;
    let Some(out) = j.outputs.into_iter().next() else { bail!("NCBI's results hold no report") };
    let r = out.report;
    Ok((r.results.search.query_len, r.search_target.db, r.program, r.results.search.hits, r.results.search.message))
}

pub fn hash(bases: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bases.to_ascii_uppercase().hash(&mut h);
    h.finish()
}

// ----- Requests -----

/// Keeps requests to NCBI at least `gap` apart.
pub struct Throttle {
    last: Option<Instant>,
    gap: Duration,
}

impl Throttle {
    pub const fn new(gap: Duration) -> Throttle {
        Throttle { last: None, gap }
    }

    /// How long to wait at `now` before the next request.
    pub fn wait(&self, now: Instant) -> Duration {
        match self.last {
            Some(last) => (last + self.gap).saturating_duration_since(now),
            None => Duration::ZERO,
        }
    }

    pub fn sent(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

static THROTTLE: Mutex<Throttle> = Mutex::new(Throttle::new(REQUEST_GAP));

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(format!("{TOOL}/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
}

/// Wait our turn, then send.
fn request<T>(ctx: &TaskCtx, f: impl FnOnce() -> std::result::Result<T, ureq::Error>) -> Result<T> {
    loop {
        let wait = THROTTLE.lock().unwrap().wait(Instant::now());
        if wait.is_zero() {
            break;
        }
        ctx.sleep(wait)?;
    }
    THROTTLE.lock().unwrap().sent(Instant::now());
    f().map_err(|e| anyhow!("{e}"))
}

fn body(r: ureq::http::Response<ureq::Body>) -> Result<String> {
    let mut r = r;
    r.body_mut().with_config().limit(256 * 1024 * 1024).read_to_string().map_err(|e| anyhow!("reading NCBI's reply: {e}"))
}

/// Runs one search: submit, wait, fetch.
pub struct BlastRunner {
    pub options: Options,
    pub query: String,
    pub email: String,
}

impl TaskRunner for BlastRunner {
    fn service(&self) -> &'static str {
        "ncbi"
    }

    fn run(self: Box<Self>, ctx: &TaskCtx) -> Result<Output> {
        let o = &self.options;
        let agent = agent();
        let hash = hash(self.query.as_bytes());
        ctx.log(format!("{} of {} bp against {}", o.program.title(), self.query.len(), o.database));
        if o.database == "nr" {
            ctx.log("nr is large and searches of it can take a long time; ClusteredNR is usually much faster");
        }
        if let Some(q) = o.entrez_query() {
            ctx.log(format!("Limited to: {q}"));
        }
        ctx.progress("Waiting to submit (NCBI asks for 10 s between requests)");
        let form = o.put_form(&self.query, &self.email);
        let put = request(ctx, || agent.post(ENDPOINT).send_form(form.iter().map(|(k, v)| (*k, v.as_str()))))?;
        let (rid, rtoe) = parse_put(&body(put)?)?;
        ctx.log(format!("Submitted · RID {rid}"));
        ctx.log(format!("NCBI expects it to take about {rtoe} s"));

        let started = Instant::now();
        // The first check waits for NCBI's estimate; later ones a minute apart.
        let mut next = Duration::from_secs(rtoe.max(10));
        let mut checks = 0;
        let hits = loop {
            ctx.progress(format!("Searching · RID {rid} · next check in {} s", next.as_secs()));
            ctx.sleep(next)?;
            next = POLL_GAP;
            checks += 1;
            let info = request(ctx, || {
                agent
                    .get(ENDPOINT)
                    .query("CMD", "Get")
                    .query("FORMAT_OBJECT", "SearchInfo")
                    .query("RID", &rid)
                    .query("tool", TOOL)
                    .query("email", &self.email)
                    .call()
            })?;
            match parse_status(&body(info)?) {
                SearchStatus::Waiting => {
                    let t = started.elapsed().as_secs();
                    ctx.log(format!("Still searching ({}:{:02} elapsed, check {checks})", t / 60, t % 60));
                    if started.elapsed() > GIVE_UP {
                        bail!("NCBI hasn't finished after three hours; giving up");
                    }
                }
                SearchStatus::Ready { hits } => break hits,
                SearchStatus::Failed => bail!("NCBI reports the search failed (RID {rid})"),
                SearchStatus::Unknown => bail!("NCBI doesn't know search {rid}; it may have expired"),
            }
        };
        let mut results = BlastResults {
            rid: rid.clone(),
            options: o.clone(),
            query_len: self.query.len(),
            database: o.database.clone(),
            program: o.program.id().into(),
            hits: Vec::new(),
            message: None,
            bases_hash: hash,
        };
        if !hits {
            ctx.log("Finished: no matches found");
            return Ok(Arc::new(results));
        }
        ctx.log("Finished; fetching the results");
        ctx.progress("Downloading results");
        let get = request(ctx, || {
            agent
                .get(ENDPOINT)
                .query("CMD", "Get")
                .query("RID", &rid)
                .query("FORMAT_TYPE", "JSON2_S")
                .query("tool", TOOL)
                .query("email", &self.email)
                .call()
        })?;
        let text = body(get)?;
        let (query_len, db, program, hits, message) = parse_json2s(&text)?;
        if query_len > 0 {
            results.query_len = query_len;
        }
        if !db.is_empty() {
            results.database = db;
        }
        if !program.is_empty() {
            results.program = program;
        }
        results.hits = hits;
        if let Some(m) = &message {
            ctx.warn(format!("NCBI: {m}"));
        }
        results.message = message;
        ctx.log(format!("{} matching sequences", results.hits.len()));
        Ok(Arc::new(results))
    }
}

// ----- Dates -----

/// `t` as a local calendar date, YYYY-MM-DD (UTC where the zone can't be found).
pub fn date(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = local_ymd(secs).unwrap_or_else(|| civil_from_days(secs.div_euclid(86_400)));
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(unix)]
fn local_ymd(secs: i64) -> Option<(i64, u32, u32)> {
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes only into `tm`, which outlives the call.
    let r = unsafe { libc::localtime_r(&t, &mut tm) };
    (!r.is_null()).then(|| (tm.tm_year as i64 + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32))
}

#[cfg(not(unix))]
fn local_ymd(_secs: i64) -> Option<(i64, u32, u32)> {
    None
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "3e-150", "0.002", "0.0" in NCBI's style.
pub fn format_evalue(e: f64) -> String {
    if e == 0.0 {
        "0.0".into()
    } else if e < 1e-4 {
        let s = format!("{e:.0e}");
        s.replace("e-0", "e-")
    } else if e < 0.1 {
        format!("{e:.4}").trim_end_matches('0').to_string()
    } else {
        format!("{e:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_reply_gives_rid_and_estimate() {
        let page = "<html><!--QBlastInfoBegin\n    RID = 9W6Z1Y0K016\n    RTOE = 27\nQBlastInfoEnd\n--></html>";
        assert_eq!(parse_put(page).unwrap(), ("9W6Z1Y0K016".into(), 27));
        let err = parse_put("<html><p class=\"error\">Query contains <b>no</b> sequence</p></html>").unwrap_err();
        assert!(err.to_string().contains("Query contains no sequence"), "{err}");
    }

    #[test]
    fn search_info_statuses() {
        assert_eq!(parse_status("<!--\nQBlastInfoBegin\n\tStatus=WAITING\nQBlastInfoEnd\n-->"), SearchStatus::Waiting);
        assert_eq!(
            parse_status("QBlastInfoBegin\n\tStatus=READY\nQBlastInfoEnd\nQBlastInfoBegin\n\tThereAreHits=yes\nQBlastInfoEnd"),
            SearchStatus::Ready { hits: true }
        );
        assert_eq!(parse_status("Status=READY\n"), SearchStatus::Ready { hits: false });
        assert_eq!(parse_status("Status=UNKNOWN"), SearchStatus::Unknown);
        assert_eq!(parse_status("Status=FAILED"), SearchStatus::Failed);
    }

    #[test]
    fn program_defaults() {
        let m = Options::new(Program::Megablast);
        assert_eq!((m.word_size, m.scores, m.gaps.as_str()), (28, (1, -2), "linear"));
        let mut o = m.clone();
        o.set_program(Program::Blastn);
        assert_eq!((o.word_size, o.scores, o.gaps.as_str(), o.database.as_str()), (11, (2, -3), "5 2", "core_nt"));
        o.set_program(Program::Blastx);
        assert_eq!((o.database.as_str(), o.composition, o.gaps.as_str()), ("clustered_nr", 2, "11 1"));
    }

    #[test]
    fn put_form_fields() {
        let mut o = Options::new(Program::Megablast);
        o.organisms = "Escherichia coli, 9606".into();
        o.exclude_organisms = "uncultured bacterium".into();
        let f = o.put_form("ACGT", "a@b.c");
        let get = |k: &str| f.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("PROGRAM"), Some("blastn"));
        assert_eq!(get("MEGABLAST"), Some("on"));
        assert_eq!(get("GAPCOSTS"), Some("0 0"));
        assert_eq!(get("FILTER"), Some("mL"));
        assert_eq!(get("NUCL_PENALTY"), Some("-2"));
        assert_eq!(get("email"), Some("a@b.c"));
        assert_eq!(get("tool"), Some("edict"));
        assert_eq!(
            get("ENTREZ_QUERY"),
            Some("(\"Escherichia coli\"[Organism] OR txid9606[Organism:exp]) NOT \"uncultured bacterium\"[Organism]")
        );
        let mut t = Options::new(Program::Tblastx);
        t.low_complexity = false;
        let f = t.put_form("ACGT", "a@b.c");
        let get = |k: &str| f.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("GAPCOSTS"), None);
        assert_eq!(get("FILTER"), Some("F"));
        assert_eq!(get("MATRIX"), Some("BLOSUM62"));
    }

    #[test]
    fn options_round_trip_through_settings() {
        let mut o = Options::new(Program::Blastx);
        o.organisms = "Homo sapiens".into();
        o.max_targets = 250;
        o.genetic_code = 11;
        o.exclude_models = true;
        let back = Options::from_map(&o.to_map());
        assert_eq!(back, o);
    }

    const SAMPLE: &str = r#"{"BlastOutput2":[{"report":{"program":"blastn","version":"BLASTN 2.16.0+",
      "search_target":{"db":"core_nt"},"results":{"search":{"query_id":"Query_1","query_len":120,
      "hits":[{"num":1,"description":[{"id":"gi|123|gb|L09137.2|SYNPUC19CV","accession":"L09137",
      "title":"pUC19c cloning vector","taxid":322367,"sciname":"synthetic construct"}],"len":2686,
      "hsps":[{"num":1,"bit_score":217.0,"score":117,"evalue":1.5e-52,"identity":118,"query_from":1,
      "query_to":120,"query_strand":"Plus","hit_from":500,"hit_to":381,"hit_strand":"Minus","align_len":120,
      "gaps":1,"qseq":"ACGT","hseq":"ACGT","midline":"||||"},{"num":2,"bit_score":40.1,"evalue":0.01,"identity":20,
      "query_from":100,"query_to":120,"align_len":21,"hit_from":1,"hit_to":21}]}]}}}}]}"#;

    #[test]
    fn parses_json2s() {
        let (qlen, db, program, hits, _) = parse_json2s(SAMPLE).unwrap();
        assert_eq!((qlen, db.as_str(), program.as_str(), hits.len()), (120, "core_nt", "blastn", 1));
        let h = &hits[0];
        assert_eq!(h.accession_version(), "L09137.2");
        assert_eq!(h.sciname(), "synthetic construct");
        assert_eq!(h.max_score(), 217.0);
        assert!((h.total_score() - 257.1).abs() < 1e-9);
        assert_eq!(h.query_cover(120), 100.0);
        assert_eq!(h.hsps[0].strand(), -1);
        assert_eq!(h.hsps[1].strand(), 1);
        assert_eq!(format_evalue(h.evalue()), "2e-52");
    }

    #[test]
    fn translated_strands() {
        let h = Hsp { query_frame: Some(-2), ..Default::default() };
        assert_eq!(h.strand(), -1);
        let h = Hsp { query_frame: Some(3), hit_frame: Some(-1), ..Default::default() };
        assert_eq!(h.strand(), -1);
        let h = Hsp { query_frame: Some(-1), hit_frame: Some(-1), ..Default::default() };
        assert_eq!(h.strand(), 1);
    }

    #[test]
    fn throttle_spaces_requests() {
        let mut t = Throttle::new(Duration::from_secs(10));
        let t0 = Instant::now();
        assert_eq!(t.wait(t0), Duration::ZERO);
        t.sent(t0);
        assert_eq!(t.wait(t0 + Duration::from_secs(4)), Duration::from_secs(6));
        assert_eq!(t.wait(t0 + Duration::from_secs(11)), Duration::ZERO);
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_780), (2026, 11, 23));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(format_evalue(0.0), "0.0");
        assert_eq!(format_evalue(0.002), "0.002");
        assert_eq!(format_evalue(3.2e-150), "3e-150");
    }

    /// Runs a real search at NCBI (takes a minute or more):
    /// `BLAST_EMAIL=you@example.org cargo test live_search -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_search() {
        let email = std::env::var("BLAST_EMAIL").expect("set BLAST_EMAIL");
        let (tx, rx) = async_channel::unbounded();
        let ctx = TaskCtx::for_test(tx);
        // The start of the pUC19 lacZα region.
        let query = "TGTAAAACGACGGCCAGTGAATTCGAGCTCGGTACCCGGGGATCCTCTAGAGTCGACCTGCAGGCATGCAAGCTTGGCGTAATCATGGTCATAGCTGTTTCC";
        let runner = Box::new(BlastRunner { options: Options::new(Program::Megablast), query: query.into(), email });
        let out = runner.run(&ctx);
        while let Ok(m) = rx.try_recv() {
            eprintln!("{}", crate::tasks::debug_msg(&m));
        }
        let out = out.unwrap();
        let r = out.downcast_ref::<BlastResults>().unwrap();
        eprintln!("{} hits; best {}", r.hits.len(), r.hits.first().map(|h| h.title()).unwrap_or_default());
        assert!(!r.hits.is_empty());
    }
}

//! Sequence document model: loading, edits that keep features in place, undo, saving.
//!
//! Pure data, no UI, so everything here is unit-testable.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context as _, Result};
use gb_io::seq::{Feature as GbFeature, Location, Seq, Topology};
use serde::{Deserialize, Serialize};

use crate::pgx::{self, SavePoint, Structure};
use crate::genetic_code;
use crate::snapgene;

/// A feature reduced to what the editor draws: half-open `[start, end)` segments in
/// sequence order. A feature that wraps the origin of a circular sequence is two segments.
#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    pub kind: String,
    pub label: String,
    pub segments: Vec<(usize, usize)>,
    /// +1 forward, -1 reverse, 0 none.
    pub strand: i8,
    /// The original GenBank feature, kept so its qualifiers survive a save.
    pub source: Option<GbFeature>,
}

impl Feature {
    /// The structure chains this feature encodes, from its `/structure="<id>:<chain>"`
    /// qualifiers: (structure id, chain id).
    pub fn structure_refs(&self) -> Vec<(String, String)> {
        let Some(src) = &self.source else { return Vec::new() };
        src.qualifier_values(pgx::STRUCTURE_QUALIFIER)
            .filter_map(|v| v.trim().split_once(':'))
            .map(|(s, c)| (s.to_string(), c.to_string()))
            .collect()
    }

    /// The feature's GenBank qualifiers, made (with just its label) if it had none.
    pub fn gb_source(&mut self) -> Option<&mut GbFeature> {
        if self.source.is_none() {
            let location = segments_location(&self.segments, self.strand)?;
            self.source = Some(GbFeature {
                kind: self.kind.clone().into(),
                location,
                qualifiers: vec![("label".into(), Some(self.label.clone()))],
            });
        }
        self.source.as_mut()
    }

    /// Make `chains` the chains of structure `id` this feature encodes, replacing any
    /// it named before. Gives the feature GenBank qualifiers if it had none.
    pub fn set_structure_chains(&mut self, id: &str, chains: &[String]) {
        if chains.is_empty() && self.source.is_none() {
            return;
        }
        let Some(src) = self.gb_source() else { return };
        src.qualifiers.retain(|(k, v)| {
            k != pgx::STRUCTURE_QUALIFIER || v.as_deref().and_then(|v| v.trim().split_once(':')).is_none_or(|(s, _)| s != id)
        });
        for c in chains {
            src.qualifiers.push((pgx::STRUCTURE_QUALIFIER.into(), Some(format!("{id}:{c}"))));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Our zip container (`.pgx`), the only format that keeps edit history.
    Native,
    GenBank,
    Fasta,
    /// SnapGene `.dna`: read-only.
    SnapGene,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Format> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            pgx::EXTENSION => Some(Format::Native),
            "fa" | "fasta" | "fna" | "fas" => Some(Format::Fasta),
            "gb" | "gbk" | "genbank" | "gbff" | "ape" => Some(Format::GenBank),
            "dna" => Some(Format::SnapGene),
            _ => None,
        }
    }

    pub fn writable(self) -> bool {
        self != Format::SnapGene
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Native => "Plasmid (.pgx)",
            Format::GenBank => "GenBank",
            Format::Fasta => "FASTA",
            Format::SnapGene => "SnapGene",
        }
    }
}

/// A feature as the edit log records it: everything a save keeps of it, in the
/// coordinates of the revision it was logged at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureRecord {
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    /// +1 forward, -1 reverse, 0 none.
    #[serde(default)]
    pub strand: i8,
    /// Half-open, 0-based `[start, end)` pairs in order.
    pub segments: Vec<[usize; 2]>,
    /// GenBank qualifiers in file order; `null` for valueless ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<(String, Option<String>)>,
}

impl FeatureRecord {
    pub fn of(f: &Feature) -> Self {
        FeatureRecord {
            kind: f.kind.clone(),
            label: f.label.clone(),
            strand: f.strand,
            segments: f.segments.iter().map(|&(s, e)| [s, e]).collect(),
            qualifiers: to_gb_feature(f).qualifiers.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    pub fn to_feature(&self) -> Feature {
        let segments: Vec<(usize, usize)> = self.segments.iter().map(|g| (g[0], g[1])).collect();
        let source = segments_location(&segments, self.strand).map(|location| GbFeature {
            kind: self.kind.clone().into(),
            location,
            qualifiers: self.qualifiers.iter().map(|(k, v)| (k.clone().into(), v.clone())).collect(),
        });
        Feature { kind: self.kind.clone(), label: self.label.clone(), segments, strand: self.strand, source }
    }

    /// The span from its first base to its last.
    pub fn extent(&self) -> Option<(usize, usize)> {
        let s = self.segments.iter().map(|g| g[0]).min()?;
        let e = self.segments.iter().map(|g| g[1]).max()?;
        Some((s, e))
    }
}

/// One step of the persistent edit log: every change a save keeps. Sequence steps
/// carry the bases involved, so the log can be undone backwards from the current
/// sequence without checkpoints, and feature steps carry the feature before and after.
///
/// Changes a sequence edit makes to features (shifting them, growing or trimming
/// them, dropping ones whose bases are all deleted) follow from that edit and aren't
/// logged separately.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Insert { pos: usize, bases: String },
    Delete { pos: usize, bases: String },
    /// Feature `ix` was added (it is the `ix`th feature afterwards).
    AddFeature { ix: usize, feature: FeatureRecord },
    /// Feature `ix` was removed.
    RemoveFeature { ix: usize, feature: FeatureRecord },
    EditFeature { ix: usize, before: FeatureRecord, after: FeatureRecord },
    /// A 3D structure was attached. Its file is kept with the document, not in the log.
    AttachStructure {
        id: String,
        name: String,
        format: pgx::StructureFormat,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        bytes: usize,
    },
    RemoveStructure { id: String, name: String },
    LinkStructure { id: String, name: String, before: pgx::ChainLinking, after: pgx::ChainLinking },
    /// The record took a new name (Save As).
    Rename { before: String, after: String },
    /// Logged just before a step's base edits: the features, as they were before it,
    /// that the edits changed other than by moving them (cut short, dropped, given a
    /// new translation, split at a new origin). With it, reading the log backwards
    /// restores features exactly. `all` replaces the whole list; `changed` lists
    /// (index, feature).
    Features {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<(usize, FeatureRecord)>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        all: Option<Vec<FeatureRecord>>,
    },
    /// The changes that follow, up to the next step, put the document back as it was
    /// at revision `to`.
    Revert { to: u64 },
}

impl Op {
    /// Inserts and deletes: the steps that change the bases.
    pub fn is_sequence(&self) -> bool {
        matches!(self, Op::Insert { .. } | Op::Delete { .. })
    }

    /// The step that exactly reverses this one, if a single step can: attaching and
    /// removing a structure can't, since the log doesn't hold structure files.
    pub fn reversal(&self) -> Option<Op> {
        Some(match self {
            Op::Insert { pos, bases } => Op::Delete { pos: *pos, bases: bases.clone() },
            Op::Delete { pos, bases } => Op::Insert { pos: *pos, bases: bases.clone() },
            Op::AddFeature { ix, feature } => Op::RemoveFeature { ix: *ix, feature: feature.clone() },
            Op::RemoveFeature { ix, feature } => Op::AddFeature { ix: *ix, feature: feature.clone() },
            Op::EditFeature { ix, before, after } => Op::EditFeature { ix: *ix, before: after.clone(), after: before.clone() },
            Op::LinkStructure { id, name, before, after } => {
                Op::LinkStructure { id: id.clone(), name: name.clone(), before: *after, after: *before }
            }
            Op::Rename { before, after } => Op::Rename { before: after.clone(), after: before.clone() },
            Op::AttachStructure { .. } | Op::RemoveStructure { .. } | Op::Features { .. } | Op::Revert { .. } => return None,
        })
    }

    /// Records kept for reading the log backwards, not changes in their own right.
    pub fn is_bookkeeping(&self) -> bool {
        matches!(self, Op::Features { .. })
    }

    /// Undo steps only hold sequence and feature changes, which always reverse.
    fn inverse(&self) -> Op {
        self.reversal().unwrap_or_else(|| self.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edit {
    /// The document revision this edit produced.
    pub rev: u64,
    /// Unix seconds.
    pub time: u64,
    /// Who made it: the `author` setting, or the account's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(flatten)]
    pub op: Op,
}

thread_local! {
    static AUTHOR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Name the person whose edits and saves are logged from now on.
pub fn set_author(name: Option<String>) {
    AUTHOR.with(|a| *a.borrow_mut() = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()));
}

pub fn author() -> Option<String> {
    AUTHOR.with(|a| a.borrow().clone())
}

#[derive(Clone)]
struct Snapshot {
    seq: Rc<Vec<u8>>,
    features: Rc<Vec<Feature>>,
}

pub struct Document {
    pub path: PathBuf,
    pub name: String,
    pub format: Format,
    pub circular: bool,
    pub seq: Rc<Vec<u8>>,
    pub features: Rc<Vec<Feature>>,
    /// Bumped on every change; layout caches key on it.
    pub version: u64,
    pub dirty: bool,
    /// Created with New File and never saved: the first save asks where to put it.
    pub untitled: bool,
    /// Persistent revision counter: one per edit (undo and redo count), across sessions.
    pub revision: u64,
    /// Every edit, oldest first, including ones loaded from a `.pgx`.
    pub history: Vec<Edit>,
    pub saves: Vec<SavePoint>,
    /// Attached 3D structures. Only `.pgx` keeps them.
    pub structures: Vec<Structure>,
    /// Problems found while loading that didn't stop the file opening.
    pub warnings: Vec<String>,
    record: Option<Seq>,
    /// `.pgx` entries this build doesn't understand, written back unchanged.
    extras: Vec<(String, Vec<u8>)>,
    /// Each undo step restores one snapshot; it may cover several logged ops.
    undo: Vec<(Snapshot, Vec<Op>)>,
    redo: Vec<(Snapshot, Vec<Op>)>,
}

const SEQ_EXTENSIONS: &[&str] =
    &[pgx::EXTENSION, "gb", "gbk", "genbank", "gbff", "ape", "fa", "fasta", "fna", "fas", "dna"];

pub fn is_sequence_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SEQ_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Document {
    /// Open by content first (zip, SnapGene header, `>`), falling back to the extension.
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if pgx::looks_like_zip(&bytes) {
            let b = pgx::read(&bytes).context("reading plasmid file")?;
            let mut doc = Self::from_record(path, b.record, Format::Native);
            doc.revision = b.revision;
            doc.history = b.history;
            doc.saves = b.saves;
            doc.extras = b.extras;
            doc.structures = b.structures;
            doc.warnings = b.warnings;
            return Ok(doc);
        }
        if snapgene::looks_like_snapgene(&bytes) {
            let record = snapgene::parse(&bytes).context("reading SnapGene file")?;
            return Ok(Self::from_record(path, record, Format::SnapGene));
        }
        if Format::from_path(path) == Some(Format::SnapGene) {
            bail!("not a SnapGene file (missing the SnapGene header)");
        }
        if bytes.first() == Some(&b'>') || Format::from_path(path) == Some(Format::Fasta) {
            return Self::from_fasta(path, &bytes);
        }
        let mut records = gb_io::reader::parse_slice(&bytes).context("parsing GenBank")?;
        if records.is_empty() {
            bail!("no GenBank records found");
        }
        Ok(Self::from_record(path, records.swap_remove(0), Format::GenBank))
    }

    fn blank(path: &Path, format: Format) -> Self {
        Self {
            path: path.to_path_buf(),
            name: file_stem(path),
            format,
            circular: false,
            seq: Rc::new(Vec::new()),
            features: Rc::new(Vec::new()),
            version: 0,
            dirty: false,
            untitled: false,
            revision: 0,
            history: Vec::new(),
            saves: Vec::new(),
            warnings: Vec::new(),
            record: None,
            extras: Vec::new(),
            structures: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A `.pgx` keeps its bases' case exactly (its log depends on it); other formats
    /// go through [`conventional_case`].
    fn from_record(path: &Path, mut record: Seq, format: Format) -> Self {
        if format != Format::Native {
            record.seq = conventional_case(std::mem::take(&mut record.seq));
        }
        let len = record.seq.len();
        let mut features: Vec<Feature> = record
            .features
            .iter()
            .filter(|f| f.kind != "source")
            .filter_map(|f| convert_feature(f, len))
            .collect();
        hide_redundant_genes(&mut features);
        let mut doc = Self::blank(path, format);
        doc.circular = record.topology == Topology::Circular;
        doc.seq = Rc::new(std::mem::take(&mut record.seq));
        doc.features = Rc::new(features);
        doc.record = Some(record);
        doc
    }

    fn from_fasta(path: &Path, bytes: &[u8]) -> Result<Self> {
        let text = String::from_utf8_lossy(bytes);
        let mut seq = Vec::new();
        let mut seen_header = false;
        for line in text.lines() {
            if line.starts_with('>') {
                if seen_header {
                    break; // first record only
                }
                seen_header = true;
                continue;
            }
            seq.extend(line.bytes().filter(|b| b.is_ascii_alphabetic()));
        }
        let mut doc = Self::blank(path, Format::Fasta);
        doc.seq = Rc::new(conventional_case(seq));
        Ok(doc)
    }

    /// An empty document that exists only in memory until its first save, which asks
    /// where to put it. `path` is the suggested location; its extension picks the format.
    pub fn new_untitled(path: &Path) -> Self {
        let format = Format::from_path(path).filter(|f| f.writable()).unwrap_or(Format::Native);
        let mut doc = Self::blank(path, format);
        doc.untitled = true;
        doc
    }

    /// Point the document at another file (Save As). The extension picks the format;
    /// saving GenBank as FASTA drops the features, which FASTA cannot hold.
    pub fn set_path(&mut self, path: &Path) -> Result<()> {
        let format = Format::from_path(path).unwrap_or(self.format);
        if !format.writable() {
            bail!("{} files can't be written; pick .{}, .gb or .fa", format.name(), pgx::EXTENSION);
        }
        self.format = format;
        self.path = path.to_path_buf();
        let name = file_stem(path);
        if self.untitled {
            self.name = name;
        } else if name != self.name {
            // The record is saved under the new name, so the log says so.
            let before = std::mem::replace(&mut self.name, name.clone());
            self.log(Op::Rename { before, after: name });
        }
        self.untitled = false;
        if let Some(record) = &mut self.record {
            record.name = Some(locus_name(&self.name));
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.seq.len()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { seq: self.seq.clone(), features: self.features.clone() }
    }

    fn log(&mut self, op: Op) {
        self.revision += 1;
        self.history.push(Edit { rev: self.revision, time: now(), author: author(), op });
    }

    /// Log a change that isn't an undo step (structures, linking).
    fn changed_for_good(&mut self, op: Op) {
        self.log(op);
        self.version += 1;
        self.dirty = true;
    }

    fn changed(&mut self, before: Snapshot, ops: Vec<Op>) {
        for op in &ops {
            self.log(op.clone());
        }
        self.undo.push((before, ops));
        self.redo.clear();
        self.version += 1;
        self.dirty = true;
    }

    /// Insert `bases` at cursor position `pos` (0..=len). Features that contain the
    /// insertion point grow; features after it shift.
    pub fn insert(&mut self, pos: usize, bases: &[u8]) {
        if bases.is_empty() {
            return;
        }
        let before = self.snapshot();
        let ops = self.apply_insert(pos, bases);
        self.changed(before, ops);
    }

    fn apply_insert(&mut self, pos: usize, bases: &[u8]) -> Vec<Op> {
        let n = bases.len();
        Rc::make_mut(&mut self.seq).splice(pos..pos, bases.iter().copied());
        let mut touched = Vec::new();
        let mut was = Vec::new();
        for (fi, f) in Rc::make_mut(&mut self.features).iter_mut().enumerate() {
            if f.segments.iter().any(|&(s, e)| s < pos && pos < e) {
                touched.push(fi);
                was.push((fi, FeatureRecord::of(f)));
            }
            for (s, e) in f.segments.iter_mut() {
                if pos <= *s {
                    *s += n;
                    *e += n;
                } else if pos < *e {
                    *e += n;
                }
            }
        }
        self.reencode(&touched);
        with_features(was, Op::Insert { pos, bases: String::from_utf8_lossy(bases).into_owned() })
    }

    /// Delete `[a, b)`. Features shrink; ones left empty are removed.
    pub fn delete(&mut self, a: usize, b: usize) {
        if a >= b {
            return;
        }
        let before = self.snapshot();
        let ops = self.apply_delete(a, b);
        self.changed(before, ops);
    }

    /// Delete `[start, len)` and `[0, end)` of a circular sequence: a selection that
    /// runs through the origin. One undo step.
    pub fn delete_across_origin(&mut self, start: usize, end: usize) {
        let len = self.len();
        let start = start.min(len);
        let end = end.min(start);
        if start == len && end == 0 {
            return;
        }
        let before = self.snapshot();
        let mut ops = Vec::new();
        // The tail first, so the head's coordinates still hold.
        if start < len {
            ops.extend(self.apply_delete(start, len));
        }
        if end > 0 {
            ops.extend(self.apply_delete(0, end));
        }
        self.changed(before, ops);
    }

    fn apply_delete(&mut self, a: usize, b: usize) -> Vec<Op> {
        let n = b - a;
        let removed: Vec<u8> = Rc::make_mut(&mut self.seq).drain(a..b).collect();
        let map = |x: usize| if x <= a { x } else if x >= b { x - n } else { a };
        let features = Rc::make_mut(&mut self.features);
        let mut kept = Vec::with_capacity(features.len());
        // Surviving features the deletion cut into, by their new index.
        let mut touched = Vec::new();
        // Features the deletion cut into or dropped, as they were, by their old index.
        let mut was = Vec::new();
        for (fi, mut f) in std::mem::take(features).into_iter().enumerate() {
            let hit = f.segments.iter().any(|&(s, e)| a < e && s < b);
            if hit {
                was.push((fi, FeatureRecord::of(&f)));
            }
            for seg in f.segments.iter_mut() {
                *seg = (map(seg.0), map(seg.1));
            }
            f.segments.retain(|(s, e)| s < e);
            if f.segments.is_empty() {
                continue;
            }
            if hit {
                touched.push(kept.len());
            }
            kept.push(f);
        }
        *features = kept;
        self.reencode(&touched);
        with_features(was, Op::Delete { pos: a, bases: String::from_utf8_lossy(&removed).into_owned() })
    }

    /// The feature as the document stores it: coding features carry their frame, table
    /// and translation.
    fn encoded(&self, mut f: Feature) -> Feature {
        genetic_code::encode(&mut f, &self.seq, genetic_code::document_table(&self.features));
        f
    }

    /// Bring the translations of features `ixs` up to date after an edit inside them.
    fn reencode(&mut self, ixs: &[usize]) {
        if ixs.is_empty() {
            return;
        }
        let table = genetic_code::document_table(&self.features);
        let features = Rc::make_mut(&mut self.features);
        for &i in ixs {
            if let Some(f) = features.get_mut(i) {
                genetic_code::encode(f, &self.seq, table);
            }
        }
    }

    /// Add a feature as one undo step.
    pub fn add_feature(&mut self, feature: Feature) {
        if feature.segments.is_empty() {
            return;
        }
        let feature = self.encoded(feature);
        let before = self.snapshot();
        let op = Op::AddFeature { ix: self.features.len(), feature: FeatureRecord::of(&feature) };
        Rc::make_mut(&mut self.features).push(feature);
        self.changed(before, vec![op]);
    }

    /// Replace feature `ix` (Edit Feature) as one undo step.
    pub fn replace_feature(&mut self, ix: usize, feature: Feature) {
        self.edit_features(vec![(ix, feature)], Vec::new());
    }

    /// Replace some features and add others, as one undo step.
    pub fn edit_features(&mut self, replace: Vec<(usize, Feature)>, add: Vec<Feature>) {
        let replace: Vec<_> = replace
            .into_iter()
            .filter(|(ix, f)| *ix < self.features.len() && !f.segments.is_empty())
            .map(|(ix, f)| (ix, self.encoded(f)))
            .collect();
        let add: Vec<_> = add.into_iter().filter(|f| !f.segments.is_empty()).map(|f| self.encoded(f)).collect();
        if replace.is_empty() && add.is_empty() {
            return;
        }
        let before = self.snapshot();
        let mut ops = Vec::new();
        let features = Rc::make_mut(&mut self.features);
        for (ix, f) in replace {
            let (was, now) = (FeatureRecord::of(&features[ix]), FeatureRecord::of(&f));
            if was != now {
                ops.push(Op::EditFeature { ix, before: was, after: now });
            }
            features[ix] = f;
        }
        for f in add {
            ops.push(Op::AddFeature { ix: features.len(), feature: FeatureRecord::of(&f) });
            features.push(f);
        }
        self.changed(before, ops);
    }

    /// Change how structure `id` links its chains. Not an undo step.
    pub fn set_structure_linking(&mut self, id: &str, linking: pgx::ChainLinking) {
        let Some(s) = self.structures.iter_mut().find(|s| s.id == id && s.linking != linking) else { return };
        let op = Op::LinkStructure { id: s.id.clone(), name: s.name.clone(), before: s.linking, after: linking };
        s.linking = linking;
        self.changed_for_good(op);
    }

    /// Remove feature `ix` as one undo step. The bases stay.
    pub fn remove_feature(&mut self, ix: usize) {
        if ix >= self.features.len() {
            return;
        }
        let before = self.snapshot();
        let feature = Rc::make_mut(&mut self.features).remove(ix);
        self.changed(before, vec![Op::RemoveFeature { ix, feature: FeatureRecord::of(&feature) }]);
    }

    /// Make `origin` position 0 of a circular sequence. Features keep the bases they
    /// cover: one that now runs through the origin splits in two, and one that used
    /// to wrap and no longer does joins back up. Logged as moving the bases before
    /// `origin` to the end, so replaying the log still reproduces the sequence.
    pub fn set_origin(&mut self, origin: usize) {
        let len = self.len();
        if !self.circular || origin == 0 || origin >= len {
            return;
        }
        let before = self.snapshot();
        let all = Op::Features { changed: Vec::new(), all: Some(self.features.iter().map(FeatureRecord::of).collect()) };
        let moved: Vec<u8> = self.seq[..origin].to_vec();
        Rc::make_mut(&mut self.seq).rotate_left(origin);
        let shift = |x: usize| (x + len - origin) % len;
        for f in Rc::make_mut(&mut self.features).iter_mut() {
            let mut segs = Vec::with_capacity(f.segments.len() + 1);
            for &(s, e) in &f.segments {
                if s < origin && origin < e {
                    segs.push((shift(s), len));
                    segs.push((0, e - origin));
                } else {
                    // A segment ending exactly at the new origin ends at the new end.
                    let ns = shift(s);
                    segs.push((ns, ns + (e - s)));
                }
            }
            segs.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::with_capacity(segs.len());
            for seg in segs {
                match merged.last_mut() {
                    Some(last) if last.1 == seg.0 => last.1 = seg.1,
                    _ => merged.push(seg),
                }
            }
            f.segments = merged;
        }
        let bases = String::from_utf8_lossy(&moved).into_owned();
        let ops = vec![all, Op::Delete { pos: 0, bases: bases.clone() }, Op::Insert { pos: len - origin, bases }];
        self.changed(before, ops);
    }

    /// Attach a structure. Not an undo step (undo covers the sequence and features),
    /// but it is logged and marks the document changed so it gets saved.
    pub fn add_structure(&mut self, structure: Structure) {
        let op = Op::AttachStructure {
            id: structure.id.clone(),
            name: structure.name.clone(),
            format: structure.format,
            source: structure.source.clone(),
            bytes: structure.data.len(),
        };
        self.structures.push(structure);
        self.changed_for_good(op);
    }

    pub fn remove_structure(&mut self, ix: usize) {
        if ix >= self.structures.len() {
            return;
        }
        let s = self.structures.remove(ix);
        // Features stop naming it. Not an undo step, like the removal itself.
        let mut edits = Vec::new();
        if self.features.iter().any(|f| f.structure_refs().iter().any(|(id, _)| *id == s.id)) {
            for (ix, f) in Rc::make_mut(&mut self.features).iter_mut().enumerate() {
                let was = FeatureRecord::of(f);
                f.set_structure_chains(&s.id, &[]);
                let now = FeatureRecord::of(f);
                if was != now {
                    edits.push(Op::EditFeature { ix, before: was, after: now });
                }
            }
        }
        for op in edits {
            self.log(op);
        }
        self.changed_for_good(Op::RemoveStructure { id: s.id, name: s.name });
    }

    /// The bases and features as they were at revision `rev`, read back from the log
    /// starting at the current state. Exact for edits this build logged; features
    /// changed by base edits in older logs come back as close as the log allows.
    /// Structures aren't covered: the log doesn't hold their files.
    pub fn state_at(&self, rev: u64) -> (Vec<u8>, Vec<Feature>) {
        let mut seq = self.seq.to_vec();
        let mut fs = self.features.to_vec();
        let h = &self.history;
        for i in (0..h.len()).rev() {
            if h[i].rev <= rev {
                break;
            }
            let changed: &[(usize, FeatureRecord)] = match i.checked_sub(1).map(|j| &h[j].op) {
                Some(Op::Features { changed, .. }) => changed,
                _ => &[],
            };
            match &h[i].op {
                Op::Insert { pos, bases } => {
                    if pos + bases.len() <= seq.len() {
                        seq.drain(*pos..pos + bases.len());
                    }
                }
                Op::Delete { pos, bases } => {
                    let pos = (*pos).min(seq.len());
                    seq.splice(pos..pos, bases.bytes());
                }
                _ => {}
            }
            fs = features_before(fs, &h[i].op, changed);
        }
        (seq, fs)
    }

    /// Put the bases and features back as they were at revision `rev`, as one undo
    /// step. Attached structures stay as they are. False if nothing would change.
    pub fn revert_to(&mut self, rev: u64) -> bool {
        if rev >= self.revision {
            return false;
        }
        let (seq, features) = self.state_at(rev);
        let now: Vec<FeatureRecord> = self.features.iter().map(FeatureRecord::of).collect();
        let then: Vec<FeatureRecord> = features.iter().map(FeatureRecord::of).collect();
        if seq == *self.seq && now == then {
            return false;
        }
        let mut ops = vec![Op::Revert { to: rev }];
        if seq != *self.seq {
            // The base edits are logged as the bases that differ; the feature steps
            // below then run from the current features to the old ones.
            ops.push(Op::Features { changed: Vec::new(), all: Some(now.clone()) });
            let cur = &self.seq[..];
            let p = cur.iter().zip(&seq).take_while(|(x, y)| x == y).count();
            let s = cur[p..].iter().rev().zip(seq[p..].iter().rev()).take_while(|(x, y)| x == y).count();
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            if p + s < cur.len() {
                ops.push(Op::Delete { pos: p, bases: text(&cur[p..cur.len() - s]) });
            }
            if p + s < seq.len() {
                ops.push(Op::Insert { pos: p, bases: text(&seq[p..seq.len() - s]) });
            }
        }
        for (ix, (b, a)) in now.iter().zip(&then).enumerate() {
            if b != a {
                ops.push(Op::EditFeature { ix, before: b.clone(), after: a.clone() });
            }
        }
        for ix in (then.len()..now.len()).rev() {
            ops.push(Op::RemoveFeature { ix, feature: now[ix].clone() });
        }
        for ix in now.len()..then.len() {
            ops.push(Op::AddFeature { ix, feature: then[ix].clone() });
        }
        let before = self.snapshot();
        self.seq = Rc::new(seq);
        self.features = Rc::new(features);
        self.changed(before, ops);
        true
    }

    pub fn undo(&mut self) -> bool {
        self.step(true)
    }

    pub fn redo(&mut self) -> bool {
        self.step(false)
    }

    /// Undo and redo restore snapshots, and are logged as edits in their own right so
    /// the history matches what actually happened to the sequence.
    fn step(&mut self, undo: bool) -> bool {
        let popped = if undo { self.undo.pop() } else { self.redo.pop() };
        let Some((snap, ops)) = popped else { return false };
        let current = self.snapshot();
        // The step's own bookkeeping described the state before it, not this one.
        let replay = |op: &&Op| !op.is_bookkeeping() && !(undo && matches!(op, Op::Revert { .. }));
        let logged: Vec<Op> = if undo {
            ops.iter().rev().filter(replay).map(Op::inverse).collect()
        } else {
            ops.iter().filter(replay).cloned().collect()
        };
        // Restoring a snapshot can bring back features the base edits had cut or
        // dropped. If reading the log backwards wouldn't get today's features back,
        // the log keeps them as they are.
        let back = logged.iter().rev().fold(snap.features.to_vec(), |fs, op| features_before(fs, op, &[]));
        if !back.iter().map(FeatureRecord::of).eq(self.features.iter().map(FeatureRecord::of)) {
            self.log(Op::Features { changed: Vec::new(), all: Some(self.features.iter().map(FeatureRecord::of).collect()) });
        }
        for op in logged {
            self.log(op);
        }
        if undo {
            self.redo.push((current, ops));
        } else {
            self.undo.push((current, ops));
        }
        self.seq = snap.seq;
        self.features = snap.features;
        self.version += 1;
        self.dirty = true;
        true
    }

    /// Render the file contents in the document's own format.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        self.serialize_as(self.format)
    }

    /// Render the file contents in any writable format (Save, Export As). Cheap; runs
    /// on the UI thread so the snapshot is consistent.
    pub fn serialize_as(&self, format: Format) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match format {
            Format::SnapGene => bail!("SnapGene .dna files can't be written"),
            Format::Fasta => {
                out.extend_from_slice(format!(">{}\n", self.name).as_bytes());
                for chunk in self.seq.chunks(70) {
                    out.extend_from_slice(chunk);
                    out.push(b'\n');
                }
            }
            Format::GenBank => self.genbank_record().write(&mut out)?,
            Format::Native => {
                let mut saves = self.saves.clone();
                if saves.last().map(|s| s.rev) != Some(self.revision) {
                    saves.push(SavePoint { rev: self.revision, time: now(), author: author(), label: None });
                }
                out = pgx::write(pgx::BundleRef {
                    record: &self.genbank_record(),
                    revision: self.revision,
                    history: &self.history,
                    saves: &saves,
                    structures: &self.structures,
                    extras: &self.extras,
                })?;
            }
        }
        Ok(out)
    }

    fn genbank_record(&self) -> Seq {
        let mut record = self.record.clone().unwrap_or_else(Seq::empty);
        if record.name.is_none() {
            record.name = Some(locus_name(&self.name));
        }
        if record.molecule_type.is_none() {
            record.molecule_type = Some("DNA".into());
        }
        if self.record.is_none() {
            record.topology = if self.circular { Topology::Circular } else { Topology::Linear };
        }
        record.seq = self.seq.to_vec();
        record.len = Some(record.seq.len());
        record.features.retain(|f| f.kind == "source");
        for f in record.features.iter_mut() {
            f.location = Location::simple_range(0, self.seq.len() as i64);
        }
        record.features.extend(self.features.iter().map(to_gb_feature));
        record
    }

    /// Clear the dirty flag, unless the document was edited after `version` was written.
    /// A native save also records a save point at the revision it wrote.
    pub fn mark_saved(&mut self, version: u64) {
        if self.version == version {
            self.dirty = false;
            if self.format == Format::Native && self.saves.last().map(|s| s.rev) != Some(self.revision) {
                self.saves.push(SavePoint { rev: self.revision, time: now(), author: author(), label: None });
            }
        }
    }

    /// Synchronous save (used by tests); the editor writes on a background thread instead.
    #[cfg(test)]
    pub fn save(&mut self) -> Result<()> {
        write_atomic(&self.path, &self.serialize()?)?;
        self.mark_saved(self.version);
        Ok(())
    }
}

/// Write to a temp file beside `path`, then rename over it, so a crash or a full
/// disk mid-write never leaves a truncated sequence file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.saving"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// GenBank LOCUS names cannot contain spaces.
fn locus_name(name: &str) -> String {
    let n: String = name.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
    if n.is_empty() { "untitled".into() } else { n }
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The name a feature is drawn with: the first of its naming qualifiers, else its type.
pub fn feature_label(f: &GbFeature) -> String {
    ["label", "gene", "product", "note", "locus_tag"]
        .iter()
        .find_map(|k| f.qualifier_values(k).next())
        .map(|s| s.to_string())
        .unwrap_or_else(|| f.kind.to_string())
}


/// Interchange formats write bases in lower case by convention (GenBank always does),
/// so a sequence with no capitals says nothing about case and is shown in capitals.
/// Mixed case is kept: lower case often marks a region.
fn conventional_case(mut seq: Vec<u8>) -> Vec<u8> {
    if !seq.iter().any(u8::is_ascii_uppercase) {
        seq.make_ascii_uppercase();
    }
    seq
}
fn convert_feature(f: &GbFeature, len: usize) -> Option<Feature> {
    let (mut segments, strand) = location_segments(&f.location);
    segments.retain(|(s, e)| s < e && *e <= len);
    if segments.is_empty() {
        return None;
    }
    segments.sort_unstable();
    let label = feature_label(f);
    Some(Feature { kind: f.kind.to_string(), label, segments, strand, source: Some(f.clone()) })
}

/// A GenBank location as half-open segments in file order, plus its strand.
/// Fuzzy ends are dropped; locations without ranges (gaps, between-base sites,
/// references to other records) yield no segments.
pub fn location_segments(loc: &Location) -> (Vec<(usize, usize)>, i8) {
    let mut segments = Vec::new();
    let mut strand = 1i8;
    collect_segments(loc, &mut segments, &mut strand, false);
    (segments, strand)
}

/// The inverse of [`location_segments`]: one range, or a join, complemented for -1.
pub fn segments_location(segments: &[(usize, usize)], strand: i8) -> Option<Location> {
    let mut ranges: Vec<Location> = segments.iter().map(|&(s, e)| Location::simple_range(s as i64, e as i64)).collect();
    let location = match ranges.len() {
        0 => return None,
        1 => ranges.pop().unwrap(),
        _ => Location::Join(ranges),
    };
    Some(if strand < 0 { Location::Complement(Box::new(location)) } else { location })
}

fn collect_segments(loc: &Location, out: &mut Vec<(usize, usize)>, strand: &mut i8, complement: bool) {
    match loc {
        Location::Range((a, _), (b, _)) => {
            out.push((*a.max(&0) as usize, *b.max(&0) as usize));
            *strand = if complement { -1 } else { 1 };
        }
        Location::Complement(inner) => collect_segments(inner, out, strand, !complement),
        Location::Join(parts) | Location::Order(parts) | Location::Bond(parts) | Location::OneOf(parts) => {
            for p in parts {
                collect_segments(p, out, strand, complement);
            }
        }
        _ => {}
    }
}

/// GenBank files usually carry a `gene` and a `CDS` over the same span; drawing both
/// doubles the annotation lanes for no information.
/// A base edit, preceded by the features it changed beyond moving them, if any.
fn with_features(changed: Vec<(usize, FeatureRecord)>, op: Op) -> Vec<Op> {
    if changed.is_empty() {
        vec![op]
    } else {
        vec![Op::Features { changed, all: None }, op]
    }
}

/// The features before logged step `op`, given the ones after it. `changed` is the
/// [`Op::Features`] record logged just before it, if any.
fn features_before(mut fs: Vec<Feature>, op: &Op, changed: &[(usize, FeatureRecord)]) -> Vec<Feature> {
    match op {
        Op::Insert { pos, bases } => return uninsert(fs, *pos, bases.len(), changed),
        Op::Delete { pos, bases } => return undelete(fs, *pos, bases.len(), changed),
        Op::AddFeature { ix, .. } if *ix < fs.len() => {
            fs.remove(*ix);
        }
        Op::RemoveFeature { ix, feature } => fs.insert((*ix).min(fs.len()), feature.to_feature()),
        Op::EditFeature { ix, before, .. } if *ix < fs.len() => fs[*ix] = before.to_feature(),
        Op::Features { all: Some(all), .. } => return all.iter().map(FeatureRecord::to_feature).collect(),
        _ => {}
    }
    fs
}

/// The features before inserting `n` bases at `pos`, given the ones after: features
/// past it move back, and `changed` restores the ones the insert changed.
fn uninsert(mut fs: Vec<Feature>, pos: usize, n: usize, changed: &[(usize, FeatureRecord)]) -> Vec<Feature> {
    let back = |x: usize| if x >= pos + n { x - n } else { x.min(pos) };
    for f in fs.iter_mut() {
        for seg in f.segments.iter_mut() {
            *seg = (back(seg.0), back(seg.1));
        }
        f.segments.retain(|(s, e)| s < e);
    }
    fs.retain(|f| !f.segments.is_empty());
    for (ix, rec) in changed {
        if let Some(f) = fs.get_mut(*ix) {
            *f = rec.to_feature();
        }
    }
    fs
}

/// The features before deleting `n` bases at `pos`, given the ones after. `changed`
/// holds, by their old index, every feature the delete cut into or dropped; the rest
/// only moved.
fn undelete(fs: Vec<Feature>, pos: usize, n: usize, changed: &[(usize, FeatureRecord)]) -> Vec<Feature> {
    let end = pos + n;
    let mut after = fs.into_iter().peekable();
    let mut changed = changed.iter().peekable();
    let mut out = Vec::new();
    loop {
        if let Some((_, rec)) = changed.next_if(|(ix, _)| *ix <= out.len()) {
            // A feature the delete cut into survives as the next one after it.
            let dropped = rec.segments.iter().all(|g| pos <= g[0] && g[1] <= end);
            if !dropped {
                after.next();
            }
            out.push(rec.to_feature());
        } else if let Some(mut f) = after.next() {
            for seg in f.segments.iter_mut() {
                if seg.0 >= pos {
                    *seg = (seg.0 + n, seg.1 + n);
                }
            }
            out.push(f);
        } else if let Some((_, rec)) = changed.next() {
            out.push(rec.to_feature());
        } else {
            return out;
        }
    }
}

fn hide_redundant_genes(features: &mut Vec<Feature>) {
    let cds: std::collections::HashSet<Vec<(usize, usize)>> =
        features.iter().filter(|f| f.kind == "CDS").map(|f| f.segments.clone()).collect();
    features.retain(|f| f.kind != "gene" || !cds.contains(&f.segments));
}

fn to_gb_feature(f: &Feature) -> GbFeature {
    // Documents never hold features without segments (edits drop emptied ones).
    let location = segments_location(&f.segments, f.strand).unwrap_or_else(|| Location::simple_range(0, 0));
    match &f.source {
        Some(src) => GbFeature { location, ..src.clone() },
        None => GbFeature {
            kind: f.kind.clone().into(),
            location,
            qualifiers: vec![("label".into(), Some(f.label.clone()))],
        },
    }
}

/// The complementary IUPAC code, in the same case.
pub fn complement(b: u8) -> u8 {
    let c = match b.to_ascii_uppercase() {
        b'A' => b'T',
        b'T' => b'A',
        b'G' => b'C',
        b'C' => b'G',
        b'U' => b'A',
        b'R' => b'Y',
        b'Y' => b'R',
        b'K' => b'M',
        b'M' => b'K',
        b'B' => b'V',
        b'V' => b'B',
        b'D' => b'H',
        b'H' => b'D',
        _ => return b,
    };
    if b.is_ascii_lowercase() { c.to_ascii_lowercase() } else { c }
}

pub fn reverse_complement(seq: &[u8]) -> Vec<u8> {
    seq.iter().rev().map(|&b| complement(b)).collect()
}

pub fn gc_percent(seq: &[u8]) -> f32 {
    if seq.is_empty() {
        return 0.0;
    }
    let gc = seq.iter().filter(|b| matches!(b.to_ascii_uppercase(), b'G' | b'C' | b'S')).count();
    gc as f32 * 100.0 / seq.len() as f32
}

/// Keep only characters that are valid IUPAC nucleotide codes, in the case they came in.
pub fn sanitize_bases(text: &str) -> Vec<u8> {
    text.bytes().filter(|b| b"ACGTURYKMSWBDHVN".contains(&b.to_ascii_uppercase())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(seq: &str, feats: Vec<(usize, usize)>) -> Document {
        let mut d = Document::blank(Path::new("/tmp/x.fa"), Format::Fasta);
        d.seq = Rc::new(seq.as_bytes().to_vec());
        d.features = Rc::new(
            feats
                .into_iter()
                .map(|(s, e)| Feature { kind: "CDS".into(), label: "f".into(), segments: vec![(s, e)], strand: 1, source: None })
                .collect(),
        );
        d
    }

    #[test]
    fn lower_case_bases_are_kept() {
        assert_eq!(complement(b'a'), b't');
        assert_eq!(complement(b'G'), b'C');
        assert_eq!(complement(b'r'), b'y');
        assert_eq!(complement(b'n'), b'n');
        assert_eq!(reverse_complement(b"AAcg"), b"cgTT");
        assert_eq!(sanitize_bases("ac gT-x\nN"), b"acgTN");
        assert_eq!(gc_percent(b"gcAT"), 50.0);

        let dir = std::env::temp_dir().join("plasmid-gpui-case");
        std::fs::create_dir_all(&dir).unwrap();
        // Mixed case survives FASTA; an all-lower-case file is just convention.
        let mixed = dir.join("mixed.fa");
        std::fs::write(&mixed, ">m\nACGTacgtACGT\n").unwrap();
        assert_eq!(&Document::open(&mixed).unwrap().seq[..], b"ACGTacgtACGT");
        let lower = dir.join("lower.fa");
        std::fs::write(&lower, ">l\nacgt\n").unwrap();
        assert_eq!(&Document::open(&lower).unwrap().seq[..], b"ACGT");

        // A .pgx keeps exactly what was typed, even all lower case, and its log.
        let path = dir.join("typed.pgx");
        let mut d = Document::new_untitled(&path);
        d.set_path(&path).unwrap();
        d.insert(0, b"acgt");
        d.save().unwrap();
        let back = Document::open(&path).unwrap();
        assert_eq!(&back.seq[..], b"acgt");
        assert_eq!(back.history, d.history);
    }

    #[test]
    fn every_saved_change_is_logged_and_survives_a_round_trip() {
        set_author(Some("ada".into()));
        let dir = std::env::temp_dir().join("plasmid-gpui-log");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.pgx");
        let mut d = Document::new_untitled(&path);
        d.set_path(&path).unwrap();
        d.insert(0, b"ACGTACGTACGT");
        let tag = Feature { kind: "misc_feature".into(), label: "tag".into(), segments: vec![(2, 6)], strand: 1, source: None };
        d.add_feature(tag);
        let mut f = d.features[0].clone();
        f.label = "renamed".into();
        d.replace_feature(0, f);
        d.remove_feature(0);
        d.undo();
        d.save().unwrap();
        set_author(None);

        let ops: Vec<&str> = d
            .history
            .iter()
            .map(|e| match e.op {
                Op::Insert { .. } => "insert",
                Op::AddFeature { .. } => "add",
                Op::EditFeature { .. } => "edit",
                Op::RemoveFeature { .. } => "remove",
                _ => "other",
            })
            .collect();
        assert_eq!(ops, ["insert", "add", "edit", "remove", "add"]);
        assert!(d.history.iter().all(|e| e.author.as_deref() == Some("ada")));
        assert_eq!(d.saves.last().unwrap().author.as_deref(), Some("ada"));
        let Op::EditFeature { before, after, .. } = &d.history[2].op else { unreachable!() };
        assert_eq!((before.label.as_str(), after.label.as_str()), ("tag", "renamed"));

        let back = Document::open(&path).unwrap();
        assert_eq!(back.history, d.history);
        assert_eq!(back.saves, d.saves);
        assert_eq!(back.features[0].label, "renamed");

        // Save As under a new name logs the rename.
        d.set_path(&dir.join("log2.pgx")).unwrap();
        assert_eq!(d.history.last().unwrap().op, Op::Rename { before: "log".into(), after: "log2".into() });
    }

    fn state(seq: &[u8], fs: &[Feature]) -> (Vec<u8>, Vec<FeatureRecord>) {
        (seq.to_vec(), fs.iter().map(FeatureRecord::of).collect())
    }

    #[test]
    fn every_revision_can_be_read_back_and_reverted_to() {
        let mut d = doc("ATGAAACCCGGGTTTTAAATGCCCTAG", vec![(0, 18), (3, 9), (18, 27), (12, 15)]);
        d.circular = true;
        let mut seen = vec![(d.revision, state(&d.seq, &d.features))];
        let mut step = |d: &mut Document, f: &dyn Fn(&mut Document)| {
            f(d);
            seen.push((d.revision, state(&d.seq, &d.features)));
        };
        step(&mut d, &|d| d.insert(6, b"GGG")); // inside two coding features: new translations
        step(&mut d, &|d| d.delete(3, 12)); // drops one feature, cuts another
        step(&mut d, &|d| d.delete(14, 17)); // drops one between survivors
        step(&mut d, &|d| d.add_feature(Feature { kind: "misc_feature".into(), label: "tag".into(), segments: vec![(1, 4)], strand: -1, source: None }));
        step(&mut d, &|d| d.set_origin(5)); // rewrites every feature, splits one
        step(&mut d, &|d| {
            d.undo();
        });
        step(&mut d, &|d| {
            d.redo();
        });
        step(&mut d, &|d| d.delete_across_origin(d.len() - 2, 2));
        step(&mut d, &|d| {
            d.undo();
        });
        step(&mut d, &|d| d.remove_feature(0));

        for (rev, want) in &seen {
            let (seq, fs) = d.state_at(*rev);
            assert_eq!(&state(&seq, &fs), want, "revision {rev}");
        }

        // Revert to the start, then to a middle revision: each is one undo step, and
        // the log still reads back exactly through the reverts.
        let (start, mid) = (seen[0].clone(), seen[4].clone());
        assert!(d.revert_to(start.0));
        assert_eq!(state(&d.seq, &d.features), start.1);
        assert!(!d.revert_to(start.0), "already there");
        let after_first = (d.revision, state(&d.seq, &d.features));
        assert!(d.revert_to(mid.0));
        assert_eq!(state(&d.seq, &d.features), mid.1);
        assert!(matches!(d.history.iter().rev().find(|e| matches!(e.op, Op::Revert { .. })).unwrap().op, Op::Revert { to } if to == mid.0));
        d.undo();
        assert_eq!(state(&d.seq, &d.features), after_first.1);
        for (rev, want) in seen.iter().chain([&after_first]) {
            let (seq, fs) = d.state_at(*rev);
            assert_eq!(&state(&seq, &fs), want, "revision {rev} after reverting");
        }

        // The log as saved reads back the same way.
        let dir = std::env::temp_dir().join("plasmid-gpui-revert");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("revert.pgx");
        d.set_path(&path).unwrap();
        d.save().unwrap();
        let back = Document::open(&path).unwrap();
        for (rev, want) in &seen {
            let (seq, fs) = back.state_at(*rev);
            assert_eq!(&state(&seq, &fs), want, "revision {rev} after reopening");
        }
    }

    #[test]
    fn insert_shifts_and_grows_features() {
        let mut d = doc("AAAACCCCGGGG", vec![(4, 8), (8, 12), (0, 2)]);
        d.insert(6, b"TT");
        assert_eq!(&d.seq[..], b"AAAACCTTCCGGGG");
        assert_eq!(d.features[0].segments, vec![(4, 10)]);
        assert_eq!(d.features[1].segments, vec![(10, 14)]);
        assert_eq!(d.features[2].segments, vec![(0, 2)]);
    }

    #[test]
    fn delete_shrinks_and_drops_features() {
        let mut d = doc("AAAACCCCGGGG", vec![(4, 8), (8, 12), (5, 7)]);
        d.delete(4, 7);
        assert_eq!(&d.seq[..], b"AAAACGGGG");
        assert_eq!(d.features.len(), 2);
        assert_eq!(d.features[0].segments, vec![(4, 5)]);
        assert_eq!(d.features[1].segments, vec![(5, 9)]);
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut d = doc("ACGT", vec![]);
        d.insert(4, b"AA");
        d.delete(0, 1);
        assert!(d.undo());
        assert_eq!(&d.seq[..], b"ACGTAA");
        assert!(d.undo());
        assert_eq!(&d.seq[..], b"ACGT");
        assert!(!d.undo());
        assert!(d.redo());
        assert_eq!(&d.seq[..], b"ACGTAA");
    }

    #[test]
    fn set_origin_rotates_sequence_and_features() {
        // Features: before the new origin, across it, after it, and one that wrapped.
        let mut d = doc("AACCGGTT", vec![(0, 2), (3, 6), (6, 8), (0, 1)]);
        Rc::make_mut(&mut d.features)[3].segments = vec![(0, 1), (7, 8)];
        d.circular = true;
        d.set_origin(4);
        assert_eq!(&d.seq[..], b"GGTTAACC");
        assert_eq!(d.features[0].segments, vec![(4, 6)]);
        assert_eq!(d.features[1].segments, vec![(0, 2), (7, 8)]);
        assert_eq!(d.features[2].segments, vec![(2, 4)]);
        assert_eq!(d.features[3].segments, vec![(3, 5)]);
        // Replaying the logged ops on the old sequence gives the new one.
        let mut seq = b"AACCGGTT".to_vec();
        for e in &d.history {
            match &e.op {
                Op::Delete { pos, bases } => drop(seq.drain(*pos..*pos + bases.len())),
                Op::Insert { pos, bases } => drop(seq.splice(*pos..*pos, bases.bytes())),
                _ => {}
            }
        }
        assert_eq!(&seq[..], &d.seq[..]);
        assert!(d.undo());
        assert_eq!(&d.seq[..], b"AACCGGTT");
        assert_eq!(d.features[1].segments, vec![(3, 6)]);
    }

    #[test]
    fn set_origin_ignores_linear_sequences() {
        let mut d = doc("AACCGGTT", vec![]);
        d.set_origin(4);
        assert_eq!(&d.seq[..], b"AACCGGTT");
        assert!(!d.undo());
    }

    #[test]
    fn delete_across_origin_is_one_undo_step() {
        let mut d = doc("AACCGGTT", vec![(6, 8), (0, 2), (2, 6)]);
        d.delete_across_origin(6, 2);
        assert_eq!(&d.seq[..], b"CCGG");
        assert_eq!(d.features.len(), 1);
        assert_eq!(d.features[0].segments, vec![(0, 4)]);
        let changes = |d: &Document| d.history.iter().filter(|e| !e.op.is_bookkeeping()).count();
        assert_eq!(changes(&d), 2);
        assert!(d.undo());
        assert_eq!(&d.seq[..], b"AACCGGTT");
        assert_eq!(d.features.len(), 3);
        assert_eq!(changes(&d), 4);
        assert!(!d.undo());
        assert!(d.redo());
        assert_eq!(&d.seq[..], b"CCGG");
    }

    #[test]
    fn loads_genbank_samples() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let puc = Document::open(&dir.join("pUC19.gb")).unwrap();
        assert_eq!(puc.len(), 2686);
        assert!(puc.circular);
        let pbr = Document::open(&dir.join("pBR322.gb")).unwrap();
        assert_eq!(pbr.len(), 4361);
        assert!(pbr.features.len() > 10);
        assert!(pbr.features.iter().any(|f| f.strand < 0));
    }

    #[test]
    fn genbank_save_round_trips() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/pBR322.gb");
        let tmp = std::env::temp_dir().join("plasmid-gpui-roundtrip.gb");
        std::fs::copy(&src, &tmp).unwrap();
        let mut d = Document::open(&tmp).unwrap();
        d.insert(10, b"GGG");
        d.save().unwrap();
        let again = Document::open(&tmp).unwrap();
        assert_eq!(again.len(), 4364);
        assert_eq!(again.features.len(), d.features.len());
        assert_eq!(again.features.iter().map(|f| &f.segments).collect::<Vec<_>>(), d.features.iter().map(|f| &f.segments).collect::<Vec<_>>());
    }

    #[test]
    fn untitled_save_as_writes_requested_format() {
        let dir = std::env::temp_dir().join("plasmid-gpui-untitled");
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = Document::new_untitled(&dir.join("untitled-1.gb"));
        assert!(d.untitled);
        d.insert(0, b"ACGTACGT");
        let target = dir.join("my construct.gb");
        d.set_path(&target).unwrap();
        d.save().unwrap();
        assert!(!d.untitled && !d.dirty);
        let again = Document::open(&target).unwrap();
        assert_eq!(&again.seq[..], b"ACGTACGT");
        assert_eq!(again.format, Format::GenBank);

        let fasta = dir.join("copy.fa");
        d.set_path(&fasta).unwrap();
        d.save().unwrap();
        assert_eq!(Document::open(&fasta).unwrap().format, Format::Fasta);
    }

    #[test]
    fn untitled_defaults_to_native() {
        let d = Document::new_untitled(Path::new("/tmp/untitled-1.pgx"));
        assert_eq!(d.format, Format::Native);
        let mut d = Document::new_untitled(Path::new("/tmp/untitled-1.gb"));
        assert_eq!(d.format, Format::GenBank);
        assert!(d.set_path(Path::new("/tmp/x.dna")).is_err(), "SnapGene is read-only");
        assert_eq!(d.format, Format::GenBank);
    }

    #[test]
    fn edits_undo_and_redo_are_logged_with_their_bases() {
        let mut d = doc("ACGT", vec![]);
        d.insert(2, b"TT");
        d.delete(0, 1);
        d.undo();
        d.redo();
        assert_eq!(d.revision, 4);
        let ops: Vec<_> = d.history.iter().map(|e| e.op.clone()).collect();
        assert_eq!(
            ops,
            vec![
                Op::Insert { pos: 2, bases: "TT".into() },
                Op::Delete { pos: 0, bases: "A".into() },
                Op::Insert { pos: 0, bases: "A".into() },
                Op::Delete { pos: 0, bases: "A".into() },
            ]
        );
        // Replaying the log backwards from the current sequence recovers the original.
        let mut seq = d.seq.to_vec();
        for e in d.history.iter().rev() {
            match &e.op {
                Op::Insert { pos, bases } => drop(seq.drain(*pos..pos + bases.len())),
                Op::Delete { pos, bases } => drop(seq.splice(*pos..*pos, bases.bytes())),
                _ => {}
            }
        }
        assert_eq!(seq, b"ACGT");
    }

    #[test]
    fn genbank_converts_to_native_and_keeps_history_across_saves() {
        let dir = std::env::temp_dir().join("plasmid-gpui-native");
        std::fs::create_dir_all(&dir).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/pBR322.gb");
        let mut d = Document::open(&src).unwrap();
        d.insert(10, b"GGG");
        let target = dir.join("pBR322.pgx");
        d.set_path(&target).unwrap();
        assert_eq!(d.format, Format::Native);
        d.save().unwrap();
        assert_eq!(d.saves.len(), 1);

        let mut again = Document::open(&target).unwrap();
        assert_eq!(again.format, Format::Native);
        assert!(again.warnings.is_empty());
        assert_eq!(&again.seq[..], &d.seq[..]);
        assert_eq!(again.circular, d.circular);
        let summary = |d: &Document| d.features.iter().map(|f| (f.label.clone(), f.segments.clone(), f.strand)).collect::<Vec<_>>();
        assert_eq!(summary(&again), summary(&d));
        assert_eq!(again.history, d.history);
        assert_eq!(again.revision, 1);

        again.delete(0, 5);
        again.save().unwrap();
        let third = Document::open(&target).unwrap();
        assert_eq!(third.revision, 2);
        assert_eq!(third.history.len(), 2);
        assert_eq!(third.saves.iter().map(|s| s.rev).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(third.len(), 4361 + 3 - 5);
    }

    #[test]
    fn export_writes_other_formats_without_switching() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/pUC19.gb");
        let d = Document::open(&src).unwrap();
        let fasta = d.serialize_as(Format::Fasta).unwrap();
        assert!(fasta.starts_with(b">pUC19\n"));
        assert!(pgx::looks_like_zip(&d.serialize_as(Format::Native).unwrap()));
        assert!(d.serialize_as(Format::SnapGene).is_err());
        assert_eq!(d.format, Format::GenBank);
        assert_eq!(d.path, src);
    }

    #[test]
    fn opens_snapgene_as_read_only() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/sample-d.dna");
        if !path.exists() {
            return;
        }
        let d = Document::open(&path).unwrap();
        assert_eq!(d.format, Format::SnapGene);
        assert!(!d.format.writable());
        assert_eq!(d.len(), 1000);
        assert_eq!(d.features.len(), 4);
    }
}

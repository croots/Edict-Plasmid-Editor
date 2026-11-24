//! The native file format: a zip archive (like ODF or EPUB) holding the sequence plus
//! the things interchange formats can't, such as the edit history.
//!
//! ```text
//! mimetype               first entry, stored uncompressed: MIME_TYPE
//! manifest.json          format name + version, app version, current revision
//! sequence.json          source of truth: sequence, topology, features, record metadata
//! sequence.gb            GenBank copy for other tools; regenerated on save, never read
//! history/ops.jsonl      one edit per line, oldest first: {rev, time, op, pos, bases}
//! history/revisions.json save points: [{rev, time, label?}]
//! structures/index.json  attached 3D structures: [{id, name, file, format, source?}]
//! structures/<id>.pdb    each structure's file, byte-for-byte as attached (.pdb or .cif)
//! alignments/…           reserved; see below
//! ```
//!
//! Compatibility rules:
//! - `format_version` is bumped only for changes old readers would misread. A reader
//!   refuses files with a newer version rather than guess.
//! - Entries a reader doesn't know are kept byte-for-byte and written back on save, so
//!   an older build never strips data a newer one added.
//! - Every edit stores the bases it inserted or removed, so the log can be replayed
//!   backwards from `sequence.json` to any earlier revision without checkpoints.
//! - Anything tied to coordinates (alignments, when they arrive) records the `revision`
//!   it was made against; if that differs from the manifest's, it is stale.
//! - Structures are stored without any tie to coordinates: which CDS a chain belongs
//!   to is worked out again from the sequences whenever the file is opened.
//!
//! Reading never touches the disk beyond the archive itself, and every entry is size-capped.

use std::io::{Cursor, Read, Write};
use std::rc::Rc;

use anyhow::{bail, Context as _, Result};
use gb_io::seq::{Date, Feature as GbFeature, Reference, Seq, Source, Topology};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::model::{self, Edit};

pub const EXTENSION: &str = "pgx";
pub const MIME_TYPE: &str = "application/vnd.plasmid-gpui+zip";
pub const FORMAT_NAME: &str = "plasmid-gpui";
pub const FORMAT_VERSION: u32 = 1;

const MAX_ENTRY_BYTES: u64 = 256 << 20;
const MAX_TOTAL_BYTES: u64 = 1 << 30;
const MAX_ENTRIES: usize = 10_000;

pub fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// How a structure's coordinates are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StructureFormat {
    Pdb,
    Mmcif,
}

impl StructureFormat {
    pub fn extension(self) -> &'static str {
        match self {
            StructureFormat::Pdb => "pdb",
            StructureFormat::Mmcif => "cif",
        }
    }
}

/// How a structure's chains are tied to the document's features.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChainLinking {
    /// Each protein chain links to whichever CDS it aligns to best. Files from before
    /// links were chosen by hand load this way.
    #[default]
    Auto,
    /// Chains link to the features that name them in a `/structure` qualifier.
    Features,
    /// Not linked to the sequence at all.
    None,
}

impl ChainLinking {
    fn is_auto(&self) -> bool {
        *self == ChainLinking::Auto
    }
}

/// The feature qualifier that ties a feature to a structure chain: `/structure="<id>:<chain>"`.
pub const STRUCTURE_QUALIFIER: &str = "structure";

/// A 3D structure attached to the document, kept exactly as it was read.
#[derive(Clone, Debug, PartialEq)]
pub struct Structure {
    /// Unique within the document; also names its file in the archive.
    pub id: String,
    pub name: String,
    pub format: StructureFormat,
    /// Where it came from, e.g. `PDB 2Y0G` or the original file name.
    pub source: Option<String>,
    pub data: Rc<[u8]>,
    pub linking: ChainLinking,
}

impl Structure {
    fn file_name(&self) -> String {
        format!("structures/{}.{}", self.id, self.format.extension())
    }
}

#[derive(Serialize, Deserialize)]
struct StructureEntry {
    id: String,
    name: String,
    file: String,
    format: StructureFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "ChainLinking::is_auto")]
    linking: ChainLinking,
}

const STRUCTURE_INDEX: &str = "structures/index.json";

/// Ids are used in file names, so keep them to lower-case letters, digits, `-` and `_`.
pub fn structure_id(stem: &str, taken: &[Structure]) -> String {
    let mut base: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '_' })
        .take(40)
        .collect();
    if base.is_empty() {
        base = "structure".into();
    }
    let mut id = base.clone();
    let mut n = 2;
    while taken.iter().any(|s| s.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavePoint {
    pub rev: u64,
    pub time: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Everything a `.pgx` holds. `record` carries the sequence, features and metadata.
pub struct Bundle {
    pub record: Seq,
    pub revision: u64,
    pub history: Vec<Edit>,
    pub saves: Vec<SavePoint>,
    pub structures: Vec<Structure>,
    /// Entries this build doesn't understand, kept verbatim for the next save.
    pub extras: Vec<(String, Vec<u8>)>,
    /// Problems that didn't stop the file opening (a damaged history line, say).
    pub warnings: Vec<String>,
}

pub struct BundleRef<'a> {
    pub record: &'a Seq,
    pub revision: u64,
    pub history: &'a [Edit],
    pub saves: &'a [SavePoint],
    pub structures: &'a [Structure],
    pub extras: &'a [(String, Vec<u8>)],
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    format_version: u32,
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    revision: u64,
}

#[derive(Serialize, Deserialize)]
struct SequenceFile {
    name: String,
    topology: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    molecule_type: Option<String>,
    sequence: String,
    #[serde(default)]
    features: Vec<FeatureJson>,
    #[serde(default)]
    record: RecordMeta,
}

#[derive(Serialize, Deserialize)]
struct FeatureJson {
    #[serde(rename = "type")]
    kind: String,
    /// +1 forward, -1 reverse, 0 none.
    #[serde(default)]
    strand: i8,
    /// Half-open, 0-based `[start, end)` pairs in order.
    segments: Vec<[usize; 2]>,
    /// GenBank qualifiers in file order; `null` for valueless ones like `/pseudo`.
    #[serde(default)]
    qualifiers: Vec<(String, Option<String>)>,
}

#[derive(Default, Serialize, Deserialize)]
struct RecordMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    definition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accession: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keywords: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dblink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    division: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<SourceJson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    references: Vec<ReferenceJson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct SourceJson {
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    organism: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ReferenceJson {
    description: String,
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authors: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consortium: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    journal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pubmed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remark: Option<String>,
}

const KNOWN: &[&str] =
    &["mimetype", "manifest.json", "sequence.json", "sequence.gb", "history/ops.jsonl", "history/revisions.json"];

pub fn write(b: BundleRef) -> Result<Vec<u8>> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // Uncompressed and first, so `file` and other sniffers can identify the archive.
    zip.start_file("mimetype", stored)?;
    zip.write_all(MIME_TYPE.as_bytes())?;

    let manifest = Manifest {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        revision: b.revision,
    };
    zip.start_file("manifest.json", deflated)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;

    zip.start_file("sequence.json", deflated)?;
    zip.write_all(&serde_json::to_vec_pretty(&to_json(b.record))?)?;

    let mut gb = Vec::new();
    b.record.write(&mut gb)?;
    zip.start_file("sequence.gb", deflated)?;
    zip.write_all(&gb)?;

    zip.start_file("history/ops.jsonl", deflated)?;
    for e in b.history {
        serde_json::to_writer(&mut zip, e)?;
        zip.write_all(b"\n")?;
    }
    zip.start_file("history/revisions.json", deflated)?;
    zip.write_all(&serde_json::to_vec_pretty(b.saves)?)?;

    let mut written: Vec<String> = KNOWN.iter().map(|s| s.to_string()).collect();
    if !b.structures.is_empty() {
        let index: Vec<StructureEntry> = b
            .structures
            .iter()
            .map(|s| StructureEntry {
                id: s.id.clone(),
                name: s.name.clone(),
                file: s.file_name(),
                format: s.format,
                source: s.source.clone(),
                linking: s.linking,
            })
            .collect();
        zip.start_file(STRUCTURE_INDEX, deflated)?;
        zip.write_all(&serde_json::to_vec_pretty(&index)?)?;
        written.push(STRUCTURE_INDEX.into());
        for s in b.structures {
            let name = s.file_name();
            if written.contains(&name) {
                bail!("two structures share the file name {name}");
            }
            zip.start_file(name.as_str(), deflated)?;
            zip.write_all(&s.data)?;
            written.push(name);
        }
    }

    // An extra can't shadow an entry this build wrote (say, an index that failed to
    // parse on open and was kept, after a structure has since been attached).
    for (name, bytes) in b.extras.iter().filter(|(n, _)| !written.contains(n)) {
        zip.start_file(name.as_str(), deflated)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

pub fn read(bytes: &[u8]) -> Result<Bundle> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).context("not a valid zip archive")?;
    if zip.len() > MAX_ENTRIES {
        bail!("archive has {} entries (limit {MAX_ENTRIES})", zip.len());
    }
    let mut total = 0u64;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f.name()?.to_string();
        // Never trust the declared size: read through a cap.
        let mut buf = Vec::new();
        (&mut f).take(MAX_ENTRY_BYTES + 1).read_to_end(&mut buf).with_context(|| format!("reading {name}"))?;
        if buf.len() as u64 > MAX_ENTRY_BYTES {
            bail!("{name} is larger than {} MB", MAX_ENTRY_BYTES >> 20);
        }
        total += buf.len() as u64;
        if total > MAX_TOTAL_BYTES {
            bail!("archive expands to more than {} MB", MAX_TOTAL_BYTES >> 20);
        }
        entries.push((name, buf));
    }
    let take = |name: &str| entries.iter().find(|(n, _)| n == name).map(|(_, b)| b.as_slice());

    let manifest: Manifest =
        serde_json::from_slice(take("manifest.json").context("missing manifest.json")?).context("reading manifest.json")?;
    if manifest.format != FORMAT_NAME {
        bail!("not a {FORMAT_NAME} file (format \"{}\")", manifest.format);
    }
    if manifest.format_version > FORMAT_VERSION {
        bail!(
            "this file uses format version {}, newer than this build understands ({FORMAT_VERSION}); update the app to open it",
            manifest.format_version
        );
    }
    let seq: SequenceFile =
        serde_json::from_slice(take("sequence.json").context("missing sequence.json")?).context("reading sequence.json")?;
    let record = from_json(seq);

    let mut warnings = Vec::new();
    let mut history = Vec::new();
    if let Some(ops) = take("history/ops.jsonl") {
        let text = String::from_utf8_lossy(ops);
        let mut bad = 0;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Edit>(line) {
                Ok(e) => history.push(e),
                Err(_) => bad += 1,
            }
        }
        if bad > 0 {
            warnings.push(format!("{bad} unreadable edit-history entr{} skipped", if bad == 1 { "y was" } else { "ies were" }));
        }
    }
    let saves = match take("history/revisions.json").map(serde_json::from_slice::<Vec<SavePoint>>) {
        Some(Ok(s)) => s,
        Some(Err(_)) => {
            warnings.push("the list of save points was unreadable and was reset".into());
            Vec::new()
        }
        None => Vec::new(),
    };
    let revision = manifest.revision.max(history.last().map_or(0, |e| e.rev));

    // Structures: the index names each file. Files it doesn't name are kept as extras,
    // and so is an index that can't be read, so nothing is lost on the next save.
    let mut structures: Vec<Structure> = Vec::new();
    let mut claimed: Vec<String> = Vec::new();
    if let Some(index) = take(STRUCTURE_INDEX) {
        match serde_json::from_slice::<Vec<StructureEntry>>(index) {
            Ok(list) => {
                claimed.push(STRUCTURE_INDEX.into());
                for e in list {
                    let ok_id = !e.id.is_empty()
                        && e.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        && !structures.iter().any(|s| s.id == e.id);
                    let s = Structure { id: e.id, name: e.name, format: e.format, source: e.source, linking: e.linking, data: Rc::from(&[][..]) };
                    match take(&e.file) {
                        Some(bytes) if ok_id && e.file == s.file_name() => {
                            claimed.push(e.file);
                            structures.push(Structure { data: Rc::from(bytes), ..s });
                        }
                        Some(_) => warnings.push(format!("structure \"{}\" has an unexpected file name and was skipped", s.name)),
                        None => warnings.push(format!("structure \"{}\" is missing its file {}", s.name, e.file)),
                    }
                }
            }
            Err(_) => warnings.push("the list of attached structures was unreadable; it was kept but can't be shown".into()),
        }
    }

    let extras = entries
        .into_iter()
        .filter(|(n, _)| !KNOWN.contains(&n.as_str()) && !claimed.contains(n) && safe_name(n))
        .collect();
    Ok(Bundle { record, revision, history, saves, structures, extras, warnings })
}

/// Entries we carry over must be plain relative paths.
fn safe_name(n: &str) -> bool {
    !n.is_empty() && !n.starts_with('/') && !n.contains('\\') && !n.split('/').any(|p| p == ".." || p.is_empty())
}

fn to_json(r: &Seq) -> SequenceFile {
    let features = r
        .features
        .iter()
        .map(|f| {
            let (segments, strand) = model::location_segments(&f.location);
            FeatureJson {
                kind: f.kind.to_string(),
                strand,
                segments: segments.into_iter().map(|(s, e)| [s, e]).collect(),
                qualifiers: f.qualifiers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            }
        })
        .collect();
    SequenceFile {
        name: r.name.clone().unwrap_or_default(),
        topology: if r.topology == Topology::Circular { "circular" } else { "linear" }.into(),
        molecule_type: r.molecule_type.clone(),
        sequence: String::from_utf8_lossy(&r.seq).into_owned(),
        features,
        record: RecordMeta {
            definition: r.definition.clone(),
            accession: r.accession.clone(),
            version: r.version.clone(),
            keywords: r.keywords.clone(),
            dblink: r.dblink.clone(),
            division: Some(r.division.clone()).filter(|d| !d.is_empty()),
            date: r.date.as_ref().map(|d| format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())),
            source: r.source.as_ref().map(|s| SourceJson { source: s.source.clone(), organism: s.organism.clone() }),
            references: r
                .references
                .iter()
                .map(|x| ReferenceJson {
                    description: x.description.clone(),
                    title: x.title.clone(),
                    authors: x.authors.clone(),
                    consortium: x.consortium.clone(),
                    journal: x.journal.clone(),
                    pubmed: x.pubmed.clone(),
                    remark: x.remark.clone(),
                })
                .collect(),
            comments: r.comments.clone(),
        },
    }
}

fn from_json(s: SequenceFile) -> Seq {
    let mut r = Seq::empty();
    r.name = Some(s.name).filter(|n| !n.is_empty());
    r.topology = if s.topology == "circular" { Topology::Circular } else { Topology::Linear };
    r.molecule_type = s.molecule_type.or(Some("DNA".into()));
    r.seq = s.sequence.into_bytes();
    r.seq.retain(|b| b.is_ascii_alphabetic());
    r.seq.make_ascii_uppercase();
    r.len = Some(r.seq.len());
    let len = r.seq.len();
    r.features = s
        .features
        .into_iter()
        .filter_map(|f| {
            let segs: Vec<_> = f.segments.iter().map(|&[a, b]| (a, b)).filter(|&(a, b)| a < b && b <= len).collect();
            Some(GbFeature {
                kind: f.kind.into(),
                location: model::segments_location(&segs, f.strand)?,
                qualifiers: f.qualifiers.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            })
        })
        .collect();
    let m = s.record;
    r.definition = m.definition;
    r.accession = m.accession;
    r.version = m.version;
    r.keywords = m.keywords;
    r.dblink = m.dblink;
    if let Some(d) = m.division {
        r.division = d;
    }
    r.date = m.date.and_then(|d| {
        let mut it = d.splitn(3, '-').map(|p| p.parse::<u32>().ok());
        let (y, mo, da) = (it.next()??, it.next()??, it.next()??);
        Date::from_ymd(y as i32, mo, da).ok()
    });
    r.source = m.source.map(|s| Source { source: s.source, organism: s.organism });
    r.references = m
        .references
        .into_iter()
        .map(|x| Reference {
            description: x.description,
            authors: x.authors,
            consortium: x.consortium,
            title: x.title,
            journal: x.journal,
            pubmed: x.pubmed,
            remark: x.remark,
        })
        .collect();
    r.comments = m.comments;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Op;

    fn sample() -> Seq {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/pBR322.gb");
        gb_io::reader::parse_file(path).unwrap().remove(0)
    }

    #[test]
    fn round_trips_record_history_and_unknown_entries() {
        let mut record = sample();
        record.seq.make_ascii_uppercase();
        let history = vec![
            Edit { rev: 1, time: 10, op: Op::Insert { pos: 3, bases: "GG".into() } },
            Edit { rev: 2, time: 11, op: Op::Delete { pos: 0, bases: "T".into() } },
        ];
        let saves = vec![SavePoint { rev: 2, time: 12, label: None }];
        let extras = vec![("alignments/a1.json".to_string(), b"{\"revision\":1}".to_vec())];
        let structures = vec![Structure {
            id: "2y0g".into(),
            name: "EGFP".into(),
            format: StructureFormat::Pdb,
            source: Some("PDB 2Y0G".into()),
            linking: ChainLinking::Auto,
            data: Rc::from(&b"ATOM      1  CA  GLY A   1       0.000   0.000   0.000  1.00  0.00           C\n"[..]),
        }];
        let bytes = write(BundleRef {
            record: &record,
            revision: 2,
            history: &history,
            saves: &saves,
            structures: &structures,
            extras: &extras,
        })
        .unwrap();
        assert!(looks_like_zip(&bytes));
        assert_eq!(&bytes[30..38], b"mimetype", "mimetype must be the first entry");

        let back = read(&bytes).unwrap();
        assert!(back.warnings.is_empty());
        assert_eq!(back.revision, 2);
        assert_eq!(back.history, history);
        assert_eq!(back.saves, saves);
        assert_eq!(back.extras, extras);
        assert_eq!(back.structures, structures);
        let r = back.record;
        assert_eq!(r.seq, record.seq);
        assert_eq!(r.topology, record.topology);
        assert_eq!(r.definition, record.definition);
        assert_eq!(r.references.len(), record.references.len());
        assert_eq!(r.source, record.source);
        assert_eq!(r.date, record.date);
        assert_eq!(r.features.len(), record.features.len());
        for (a, b) in r.features.iter().zip(&record.features) {
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.qualifiers, b.qualifiers);
            assert_eq!(model::location_segments(&a.location), model::location_segments(&b.location));
        }
    }

    #[test]
    fn refuses_newer_versions_and_foreign_zips() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("manifest.json", SimpleFileOptions::default()).unwrap();
        zip.write_all(br#"{"format":"plasmid-gpui","format_version":99}"#).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let err = read(&bytes).err().unwrap().to_string();
        assert!(err.contains("newer"), "{err}");

        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("word/document.xml", SimpleFileOptions::default()).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(read(&bytes).is_err());
    }

    #[test]
    fn keeps_structure_files_the_index_cannot_vouch_for() {
        let record = sample();
        // A damaged index is kept verbatim (as is the file it named) and doesn't
        // collide with a fresh index written after another structure is attached.
        let extras = vec![
            (STRUCTURE_INDEX.to_string(), b"not json".to_vec()),
            ("structures/old.cif".to_string(), b"data_old\n".to_vec()),
        ];
        let bundle = |structures: &[Structure], extras: &[(String, Vec<u8>)]| {
            let b = BundleRef { record: &record, revision: 0, history: &[], saves: &[], structures, extras };
            read(&write(b).unwrap()).unwrap()
        };
        let back = bundle(&[], &extras);
        assert!(back.structures.is_empty());
        assert_eq!(back.warnings.len(), 1);
        assert_eq!(back.extras, extras);

        let fresh = [Structure {
            id: "new".into(),
            name: "new".into(),
            format: StructureFormat::Mmcif,
            source: None,
            linking: ChainLinking::Auto,
            data: Rc::from(&b"data_x\n"[..]),
        }];
        let back = bundle(&fresh, &back.extras);
        assert_eq!(back.structures, fresh);
        assert_eq!(back.extras, vec![("structures/old.cif".to_string(), b"data_old\n".to_vec())]);
    }

    #[test]
    fn structure_ids_are_file_safe_and_unique() {
        let taken = [Structure {
            id: "af-p42212".into(),
            name: String::new(),
            format: StructureFormat::Pdb,
            source: None,
            linking: ChainLinking::Auto,
            data: Rc::from(&[][..]),
        }];
        assert_eq!(structure_id("AF-P42212", &taken), "af-p42212-2");
        assert_eq!(structure_id("my file (1)", &[]), "my_file__1_");
        assert_eq!(structure_id("", &[]), "structure");
    }

    #[test]
    fn drops_unsafe_entry_names() {
        assert!(safe_name("alignments/a.json"));
        assert!(!safe_name("../evil"));
        assert!(!safe_name("/etc/passwd"));
        assert!(!safe_name("a//b"));
    }
}

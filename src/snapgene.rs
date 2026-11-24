//! Read-only import of SnapGene `.dna` files.
//!
//! The format is undocumented. This follows the reverse-engineered layout that
//! Biopython's `SnapGeneIO` uses: a series of packets, each a type byte, a big-endian
//! `u32` length and that many bytes of payload.
//!
//! | type | payload |
//! |---|---|
//! | `0x09` | header: `"SnapGene"` then file type and version numbers |
//! | `0x00` | sequence: a flags byte (bit 0 = circular), then the bases as ASCII |
//! | `0x0A` | features as XML |
//! | `0x05` | primers as XML (imported as `primer_bind` features) |
//! | `0x06` | notes as XML (description and comments) |
//!
//! Every other packet (history, alignments, compressed undo data…) is skipped. We
//! never write `.dna`: SnapGene opens GenBank, and a hand-rolled writer risks files
//! that SnapGene silently mangles.

use std::borrow::Cow;

use anyhow::{bail, Context as _, Result};
use gb_io::seq::{Feature as GbFeature, Location, Seq, Topology};

pub const MAGIC: u8 = 0x09;

pub fn looks_like_snapgene(bytes: &[u8]) -> bool {
    bytes.len() >= 13 && bytes[0] == MAGIC && &bytes[5..13] == b"SnapGene"
}

pub fn parse(bytes: &[u8]) -> Result<Seq> {
    if !looks_like_snapgene(bytes) {
        bail!("not a SnapGene file (missing the SnapGene header)");
    }
    let mut record = Seq::empty();
    record.molecule_type = Some("DNA".into());
    let mut have_seq = false;
    let mut feature_xml = None;
    let mut primer_xml = None;
    let mut notes_xml = None;

    let mut rest = bytes;
    while !rest.is_empty() {
        if rest.len() < 5 {
            bail!("truncated SnapGene packet header");
        }
        let kind = rest[0];
        let len = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
        let Some(payload) = rest.get(5..5 + len) else {
            bail!("SnapGene packet 0x{kind:02x} claims {len} bytes but the file ends first");
        };
        rest = &rest[5 + len..];
        match kind {
            0x00 if !have_seq => {
                let (&flags, bases) = payload.split_first().context("empty SnapGene sequence packet")?;
                record.topology = if flags & 1 != 0 { Topology::Circular } else { Topology::Linear };
                record.seq = bases.iter().copied().filter(u8::is_ascii_alphabetic).collect();
                have_seq = true;
            }
            0x0A => feature_xml = Some(payload),
            0x05 => primer_xml = Some(payload),
            0x06 => notes_xml = Some(payload),
            _ => {}
        }
    }
    if !have_seq {
        bail!("SnapGene file has no DNA sequence (protein and RNA files are not supported)");
    }
    let len = record.seq.len();
    record.len = Some(len);
    let circular = record.topology == Topology::Circular;
    if let Some(xml) = feature_xml {
        record.features.extend(parse_features(xml, len, circular).context("reading SnapGene features")?);
    }
    if let Some(xml) = primer_xml {
        record.features.extend(parse_primers(xml, len, circular).context("reading SnapGene primers")?);
    }
    if let Some(xml) = notes_xml {
        parse_notes(xml, &mut record).context("reading SnapGene notes")?;
    }
    Ok(record)
}

fn xml_doc(bytes: &[u8]) -> Result<String> {
    Ok(std::str::from_utf8(bytes).context("XML is not UTF-8")?.to_owned())
}

fn parse_features(xml: &[u8], len: usize, circular: bool) -> Result<Vec<GbFeature>> {
    let text = xml_doc(xml)?;
    let doc = roxmltree::Document::parse(&text)?;
    let mut out = Vec::new();
    for f in doc.descendants().filter(|n| n.has_tag_name("Feature")) {
        let kind = f.attribute("type").unwrap_or("misc_feature").to_string();
        let mut ranges = Vec::new();
        for seg in f.children().filter(|n| n.has_tag_name("Segment")) {
            if seg.attribute("type") == Some("gap") {
                continue;
            }
            if let Some(r) = seg.attribute("range") {
                ranges.extend(parse_range(r, len, circular));
            }
        }
        let Some(location) = make_location(ranges, f.attribute("directionality") == Some("2")) else { continue };
        let mut qualifiers: Vec<(Cow<'static, str>, Option<String>)> = Vec::new();
        for q in f.children().filter(|n| n.has_tag_name("Q")) {
            let Some(key) = q.attribute("name") else { continue };
            let values: Vec<_> = q.children().filter(|n| n.has_tag_name("V")).collect();
            if values.is_empty() {
                qualifiers.push((key.to_string().into(), None));
            }
            for v in values {
                let value = v.attribute("text").or(v.attribute("predef")).or(v.attribute("int")).map(strip_html);
                qualifiers.push((key.to_string().into(), value));
            }
        }
        // SnapGene shows `name`; keep it as the label unless the file already has one.
        if let Some(name) = f.attribute("name") {
            if !qualifiers.iter().any(|(k, _)| k == "label") {
                qualifiers.insert(0, ("label".into(), Some(name.to_string())));
            }
        }
        out.push(GbFeature { kind: kind.into(), location, qualifiers });
    }
    Ok(out)
}

fn parse_primers(xml: &[u8], len: usize, circular: bool) -> Result<Vec<GbFeature>> {
    let text = xml_doc(xml)?;
    let doc = roxmltree::Document::parse(&text)?;
    let mut out = Vec::new();
    for p in doc.descendants().filter(|n| n.has_tag_name("Primer")) {
        let name = p.attribute("name").unwrap_or("primer");
        // Each site is usually listed twice (full and "simplified"); keep one of each.
        let mut seen = Vec::new();
        for site in p.children().filter(|n| n.has_tag_name("BindingSite")) {
            let (Some(loc), strand) = (site.attribute("location"), site.attribute("boundStrand")) else { continue };
            if seen.contains(&(loc, strand)) {
                continue;
            }
            seen.push((loc, strand));
            let Some(location) = make_location(parse_range(loc, len, circular), strand == Some("1")) else { continue };
            let mut qualifiers = vec![("label".into(), Some(name.to_string()))];
            if let Some(s) = p.attribute("sequence") {
                qualifiers.push(("note".into(), Some(format!("sequence: {s}"))));
            }
            out.push(GbFeature { kind: "primer_bind".into(), location, qualifiers });
        }
    }
    Ok(out)
}

fn parse_notes(xml: &[u8], record: &mut Seq) -> Result<()> {
    let text = xml_doc(xml)?;
    let doc = roxmltree::Document::parse(&text)?;
    let field = |tag: &str| {
        doc.descendants()
            .find(|n| n.has_tag_name(tag))
            .and_then(|n| n.text())
            .map(strip_html)
            .filter(|s| !s.is_empty())
    };
    if let Some(d) = field("Description") {
        record.definition = Some(d);
    }
    if let Some(c) = field("Comments") {
        record.comments.push(c);
    }
    if let Some(a) = field("AccessionNumber") {
        record.accession = Some(a);
    }
    Ok(())
}

/// `"a-b"`, 1-based inclusive, as half-open 0-based segments. On a circular sequence
/// `a > b` means the range wraps the origin, which becomes two segments.
fn parse_range(r: &str, len: usize, circular: bool) -> Vec<(usize, usize)> {
    let Some((a, b)) = r.split_once('-') else { return vec![] };
    let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) else { return vec![] };
    if a == 0 || b == 0 || a > len || b > len {
        return vec![];
    }
    if a <= b {
        vec![(a - 1, b)]
    } else if circular {
        vec![(a - 1, len), (0, b)]
    } else {
        vec![(b - 1, a)]
    }
}

fn make_location(ranges: Vec<(usize, usize)>, reverse: bool) -> Option<Location> {
    let mut parts: Vec<Location> = ranges.into_iter().map(|(s, e)| Location::simple_range(s as i64, e as i64)).collect();
    let loc = match parts.len() {
        0 => return None,
        1 => parts.pop().unwrap(),
        _ => Location::Join(parts),
    };
    Some(if reverse { Location::Complement(Box::new(loc)) } else { loc })
}

/// SnapGene stores rich text as `<html><body>…</body></html>`; keep the text only.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![kind];
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn header() -> Vec<u8> {
        packet(0x09, b"SnapGene\x00\x01\x00\x0e\x00\x10")
    }

    #[test]
    fn parses_sequence_features_primers_and_notes() {
        let mut file = header();
        file.extend(packet(0x00, b"\x03acgtacgtacgtacgtacgt"));
        file.extend(packet(0x0B, b"ignored packet"));
        file.extend(packet(
            0x0A,
            br#"<?xml version="1.0"?><Features nextValidID="2">
              <Feature name="bla" directionality="2" type="CDS"><Segment range="3-8" type="standard"/>
                <Q name="note"><V text="&lt;html&gt;&lt;body&gt;beta &amp;amp; lactamase&lt;/body&gt;&lt;/html&gt;"/></Q>
                <Q name="codon_start"><V int="1"/></Q></Feature>
              <Feature name="wrap" type="misc_feature"><Segment range="18-2" type="standard"/></Feature>
            </Features>"#,
        ));
        file.extend(packet(
            0x05,
            br#"<Primers><Primer name="fwd" sequence="ACGT">
              <BindingSite location="1-4" boundStrand="0" simplified="1"/>
              <BindingSite location="1-4" boundStrand="0"/></Primer></Primers>"#,
        ));
        file.extend(packet(0x06, b"<Notes><Description>&lt;html&gt;&lt;body&gt;A test&lt;/body&gt;&lt;/html&gt;</Description></Notes>"));

        let r = parse(&file).unwrap();
        // The parser keeps case; the document capitalizes an all-lower-case sequence.
        assert_eq!(r.seq, b"acgtacgtacgtacgtacgt");
        assert_eq!(r.topology, Topology::Circular);
        assert_eq!(r.definition.as_deref(), Some("A test"));
        assert_eq!(r.features.len(), 3);
        let bla = &r.features[0];
        assert_eq!(bla.kind, "CDS");
        assert_eq!(bla.location, Location::Complement(Box::new(Location::simple_range(2, 8))));
        assert_eq!(bla.qualifier_values("label").next(), Some("bla"));
        assert_eq!(bla.qualifier_values("note").next(), Some("beta & lactamase"));
        assert_eq!(bla.qualifier_values("codon_start").next(), Some("1"));
        assert_eq!(
            r.features[1].location,
            Location::Join(vec![Location::simple_range(17, 20), Location::simple_range(0, 2)])
        );
        assert_eq!(r.features[2].kind, "primer_bind");
        assert_eq!(r.features[2].location, Location::simple_range(0, 4));
    }

    #[test]
    fn rejects_truncated_and_foreign_files() {
        assert!(parse(b"LOCUS       x").is_err());
        let mut file = header();
        file.extend_from_slice(&[0x00, 0, 0, 0, 50, 1, b'A']);
        assert!(parse(&file).is_err());
    }

    #[test]
    fn reads_biopython_sample_if_present() {
        // Biopython's Tests/SnapGene/sample-d.dna, when dropped into samples/.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/sample-d.dna");
        let Ok(bytes) = std::fs::read(path) else { return };
        let r = parse(&bytes).unwrap();
        assert_eq!(r.seq.len(), 1000);
        assert_eq!(r.topology, Topology::Linear);
        assert_eq!(r.features.len(), 4);
        let a = r.features.iter().find(|f| f.qualifier_values("label").next() == Some("FeatureA")).unwrap();
        assert_eq!(a.kind, "promoter");
        assert_eq!(a.location, Location::simple_range(49, 150));
    }
}

//! The NCBI genetic codes (translation tables), and translating features with them.
//!
//! Tables are as published in NCBI's `gc.prt`: amino acids, then start codons, for the
//! 64 codons in TCAG order (TTT, TTC, TTA, TTG, TCT, …, GGG).

use crate::model::{self, Feature};

pub struct Code {
    pub id: u8,
    pub name: &'static str,
    aas: &'static [u8; 64],
    starts: &'static [u8; 64],
}

/// The table GenBank assumes when a CDS doesn't name one.
pub const GENBANK_DEFAULT: u8 = 1;
/// The table new coding features get when the document doesn't suggest one.
pub const NEW_FEATURE_DEFAULT: u8 = 11;

#[rustfmt::skip]
pub static CODES: &[Code] = &[
    Code { id: 1, name: "Standard",
        aas:    b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"---M------**--*----M---------------M----------------------------" },
    Code { id: 2, name: "Vertebrate Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIMMTTTTNNKKSS**VVVVAAAADDEEGGGG",
        starts: b"----------**--------------------MMMM----------**---M------------" },
    Code { id: 3, name: "Yeast Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWTTTTPPPPHHQQRRRRIIMMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------**----------------------MM---------------M------------" },
    Code { id: 4, name: "Mold, Protozoan, Coelenterate Mitochondrial; Mycoplasma/Spiroplasma",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"--MM------**-------M------------MMMM---------------M------------" },
    Code { id: 5, name: "Invertebrate Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIMMTTTTNNKKSSSSVVVVAAAADDEEGGGG",
        starts: b"---M------**--------------------MMMM---------------M------------" },
    Code { id: 6, name: "Ciliate, Dasycladacean and Hexamita Nuclear",
        aas:    b"FFLLSSSSYYQQCC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"--------------*--------------------M----------------------------" },
    Code { id: 9, name: "Echinoderm and Flatworm Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIIMTTTTNNNKSSSSVVVVAAAADDEEGGGG",
        starts: b"----------**-----------------------M---------------M------------" },
    Code { id: 10, name: "Euplotid Nuclear",
        aas:    b"FFLLSSSSYY**CCCWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------**-----------------------M----------------------------" },
    Code { id: 11, name: "Bacterial, Archaeal and Plant Plastid",
        aas:    b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"---M------**--*----M------------MMMM---------------M------------" },
    Code { id: 12, name: "Alternative Yeast Nuclear",
        aas:    b"FFLLSSSSYY**CC*WLLLSPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------**--*----M---------------M----------------------------" },
    Code { id: 13, name: "Ascidian Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIMMTTTTNNKKSSGGVVVVAAAADDEEGGGG",
        starts: b"---M------**----------------------MM---------------M------------" },
    Code { id: 14, name: "Alternative Flatworm Mitochondrial",
        aas:    b"FFLLSSSSYYY*CCWWLLLLPPPPHHQQRRRRIIIMTTTTNNNKSSSSVVVVAAAADDEEGGGG",
        starts: b"-----------*-----------------------M----------------------------" },
    Code { id: 15, name: "Blepharisma Nuclear",
        aas:    b"FFLLSSSSYY*QCC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------*---*--------------------M----------------------------" },
    Code { id: 16, name: "Chlorophycean Mitochondrial",
        aas:    b"FFLLSSSSYY*LCC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------*---*--------------------M----------------------------" },
    Code { id: 21, name: "Trematode Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIMMTTTTNNNKSSSSVVVVAAAADDEEGGGG",
        starts: b"----------**-----------------------M---------------M------------" },
    Code { id: 22, name: "Scenedesmus obliquus Mitochondrial",
        aas:    b"FFLLSS*SYY*LCC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"------*---*---*--------------------M----------------------------" },
    Code { id: 23, name: "Thraustochytrium Mitochondrial",
        aas:    b"FF*LSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"--*-------**--*-----------------M--M---------------M------------" },
    Code { id: 24, name: "Rhabdopleuridae Mitochondrial",
        aas:    b"FFLLSSSSYY**CCWWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSSKVVVVAAAADDEEGGGG",
        starts: b"---M------**-------M---------------M---------------M------------" },
    Code { id: 25, name: "Candidate Division SR1 and Gracilibacteria",
        aas:    b"FFLLSSSSYY**CCGWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"---M------**-----------------------M---------------M------------" },
    Code { id: 26, name: "Pachysolen tannophilus Nuclear",
        aas:    b"FFLLSSSSYY**CC*WLLLAPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------**--*----M---------------M----------------------------" },
    Code { id: 27, name: "Karyorelict Nuclear",
        aas:    b"FFLLSSSSYYQQCCWWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"--------------*--------------------M----------------------------" },
    Code { id: 28, name: "Condylostoma Nuclear",
        aas:    b"FFLLSSSSYYQQCCWWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------**--*--------------------M----------------------------" },
    Code { id: 29, name: "Mesodinium Nuclear",
        aas:    b"FFLLSSSSYYYYCC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"--------------*--------------------M----------------------------" },
    Code { id: 30, name: "Peritrich Nuclear",
        aas:    b"FFLLSSSSYYEECC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"--------------*--------------------M----------------------------" },
    Code { id: 31, name: "Blastocrithidia Nuclear",
        aas:    b"FFLLSSSSYYEECCWWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"----------**-----------------------M----------------------------" },
    Code { id: 32, name: "Balanophoraceae Plastid",
        aas:    b"FFLLSSSSYY*WCC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG",
        starts: b"---M------*---*----M------------MMMM---------------M------------" },
    Code { id: 33, name: "Cephalodiscidae Mitochondrial",
        aas:    b"FFLLSSSSYYY*CCWWLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSSKVVVVAAAADDEEGGGG",
        starts: b"---M-------*-------M---------------M---------------M------------" },
];

impl Code {
    pub fn get(id: u8) -> Option<&'static Code> {
        CODES.iter().find(|c| c.id == id)
    }

    fn index(c: [u8; 3]) -> Option<usize> {
        let ix = |b: u8| match b.to_ascii_uppercase() {
            b'T' | b'U' => Some(0),
            b'C' => Some(1),
            b'A' => Some(2),
            b'G' => Some(3),
            _ => None,
        };
        Some(ix(c[0])? * 16 + ix(c[1])? * 4 + ix(c[2])?)
    }

    /// The amino acid for a codon; `X` when it has an ambiguous base.
    pub fn amino_acid(&self, c: [u8; 3]) -> u8 {
        Self::index(c).map_or(b'X', |i| self.aas[i])
    }

    pub fn is_start(&self, c: [u8; 3]) -> bool {
        Self::index(c).is_some_and(|i| self.starts[i] == b'M')
    }
}

/// Feature types drawn and encoded as a translation.
pub fn is_coding(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("CDS") || kind.eq_ignore_ascii_case("ORF")
}

fn qualifier<'a>(f: &'a Feature, key: &'a str) -> Option<&'a str> {
    f.source.as_ref()?.qualifier_values(key).next()
}

fn has_qualifier(f: &Feature, key: &str) -> bool {
    f.source.as_ref().is_some_and(|g| g.qualifiers.iter().any(|(k, _)| k == key))
}

/// The feature's `/transl_table`, or GenBank's default.
pub fn feature_table(f: &Feature) -> u8 {
    qualifier(f, "transl_table").and_then(|v| v.trim().parse().ok()).unwrap_or(GENBANK_DEFAULT)
}

/// Bases to skip before the first codon, from `/codon_start` (1–3).
pub fn codon_offset(f: &Feature) -> usize {
    qualifier(f, "codon_start").and_then(|v| v.trim().parse::<usize>().ok()).map_or(0, |c| c.saturating_sub(1).min(2))
}

/// The positions of a feature's bases in coding order (the reverse strand runs backwards).
fn coding_positions(f: &Feature) -> Vec<usize> {
    let mut pos: Vec<usize> = f.segments.iter().flat_map(|&(s, e)| s..e).collect();
    if f.strand < 0 {
        pos.reverse();
    }
    pos
}

/// Translate a feature with its own table and frame; returns the protein and, for each
/// amino acid, the sequence positions of its codon. A first codon that the table allows
/// as a start reads as M, as GenBank's `/translation` does.
pub fn translate(seq: &[u8], f: &Feature) -> (Vec<u8>, Vec<[usize; 3]>) {
    let code = Code::get(feature_table(f)).unwrap_or(&CODES[0]);
    let offset = codon_offset(f);
    let pos: Vec<usize> = coding_positions(f).into_iter().filter(|&p| p < seq.len()).collect();
    let mut protein = Vec::new();
    let mut codons = Vec::new();
    for c in pos[offset.min(pos.len())..].chunks_exact(3) {
        let base = |p: usize| if f.strand < 0 { model::complement(seq[p]) } else { seq[p] };
        let codon = [base(c[0]), base(c[1]), base(c[2])];
        let aa = if codons.is_empty() && offset == 0 && code.is_start(codon) { b'M' } else { code.amino_acid(codon) };
        protein.push(aa);
        codons.push([c[0], c[1], c[2]]);
    }
    (protein, codons)
}

/// Fill in a coding feature's `/codon_start` and `/transl_table` if it lacks them, and
/// set its `/translation` from the sequence. A translation someone curated (the feature
/// has `/transl_except`, `/exception` or `/pseudo`) is left alone.
pub fn encode(f: &mut Feature, seq: &[u8], default_table: u8) {
    if !is_coding(&f.kind) || f.strand == 0 {
        return;
    }
    let curated = ["transl_except", "exception", "pseudo"].iter().any(|k| has_qualifier(f, k));
    let needs_frame = !has_qualifier(f, "codon_start");
    let needs_table = !has_qualifier(f, "transl_table");
    let Some(src) = f.gb_source() else { return };
    if needs_frame {
        src.qualifiers.push(("codon_start".into(), Some("1".into())));
    }
    if needs_table {
        src.qualifiers.push(("transl_table".into(), Some(default_table.to_string())));
    }
    if curated {
        return;
    }
    let (mut protein, _) = translate(seq, f);
    if protein.last() == Some(&b'*') {
        protein.pop();
    }
    let translation = String::from_utf8_lossy(&protein).into_owned();
    let Some(src) = f.gb_source() else { return };
    match src.qualifiers.iter_mut().find(|(k, _)| k == "translation") {
        Some((_, v)) => *v = Some(translation),
        None => src.qualifiers.push(("translation".into(), Some(translation))),
    }
}

/// The table new coding features in a document get: the one its coding features
/// already use, if they agree, else `NEW_FEATURE_DEFAULT`.
pub fn document_table(features: &[Feature]) -> u8 {
    let mut tables =
        features.iter().filter(|f| is_coding(&f.kind) && has_qualifier(f, "transl_table")).map(feature_table);
    match tables.next() {
        Some(t) if tables.all(|u| u == t) => t,
        _ => NEW_FEATURE_DEFAULT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cds(segments: Vec<(usize, usize)>, strand: i8, quals: &[(&str, &str)]) -> Feature {
        let mut f = Feature { kind: "CDS".into(), label: String::new(), segments, strand, source: None };
        let src = f.gb_source().unwrap();
        src.qualifiers.extend(quals.iter().map(|(k, v)| (k.to_string().into(), Some(v.to_string()))));
        f
    }

    #[test]
    fn tables_are_well_formed() {
        for c in CODES {
            assert!(c.aas.iter().all(|a| a.is_ascii_uppercase() || *a == b'*'), "table {}", c.id);
            assert!(c.starts.iter().all(|a| matches!(a, b'-' | b'M' | b'*')), "table {}", c.id);
        }
        let std = Code::get(1).unwrap();
        assert_eq!(std.amino_acid(*b"ATG"), b'M');
        assert_eq!(std.amino_acid(*b"TGA"), b'*');
        assert_eq!(std.amino_acid(*b"GGN"), b'X');
        // Vertebrate mitochondria read TGA as W and AGA as a stop.
        let mito = Code::get(2).unwrap();
        assert_eq!(mito.amino_acid(*b"TGA"), b'W');
        assert_eq!(mito.amino_acid(*b"AGA"), b'*');
        // Ciliates read TAA as Q.
        assert_eq!(Code::get(6).unwrap().amino_acid(*b"TAA"), b'Q');
        assert!(Code::get(11).unwrap().is_start(*b"GTG"));
        assert!(!Code::get(1).unwrap().is_start(*b"GTG"));
    }

    #[test]
    fn translation_follows_the_features_table_and_frame() {
        // GTG start, TGA, TAA stop.
        let seq = b"GTGTGAAAATAA";
        let bacterial = cds(vec![(0, 12)], 1, &[("transl_table", "11")]);
        assert_eq!(translate(seq, &bacterial).0, b"M*K*");
        let mito = cds(vec![(0, 12)], 1, &[("transl_table", "2")]);
        // GTG starts mitochondrial genes too, but reads as V inside one.
        assert_eq!(translate(seq, &mito).0, b"MWK*");
        let standard = cds(vec![(0, 12)], 1, &[("transl_table", "1")]);
        assert_eq!(translate(seq, &standard).0, b"V*K*");
        let shifted = cds(vec![(0, 12)], 1, &[("transl_table", "11"), ("codon_start", "2")]);
        assert_eq!(translate(seq, &shifted).0, b"CEN");
    }

    #[test]
    fn matches_the_translations_in_the_samples() {
        let mut checked = 0;
        for name in ["pEGFP-N1.gb", "pBR322.gb", "pUC19.gb", "lambda_phage.gb"] {
            let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/samples")).join(name);
            let doc = crate::model::Document::open(&path).unwrap();
            for f in doc.features.iter().filter(|f| is_coding(&f.kind)) {
                let Some(stored) = qualifier(f, "translation") else { continue };
                let (p, _) = translate(&doc.seq, f);
                let p = String::from_utf8_lossy(p.strip_suffix(b"*").unwrap_or(&p)).into_owned();
                assert_eq!(p, stored.replace(char::is_whitespace, ""), "{name} {}", f.label);
                checked += 1;
            }
        }
        assert!(checked > 10, "only {checked} CDSs had translations");
    }

    #[test]
    fn edits_inside_a_cds_refresh_its_translation() {
        let mut doc = crate::model::Document::open(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/pEGFP-N1.gb"))).unwrap();
        let ix = doc.features.iter().position(|f| f.segments == vec![(678, 1398)]).unwrap();
        // Codon 2 (GTG, V) becomes GCG (A).
        doc.delete(682, 683);
        doc.insert(682, b"C");
        let t = qualifier(&doc.features[ix], "translation").unwrap().to_string();
        assert!(t.starts_with("MASKG"), "{}", &t[..10]);
    }

    #[test]
    fn encode_adds_frame_table_and_translation() {
        let seq = b"ATGAAATAA";
        let mut f = Feature { kind: "CDS".into(), label: "x".into(), segments: vec![(0, 9)], strand: 1, source: None };
        encode(&mut f, seq, 11);
        assert_eq!(qualifier(&f, "codon_start"), Some("1"));
        assert_eq!(qualifier(&f, "transl_table"), Some("11"));
        assert_eq!(qualifier(&f, "translation"), Some("MK"));
        // Re-encoding updates the translation in place.
        encode(&mut f, b"ATGCCCTAA", 11);
        assert_eq!(qualifier(&f, "translation"), Some("MP"));
        assert_eq!(f.source.as_ref().unwrap().qualifier_values("translation").count(), 1);
        // Curated translations stay.
        let mut c = cds(vec![(0, 9)], 1, &[("transl_except", "(pos:4..6,aa:Sec)"), ("translation", "MU")]);
        encode(&mut c, seq, 11);
        assert_eq!(qualifier(&c, "translation"), Some("MU"));
    }
}

//! Colors. The active theme lives in a thread-local (all painting happens on the UI
//! thread), so drawing code keeps calling `theme::text()` and friends.
//!
//! Themes are either the built-in "Plasmid Light" palette (loosely modelled on
//! Benchling) or any base16 scheme: the ~370 bundled from tinted-theming/schemes, or
//! YAML files the user points at in Settings.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{bail, Context as _, Result};
use gpui::{rgb, Hsla, Rgba};

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/bundled_themes.rs"));
}

pub const DEFAULT_THEME_ID: &str = "plasmid-light";

/// Every color the UI draws with.
#[derive(Clone)]
pub struct Theme {
    pub app_bg: Hsla,
    pub panel: Hsla,
    pub editor_bg: Hsla,
    pub border: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub accent: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub ruler: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub error: Hsla,
    /// Feature fills: CDS, gene, promoter, terminator, rep_origin, primer_bind,
    /// protein_bind, RBS/regulatory, RNA, other.
    pub features: [Hsla; 10],
    /// Candidate label colors for feature bars; the one contrasting best with the fill wins.
    pub feature_text_dark: Hsla,
    pub feature_text_light: Hsla,
}

thread_local! {
    static CURRENT: RefCell<Rc<Theme>> = RefCell::new(Rc::new(Theme::plasmid_light()));
}

pub fn set(theme: Theme) {
    CURRENT.with(|t| *t.borrow_mut() = Rc::new(theme));
}

fn get<T>(f: impl FnOnce(&Theme) -> T) -> T {
    CURRENT.with(|t| f(&t.borrow()))
}

pub fn app_bg() -> Hsla { get(|t| t.app_bg) }
pub fn panel() -> Hsla { get(|t| t.panel) }
pub fn editor_bg() -> Hsla { get(|t| t.editor_bg) }
pub fn border() -> Hsla { get(|t| t.border) }
pub fn hover() -> Hsla { get(|t| t.hover) }
pub fn active() -> Hsla { get(|t| t.active) }
pub fn accent() -> Hsla { get(|t| t.accent) }
pub fn text() -> Hsla { get(|t| t.text) }
pub fn muted_text() -> Hsla { get(|t| t.muted_text) }
pub fn ruler() -> Hsla { get(|t| t.ruler) }
pub fn cursor() -> Hsla { get(|t| t.cursor) }
pub fn selection() -> Hsla { get(|t| t.selection) }
pub fn error() -> Hsla { get(|t| t.error) }

pub fn feature(kind: &str) -> Hsla {
    let ix = match kind {
        "CDS" => 0,
        "gene" => 1,
        "promoter" => 2,
        "terminator" => 3,
        "rep_origin" => 4,
        "primer_bind" => 5,
        "protein_bind" | "misc_binding" => 6,
        "RBS" | "regulatory" => 7,
        "mRNA" | "misc_RNA" | "ncRNA" => 8,
        _ => 9,
    };
    get(|t| t.features[ix])
}

/// Label color for text drawn on top of `fill`.
pub fn feature_text(fill: Hsla) -> Hsla {
    get(|t| {
        let l = luminance(fill);
        if contrast(l, luminance(t.feature_text_dark)) >= contrast(l, luminance(t.feature_text_light)) {
            t.feature_text_dark
        } else {
            t.feature_text_light
        }
    })
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn with_alpha(mut color: Hsla, a: f32) -> Hsla {
    color.a = a;
    color
}

/// Linear blend in sRGB space; `t = 0` is `a`, `t = 1` is `b`.
fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (a.into(), b.into());
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1.0 }.into()
}

/// WCAG relative luminance.
fn luminance(color: Hsla) -> f32 {
    let c: Rgba = color.into();
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

fn contrast(l1: f32, l2: f32) -> f32 {
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

impl Theme {
    pub fn plasmid_light() -> Self {
        Self {
            app_bg: c(0xf4f5f7),
            panel: c(0xf7f8fa),
            editor_bg: c(0xffffff),
            border: c(0xdfe2e7),
            hover: c(0xe9edf3),
            active: c(0xdde8fb),
            accent: c(0x2f6fdf),
            text: c(0x1d2330),
            muted_text: c(0x6b7280),
            ruler: c(0x9aa1ad),
            cursor: c(0x2f6fdf),
            selection: with_alpha(c(0x2f6fdf), 0.2),
            error: c(0xd03b3b),
            features: [
                0xf6c56b, 0xf3df9b, 0x8fd18f, 0xe88d8d, 0xf4e36b, 0x9bc4f2, 0xc6a6ee, 0x85d5cf, 0xf2a7c8, 0xc9ced6,
            ]
            .map(c),
            feature_text_dark: c(0x1d2330),
            feature_text_light: c(0xffffff),
        }
    }

    /// Map a base16 palette onto the UI, following the base16 styling guidelines:
    /// 00 default background, 01 lighter background (status bars), 02 selection,
    /// 03 comments, 04 dark foreground, 05 default foreground, 07 lightest,
    /// 08–0F accents (red, orange, yellow, green, cyan, blue, purple, brown).
    pub fn from_base16(p: &[u32; 16]) -> Self {
        let b = p.map(c);
        Self {
            app_bg: b[0x01],
            panel: b[0x01],
            editor_bg: b[0x00],
            border: mix(b[0x01], b[0x02], 0.6),
            hover: mix(b[0x01], b[0x02], 0.35),
            // base02 is the "selection" color, but in some light schemes it is a heavy mid-grey;
            // an accent tint reads as "selected" in every scheme.
            active: mix(b[0x00], b[0x0D], 0.18),
            accent: b[0x0D],
            text: b[0x05],
            muted_text: b[0x04],
            ruler: b[0x03],
            cursor: b[0x0D],
            selection: with_alpha(b[0x0D], 0.28),
            error: b[0x08],
            features: [
                b[0x0A],                       // CDS: yellow
                mix(b[0x0A], b[0x00], 0.45),   // gene: faded yellow (usually hidden under its CDS)
                b[0x0B],                       // promoter: green
                b[0x08],                       // terminator: red
                b[0x09],                       // rep_origin: orange
                b[0x0D],                       // primer_bind: blue
                b[0x0E],                       // protein_bind: purple
                b[0x0C],                       // RBS / regulatory: cyan
                b[0x0F],                       // RNAs: brown
                mix(b[0x03], b[0x00], 0.3),    // anything else: grey
            ],
            feature_text_dark: if luminance(b[0x00]) < luminance(b[0x07]) { b[0x00] } else { b[0x07] },
            feature_text_light: if luminance(b[0x00]) < luminance(b[0x07]) { b[0x07] } else { b[0x00] },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ThemeSource {
    Builtin,
    Bundled,
    Custom(PathBuf),
}

/// One selectable theme.
#[derive(Clone, Debug)]
pub struct ThemeEntry {
    /// Stable key saved in settings: `plasmid-light`, `base16:<slug>`, `file:<path>`.
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub source: ThemeSource,
    /// `None` for the built-in palette.
    pub palette: Option<[u32; 16]>,
}

impl ThemeEntry {
    pub fn theme(&self) -> Theme {
        match &self.palette {
            Some(p) => Theme::from_base16(p),
            None => Theme::plasmid_light(),
        }
    }

    /// The scheme's sixteen colors, for swatches in the picker.
    pub fn swatches(&self) -> [Hsla; 16] {
        self.palette.unwrap_or([
            0xffffff, 0xf7f8fa, 0xdde8fb, 0x9aa1ad, 0x6b7280, 0x1d2330, 0x1d2330, 0x1d2330, 0xe88d8d, 0xf4e36b,
            0xf6c56b, 0x8fd18f, 0x85d5cf, 0x2f6fdf, 0xc6a6ee, 0xf2a7c8,
        ])
        .map(c)
    }
}

/// Parse a base16 scheme in either the current tinted-theming layout
/// (`name:` plus `palette: { base00: "#rrggbb", … }`) or the original chriskempson
/// layout (`scheme:` plus top-level `base00: "rrggbb"`). Returns the name and colors.
pub fn parse_base16(text: &str) -> Result<(String, [u32; 16])> {
    let mut name = None;
    let mut colors: [Option<u32>; 16] = [None; 16];
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or(line).trim(); // drop trailing comments
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim();
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        match key {
            "scheme" | "name" if name.is_none() && !value.is_empty() => name = Some(value.to_string()),
            _ if key.len() == 6 && key.starts_with("base0") => {
                let Some(ix) = key[5..].chars().next().and_then(|ch| ch.to_digit(16)) else { continue };
                let hex = value.trim_start_matches('#');
                if hex.len() != 6 {
                    bail!("{key}: expected a 6-digit hex color, got {value:?}");
                }
                colors[ix as usize] = Some(u32::from_str_radix(hex, 16).with_context(|| format!("{key}: {value:?}"))?);
            }
            _ => {}
        }
    }
    let mut out = [0u32; 16];
    for (ix, v) in colors.iter().enumerate() {
        out[ix] = v.with_context(|| format!("missing base0{ix:X}"))?;
    }
    Ok((name.unwrap_or_else(|| "Untitled scheme".into()), out))
}

fn entry_from_palette(id: String, name: String, palette: [u32; 16], source: ThemeSource) -> ThemeEntry {
    let dark = luminance(c(palette[0])) < 0.3;
    ThemeEntry { id, name, dark, source, palette: Some(palette) }
}

/// The built-in palette followed by every bundled base16 scheme, sorted by name.
pub fn bundled_themes() -> Vec<ThemeEntry> {
    let mut out: Vec<ThemeEntry> = bundled::BUNDLED
        .iter()
        .filter_map(|(slug, text)| {
            let (name, palette) = parse_base16(text).ok()?;
            Some(entry_from_palette(format!("base16:{slug}"), name, palette, ThemeSource::Bundled))
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out.insert(
        0,
        ThemeEntry {
            id: DEFAULT_THEME_ID.into(),
            name: "Plasmid Light (default)".into(),
            dark: false,
            source: ThemeSource::Builtin,
            palette: None,
        },
    );
    out
}

/// Load the scheme at `path`, or every `.yaml`/`.yml` scheme in it when it is a folder.
/// Files that fail to parse are reported, not fatal.
pub fn load_custom(path: &Path) -> (Vec<ThemeEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    let files: Vec<PathBuf> = if path.is_dir() {
        match std::fs::read_dir(path) {
            Ok(rd) => {
                let mut v: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")))
                    .collect();
                v.sort();
                v
            }
            Err(e) => {
                errors.push(format!("{}: {e}", path.display()));
                Vec::new()
            }
        }
    } else {
        vec![path.to_path_buf()]
    };
    for file in files {
        let parsed = std::fs::read_to_string(&file).map_err(anyhow::Error::from).and_then(|t| parse_base16(&t));
        match parsed {
            Ok((name, palette)) => entries.push(entry_from_palette(
                format!("file:{}", file.display()),
                name,
                palette,
                ThemeSource::Custom(path.to_path_buf()),
            )),
            Err(e) => errors.push(format!("{}: {e:#}", file.display())),
        }
    }
    (entries, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tinted_layout() {
        let (name, p) = parse_base16(
            "system: \"base16\"\nname: \"Ocean\"\nauthor: \"x\"\nvariant: \"dark\"\npalette:\n  base00: \"#2b303b\"\n  base01: \"#343d46\"\n  base02: \"#4f5b66\"\n  base03: \"#65737e\"\n  base04: \"#a7adba\"\n  base05: \"#c0c5ce\"\n  base06: \"#dfe1e8\"\n  base07: \"#eff1f5\"\n  base08: \"#bf616a\"\n  base09: \"#d08770\"\n  base0A: \"#ebcb8b\"\n  base0B: \"#a3be8c\"\n  base0C: \"#96b5b4\"\n  base0D: \"#8fa1b3\"\n  base0E: \"#b48ead\"\n  base0F: \"#ab7967\"\n",
        )
        .unwrap();
        assert_eq!(name, "Ocean");
        assert_eq!(p[0], 0x2b303b);
        assert_eq!(p[0xA], 0xebcb8b);
        assert_eq!(p[0xF], 0xab7967);
    }

    #[test]
    fn parses_legacy_layout() {
        let mut text = String::from("scheme: \"Legacy\"\nauthor: \"y\"\n");
        for i in 0..16 {
            text.push_str(&format!("base0{i:X}: \"{:06x}\" # comment\n", i * 0x111111));
        }
        let (name, p) = parse_base16(&text).unwrap();
        assert_eq!(name, "Legacy");
        assert_eq!(p[0xF], 0xffffff);
    }

    #[test]
    fn rejects_incomplete_scheme() {
        assert!(parse_base16("name: x\nbase00: \"000000\"\n").is_err());
    }

    #[test]
    fn every_bundled_scheme_parses() {
        for (slug, text) in bundled::BUNDLED {
            parse_base16(text).unwrap_or_else(|e| panic!("{slug}: {e:#}"));
        }
        assert!(bundled::BUNDLED.len() > 300);
    }
}

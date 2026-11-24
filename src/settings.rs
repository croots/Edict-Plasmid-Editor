//! User settings, stored as plain `key = value` lines in
//! `$XDG_CONFIG_HOME/plasmid-gpui/settings.conf` (`~/.config/...` by default).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::theme::DEFAULT_THEME_ID;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// A `ThemeEntry::id`.
    pub theme: String,
    /// base16 scheme files, or folders of them, added in Settings.
    pub custom_theme_paths: Vec<PathBuf>,
    /// The name the edit history records changes under; the account's name if unset.
    pub author: Option<String>,
    /// Sent to NCBI with BLAST searches; their usage rules ask for a contact address.
    pub email: Option<String>,
    /// The BLAST dialog's last options, saved as `blast.<key> = value` lines.
    pub blast: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: DEFAULT_THEME_ID.into(), custom_theme_paths: Vec::new(), author: None, email: None, blast: BTreeMap::new() }
    }
}

pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("plasmid-gpui").join("settings.conf"))
}

impl Settings {
    pub fn load() -> Self {
        config_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| Self::parse(&t)).unwrap_or_default()
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "theme" if !value.is_empty() => s.theme = value.to_string(),
                "custom_theme" if !value.is_empty() => s.custom_theme_paths.push(PathBuf::from(value)),
                "author" if !value.is_empty() => s.author = Some(value.to_string()),
                "email" if !value.is_empty() => s.email = Some(value.to_string()),
                k if k.starts_with("blast.") && k.len() > "blast.".len() => {
                    s.blast.insert(k["blast.".len()..].to_string(), value.to_string());
                }
                _ => {}
            }
        }
        s
    }

    fn serialize(&self) -> String {
        let mut out = String::from("# plasmid-gpui settings\n");
        out.push_str(&format!("theme = {}\n", self.theme));
        for p in &self.custom_theme_paths {
            out.push_str(&format!("custom_theme = {}\n", p.display()));
        }
        if let Some(a) = &self.author {
            out.push_str(&format!("author = {a}\n"));
        }
        if let Some(e) = &self.email {
            out.push_str(&format!("email = {e}\n"));
        }
        for (k, v) in &self.blast {
            out.push_str(&format!("blast.{k} = {}\n", v.replace('\n', " ")));
        }
        out
    }

    /// Who the edit history credits: the `author` setting, else the login name.
    pub fn effective_author(&self) -> Option<String> {
        self.author.clone().or_else(|| {
            ["USER", "USERNAME", "LOGNAME"].iter().find_map(|k| std::env::var(k).ok()).filter(|n| !n.trim().is_empty())
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path().context("no HOME or XDG_CONFIG_HOME to save settings in")?;
        write(&path, &self.serialize())
    }
}

thread_local! {
    static SHARED: RefCell<Settings> = RefCell::new(Settings::default());
}

/// The settings as the workspace last set them, for views that don't own them
/// (the BLAST dialog reads the email and its remembered options here).
pub fn shared() -> Settings {
    SHARED.with(|s| s.borrow().clone())
}

pub fn set_shared(settings: Settings) {
    SHARED.with(|s| *s.borrow_mut() = settings);
}

/// Change the shared settings and save them.
pub fn update_shared(f: impl FnOnce(&mut Settings)) -> Result<()> {
    let s = SHARED.with(|s| {
        let mut s = s.borrow_mut();
        f(&mut s);
        s.clone()
    });
    s.save()
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    crate::model::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = Settings {
            theme: "file:/home/me/themes/my theme.yaml".into(),
            custom_theme_paths: vec!["/home/me/themes".into(), "/tmp/one.yaml".into()],
            author: Some("Ada Lovelace".into()),
            email: Some("ada@example.org".into()),
            blast: [("program".to_string(), "blastx".to_string()), ("organism".to_string(), "Homo sapiens (taxid:9606)".to_string())].into(),
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
    }
}

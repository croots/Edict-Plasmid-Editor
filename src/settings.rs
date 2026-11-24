//! User settings, stored as plain `key = value` lines in
//! `$XDG_CONFIG_HOME/plasmid-gpui/settings.conf` (`~/.config/...` by default).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::theme::DEFAULT_THEME_ID;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// A `ThemeEntry::id`.
    pub theme: String,
    /// base16 scheme files, or folders of them, added in Settings.
    pub custom_theme_paths: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: DEFAULT_THEME_ID.into(), custom_theme_paths: Vec::new() }
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
        out
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path().context("no HOME or XDG_CONFIG_HOME to save settings in")?;
        write(&path, &self.serialize())
    }
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
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
    }
}

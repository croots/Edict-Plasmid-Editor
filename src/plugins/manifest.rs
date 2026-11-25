//! A plugin's manifest: the `[tool.edict]` table of the PEP 723 block at the top
//! of its `plugin.py`. The same block's `dependencies` are what `uv run --script`
//! installs, so one readable file says what the plugin needs and what it does.
//!
//! ```python
//! # /// script
//! # requires-python = ">=3.10"
//! # dependencies = ["esm==3.5.0"]
//! #
//! # [tool.edict]
//! # id = "esmfold2"
//! # name = "ESMFold2"
//! # api = 1
//! #
//! # [[tool.edict.env]]            # tried in order; the first one present is used
//! # kind = "conda"
//! # name = "esmfold2"
//! #
//! # [[tool.edict.action]]
//! # id = "predict"
//! # label = "Predict structure (ESMFold2)"
//! # on = "feature"
//! # kinds = ["CDS", "ORF"]
//! # input = "protein"
//! # output = "structure"
//! #
//! # [[tool.edict.setting]]
//! # key = "model"
//! # label = "Model"
//! # type = "choice"
//! # choices = [{ value = "fast", label = "Fast" }, "full"]
//! # default = "fast"
//! # ///
//! ```

use std::collections::BTreeMap;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

/// The plugin protocol version this app speaks.
pub const API: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub api: u32,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Python environments to run in, in order of preference.
    #[serde(default)]
    pub env: Vec<EnvSpec>,
    #[serde(default, rename = "action")]
    pub actions: Vec<Action>,
    #[serde(default, rename = "setting")]
    pub settings: Vec<Setting>,
}

/// Where the plugin's Python comes from.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum EnvSpec {
    /// `uv run --script plugin.py`: uv installs the block's dependencies itself.
    Uv,
    /// A virtualenv folder (relative to the plugin's folder, or absolute, or `~/...`).
    Venv { path: String },
    /// A conda environment, by name or by prefix folder.
    Conda {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        prefix: Option<String>,
    },
    /// `nix shell <installable> -c python`. `{flake}` stands for this app's flake.
    Nix { installable: String },
    /// A Python interpreter, by path or by name on PATH ("python3").
    Python { path: String },
}

impl EnvSpec {
    /// Parse the user's choice, as saved in settings: "uv", "venv:<path>",
    /// "conda:<name>", "conda-prefix:<path>", "nix:<installable>", "python:<path>".
    pub fn parse_choice(s: &str) -> Option<EnvSpec> {
        let s = s.trim();
        let (kind, arg) = s.split_once(':').map(|(k, a)| (k.trim(), a.trim().to_string())).unwrap_or((s, String::new()));
        let need = |a: String| (!a.is_empty()).then_some(a);
        Some(match kind {
            "uv" => EnvSpec::Uv,
            "venv" => EnvSpec::Venv { path: need(arg)? },
            "conda" => EnvSpec::Conda { name: Some(need(arg)?), prefix: None },
            "conda-prefix" => EnvSpec::Conda { name: None, prefix: Some(need(arg)?) },
            "nix" => EnvSpec::Nix { installable: need(arg)? },
            "python" => EnvSpec::Python { path: need(arg)? },
            _ => return None,
        })
    }

    /// The inverse of `parse_choice`.
    pub fn to_choice(&self) -> String {
        match self {
            EnvSpec::Uv => "uv".into(),
            EnvSpec::Venv { path } => format!("venv:{path}"),
            EnvSpec::Conda { name: Some(n), .. } => format!("conda:{n}"),
            EnvSpec::Conda { prefix: Some(p), .. } => format!("conda-prefix:{p}"),
            EnvSpec::Conda { .. } => "conda:".into(),
            EnvSpec::Nix { installable } => format!("nix:{installable}"),
            EnvSpec::Python { path } => format!("python:{path}"),
        }
    }

    /// "conda env esmfold2", for the Settings page.
    pub fn describe(&self) -> String {
        match self {
            EnvSpec::Uv => "uv (installs the script's dependencies)".into(),
            EnvSpec::Venv { path } => format!("virtualenv {path}"),
            EnvSpec::Conda { name: Some(n), .. } => format!("conda env {n}"),
            EnvSpec::Conda { prefix: Some(p), .. } => format!("conda env at {p}"),
            EnvSpec::Conda { .. } => "conda".into(),
            EnvSpec::Nix { installable } => format!("nix {installable}"),
            EnvSpec::Python { path } => format!("python {path}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    /// The menu item's text.
    pub label: String,
    /// What it's offered on: "feature" (a feature's right-click menu).
    pub on: String,
    /// Feature kinds it's offered for ("CDS", "ORF"); empty for every kind.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// What the plugin is given: "protein" (the feature translated) or "dna".
    pub input: String,
    /// What it gives back: "structure".
    pub output: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl Action {
    pub fn offered_for(&self, kind: &str) -> bool {
        self.on == "feature" && (self.kinds.is_empty() || self.kinds.iter().any(|k| k.eq_ignore_ascii_case(kind)))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SettingType {
    Choice,
    Int,
    Float,
    Bool,
    String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Choice {
    Plain(String),
    Labelled {
        value: String,
        label: String,
        #[serde(default)]
        help: Option<String>,
    },
}

impl Choice {
    pub fn value(&self) -> &str {
        match self {
            Choice::Plain(v) | Choice::Labelled { value: v, .. } => v,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Choice::Plain(v) => v,
            Choice::Labelled { label, .. } => label,
        }
    }

    pub fn help(&self) -> Option<&str> {
        match self {
            Choice::Labelled { help, .. } => help.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub ty: SettingType,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    pub default: toml::Value,
    #[serde(default)]
    pub help: Option<String>,
    /// A heading the setting is shown under ("Memory").
    #[serde(default)]
    pub group: Option<String>,
    /// Only for these actions; every action when empty.
    #[serde(default)]
    pub actions: Vec<String>,
}

impl Setting {
    /// The default as the text a form field holds.
    pub fn default_text(&self) -> String {
        match &self.default {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::Boolean(b) => b.to_string(),
            other => format!("{other:?}"),
        }
    }

    pub fn applies_to(&self, action: &str) -> bool {
        self.actions.is_empty() || self.actions.iter().any(|a| a == action)
    }

    /// Check a value as typed, and turn it into the JSON the plugin gets.
    pub fn value(&self, text: &str) -> std::result::Result<serde_json::Value, String> {
        let text = text.trim();
        let range = |v: f64| -> std::result::Result<(), String> {
            match (self.min, self.max) {
                (Some(lo), _) if v < lo => Err(format!("{} must be at least {lo}", self.label)),
                (_, Some(hi)) if v > hi => Err(format!("{} must be at most {hi}", self.label)),
                _ => Ok(()),
            }
        };
        match self.ty {
            SettingType::Choice => self
                .choices
                .iter()
                .find(|c| c.value() == text)
                .map(|c| serde_json::Value::String(c.value().to_string()))
                .ok_or_else(|| format!("{}: pick one of the choices", self.label)),
            SettingType::Int => {
                let v: i64 = text.parse().map_err(|_| format!("{} must be a whole number", self.label))?;
                range(v as f64)?;
                Ok(v.into())
            }
            SettingType::Float => {
                let v: f64 = text.parse().map_err(|_| format!("{} must be a number", self.label))?;
                if !v.is_finite() {
                    return Err(format!("{} must be a number", self.label));
                }
                range(v)?;
                Ok(serde_json::Number::from_f64(v).map(serde_json::Value::Number).unwrap_or_default())
            }
            SettingType::Bool => match text {
                "true" => Ok(true.into()),
                "false" => Ok(false.into()),
                _ => Err(format!("{} must be true or false", self.label)),
            },
            SettingType::String => Ok(text.to_string().into()),
        }
    }
}

#[derive(Deserialize)]
struct ScriptBlock {
    #[serde(default)]
    tool: BTreeMap<String, toml::Value>,
}

/// The TOML inside a `# /// script` ... `# ///` comment block (PEP 723).
pub fn script_block(source: &str) -> Option<String> {
    let mut lines = source.lines();
    lines.by_ref().find(|l| l.trim_end() == "# /// script")?;
    let mut toml = String::new();
    for line in lines {
        let line = line.trim_end();
        if line == "# ///" {
            return Some(toml);
        }
        let content = if line == "#" {
            ""
        } else if let Some(rest) = line.strip_prefix("# ") {
            rest
        } else {
            // Not a comment line: the block was never closed.
            return None;
        };
        toml.push_str(content);
        toml.push('\n');
    }
    None
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Read and check a plugin script's manifest.
pub fn parse(source: &str) -> Result<Manifest> {
    let block = script_block(source).context("no `# /// script` block at the top of the file")?;
    let block: ScriptBlock = toml::from_str(&block).context("the script block isn't valid TOML")?;
    let table = block.tool.get("edict").cloned().context("the script block has no [tool.edict] table")?;
    let m: Manifest = table.try_into().context("in [tool.edict]")?;
    check(&m)?;
    Ok(m)
}

fn check(m: &Manifest) -> Result<()> {
    if m.api != API {
        bail!("written for plugin API {}, but this version of the app speaks API {API}", m.api);
    }
    if !valid_id(&m.id) {
        bail!("id {:?}: use lowercase letters, digits, - and _", m.id);
    }
    if m.actions.is_empty() {
        bail!("no [[tool.edict.action]]: the plugin wouldn't appear anywhere");
    }
    let mut seen = Vec::new();
    for a in &m.actions {
        if !valid_id(&a.id) || seen.contains(&&a.id) {
            bail!("action id {:?} is missing, repeated, or has characters other than a-z 0-9 - _", a.id);
        }
        seen.push(&a.id);
        if a.on != "feature" {
            bail!("action {}: on = {:?} isn't supported (only \"feature\")", a.id, a.on);
        }
        if !matches!(a.input.as_str(), "protein" | "dna") {
            bail!("action {}: input = {:?} isn't supported (\"protein\" or \"dna\")", a.id, a.input);
        }
        if a.output != "structure" {
            bail!("action {}: output = {:?} isn't supported (only \"structure\")", a.id, a.output);
        }
    }
    let mut keys = Vec::new();
    for s in &m.settings {
        if !valid_id(&s.key) || s.key == "env" || keys.contains(&&s.key) {
            bail!("setting key {:?} is missing, repeated, reserved, or has characters other than a-z 0-9 - _", s.key);
        }
        keys.push(&s.key);
        if s.ty == SettingType::Choice && s.choices.is_empty() {
            bail!("setting {}: a choice needs choices", s.key);
        }
        if let Err(e) = s.value(&s.default_text()) {
            bail!("setting {}: its default doesn't fit: {e}", s.key);
        }
        if let Some(a) = s.actions.iter().find(|a| !m.actions.iter().any(|x| &x.id == *a)) {
            bail!("setting {}: no action {a:?}", s.key);
        }
    }
    for e in &m.env {
        if let EnvSpec::Conda { name: None, prefix: None } = e {
            bail!("a conda env needs a name or a prefix");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy"]
#
# [tool.edict]
# id = "toy"
# name = "Toy"
# api = 1
#
# [[tool.edict.env]]
# kind = "conda"
# name = "toy"
#
# [[tool.edict.env]]
# kind = "uv"
#
# [[tool.edict.action]]
# id = "fold"
# label = "Fold it"
# on = "feature"
# kinds = ["CDS", "ORF"]
# input = "protein"
# output = "structure"
#
# [[tool.edict.setting]]
# key = "model"
# label = "Model"
# type = "choice"
# choices = [{ value = "fast", label = "Fast" }, "full"]
# default = "fast"
#
# [[tool.edict.setting]]
# key = "floor"
# label = "RAM floor"
# type = "float"
# min = 0.5
# default = 2.0
# ///
import sys
"#;

    #[test]
    fn parses_a_manifest() {
        let m = parse(GOOD).unwrap();
        assert_eq!(m.id, "toy");
        assert_eq!(m.env, vec![EnvSpec::Conda { name: Some("toy".into()), prefix: None }, EnvSpec::Uv]);
        assert!(m.actions[0].offered_for("cds"));
        assert!(!m.actions[0].offered_for("gene"));
        assert_eq!(m.settings[0].choices[1].label(), "full");
        assert_eq!(m.settings[1].default_text(), "2");
        assert_eq!(m.settings[1].value("2.5").unwrap(), serde_json::json!(2.5));
        assert!(m.settings[1].value("0.1").is_err());
        assert!(m.settings[0].value("huge").is_err());
    }

    #[test]
    fn rejects_bad_manifests() {
        assert!(parse("print('hi')").is_err());
        assert!(parse(&GOOD.replace("api = 1", "api = 2")).unwrap_err().to_string().contains("API"));
        assert!(parse(&GOOD.replace("default = \"fast\"", "default = \"slow\"")).is_err());
        assert!(parse(&GOOD.replace("kind = \"uv\"", "kind = \"pip\"")).is_err());
        // An unclosed block.
        assert!(parse(&GOOD.replace("# ///\nimport", "import")).is_err());
    }

    #[test]
    fn env_choices_round_trip() {
        for s in ["uv", "venv:~/envs/esm", "conda:esm", "conda-prefix:/opt/c", "nix:{flake}#esmfold2-env", "python:python3"] {
            assert_eq!(EnvSpec::parse_choice(s).unwrap().to_choice(), s);
        }
        assert!(EnvSpec::parse_choice("conda:").is_none());
        assert!(EnvSpec::parse_choice("pip").is_none());
    }
}

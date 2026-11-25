//! Plugins: readable Python scripts in `~/.config/plasmid-gpui/plugins/<id>/plugin.py`
//! that add actions to the app (Predict structure on a CDS's right-click menu).
//!
//! A plugin's manifest (see `manifest`) says which Python environment it runs in,
//! where it appears, and what settings its dialog offers. Running it is a task like a
//! BLAST search: its dialog shows the settings, then the script's terminal output
//! (and can be backgrounded), then the result, which the user may attach.

pub mod dialog;
pub mod env;
mod form;
pub mod manifest;
pub mod runner;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{App, Global};

use manifest::{Action, EnvSpec, Manifest};

/// Plugins that ship with the app, installed from Settings → Plugins.
pub const BUNDLED: &[(&str, &str)] = &[
    ("esmfold2", include_str!("../../plugins/esmfold2/plugin.py")),
    ("ideal-helix", include_str!("../../plugins/ideal-helix/plugin.py")),
];

pub struct Plugin {
    pub dir: PathBuf,
    pub script: PathBuf,
    pub manifest: Manifest,
}

/// How a plugin will run: the environment, and the command up to the script.
pub type ResolvedEnv = std::result::Result<(EnvSpec, Vec<OsString>), Vec<String>>;

impl Plugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// The saved values of its settings, and `env` if the user chose one.
    pub fn saved(&self) -> BTreeMap<String, String> {
        crate::settings::shared().plugins.get(self.id()).cloned().unwrap_or_default()
    }

    /// The environment the user chose in Settings, if any.
    pub fn env_override(&self) -> Option<EnvSpec> {
        self.saved().get("env").and_then(|s| EnvSpec::parse_choice(s))
    }

    /// The environment to run in: the user's choice if made, else the manifest's first
    /// usable one.
    pub fn resolve_env(&self) -> ResolvedEnv {
        match self.env_override() {
            Some(spec) => env::resolve(&spec, &self.dir).map(|argv| (spec.clone(), argv)).map_err(|e| vec![format!("{}: {e}", spec.describe())]),
            None => env::first_usable(&self.manifest.env, &self.dir),
        }
    }

}

/// A plugin folder that couldn't be loaded, and why.
pub struct Problem {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Default)]
pub struct Registry {
    pub plugins: Vec<Arc<Plugin>>,
    pub problems: Vec<Problem>,
}

impl Global for Registry {}

/// `~/.config/plasmid-gpui/plugins`, next to settings.conf.
pub fn plugins_dir() -> Option<PathBuf> {
    Some(crate::settings::config_path()?.parent()?.join("plugins"))
}

/// Load every `<dir>/<id>/plugin.py`.
pub fn scan(dir: Option<&Path>) -> Registry {
    let mut reg = Registry::default();
    let Some(dir) = dir else { return reg };
    let Ok(entries) = std::fs::read_dir(dir) else { return reg };
    let mut folders: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    folders.sort();
    for folder in folders {
        let script = folder.join("plugin.py");
        if !script.is_file() {
            continue;
        }
        let loaded = std::fs::read_to_string(&script)
            .with_context(|| format!("reading {}", script.display()))
            .and_then(|src| manifest::parse(&src));
        match loaded {
            Ok(m) if reg.plugins.iter().any(|p| p.manifest.id == m.id) => {
                reg.problems.push(Problem { path: script, error: format!("another plugin already has the id {:?}", m.id) })
            }
            Ok(manifest) => reg.plugins.push(Arc::new(Plugin { dir: folder, script, manifest })),
            Err(e) => reg.problems.push(Problem { path: script, error: format!("{e:#}") }),
        }
    }
    reg
}

pub fn init(cx: &mut App) {
    cx.set_global(scan(plugins_dir().as_deref()));
}

pub fn rescan(cx: &mut App) {
    cx.set_global(scan(plugins_dir().as_deref()));
}


/// The plugin actions offered on a feature of `kind`: (plugin, action).
pub fn actions_for(kind: &str, cx: &App) -> Vec<(Arc<Plugin>, Action)> {
    let Some(reg) = cx.try_global::<Registry>() else { return Vec::new() };
    reg.plugins
        .iter()
        .flat_map(|p| p.manifest.actions.iter().filter(|a| a.offered_for(kind)).map(move |a| (p.clone(), a.clone())))
        .collect()
}

/// Write the bundled plugins into `dir`. A changed copy is kept as `plugin.py.bak`.
/// Returns what was installed or updated.
pub fn install_bundled(dir: &Path) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for (id, source) in BUNDLED {
        let folder = dir.join(id);
        let script = folder.join("plugin.py");
        std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
        match std::fs::read_to_string(&script) {
            Ok(old) if old == *source => continue,
            Ok(old) => {
                std::fs::write(folder.join("plugin.py.bak"), old)?;
                done.push(format!("{id} (updated; your copy is plugin.py.bak)"));
            }
            Err(_) => done.push(id.to_string()),
        }
        crate::model::write_atomic(&script, source.as_bytes())?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_plugins_have_valid_manifests() {
        for (id, src) in BUNDLED {
            let m = manifest::parse(src).unwrap_or_else(|e| panic!("{id}: {e:#}"));
            assert_eq!(m.id, *id);
        }
    }

    #[test]
    fn scans_and_installs() {
        let dir = std::env::temp_dir().join(format!("plasmid-plugins-scan-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("broken")).unwrap();
        std::fs::write(dir.join("broken/plugin.py"), "print('no manifest')").unwrap();
        let installed = install_bundled(&dir).unwrap();
        assert_eq!(installed.len(), BUNDLED.len());
        assert!(install_bundled(&dir).unwrap().is_empty(), "a second install changes nothing");
        let reg = scan(Some(&dir));
        assert_eq!(reg.plugins.len(), BUNDLED.len());
        assert_eq!(reg.problems.len(), 1);
        assert!(reg.problems[0].error.contains("script"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

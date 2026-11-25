//! Turning a plugin's environment spec into the command that runs its script.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::manifest::EnvSpec;

/// A program found on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// `~/x` and paths relative to the plugin's folder, made absolute.
pub fn expand(path: &str, plugin_dir: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        plugin_dir.join(p)
    }
}

/// The folder holding this app's flake, for `{flake}` in nix installables:
/// `$PLASMID_FLAKE`, else the source tree the app was built from if it's still there.
pub fn app_flake() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PLASMID_FLAKE").map(PathBuf::from).filter(|p| p.join("flake.nix").is_file()) {
        return Some(p);
    }
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    src.join("flake.nix").is_file().then_some(src)
}

fn conda() -> Option<PathBuf> {
    // conda run, or one of its drop-in replacements.
    ["conda", "mamba", "micromamba"].iter().find_map(|n| which(n)).or_else(|| {
        std::env::var_os("CONDA_EXE").map(PathBuf::from).filter(|p| is_executable(p))
    })
}

/// What's needed to run a plugin in `spec`, or why it can't be used here.
pub fn resolve(spec: &EnvSpec, plugin_dir: &Path) -> Result<Vec<OsString>, String> {
    let os = |s: &str| OsString::from(s);
    match spec {
        EnvSpec::Uv => {
            let uv = which("uv").ok_or("uv isn't on PATH (https://docs.astral.sh/uv/)")?;
            Ok(vec![uv.into(), os("run"), os("--script")])
        }
        EnvSpec::Venv { path } => {
            let dir = expand(path, plugin_dir);
            let py = ["bin/python", "bin/python3"].iter().map(|b| dir.join(b)).find(|p| is_executable(p));
            let py = py.ok_or_else(|| format!("no virtualenv at {}", dir.display()))?;
            Ok(vec![py.into()])
        }
        EnvSpec::Conda { name, prefix } => {
            let conda = conda().ok_or("conda (or mamba, micromamba) isn't on PATH")?;
            let mut argv = vec![conda.into_os_string(), os("run")];
            match (name, prefix) {
                (_, Some(p)) => {
                    let dir = expand(p, plugin_dir);
                    if !dir.join("conda-meta").is_dir() {
                        return Err(format!("no conda env at {}", dir.display()));
                    }
                    argv.extend([os("-p"), dir.into()]);
                }
                (Some(n), None) => argv.extend([os("-n"), os(n)]),
                (None, None) => return Err("a conda env needs a name or a prefix".into()),
            }
            argv.extend([os("--no-capture-output"), os("python")]);
            Ok(argv)
        }
        EnvSpec::Nix { installable } => {
            let nix = which("nix").ok_or("nix isn't on PATH")?;
            let installable = if installable.contains("{flake}") {
                let flake = app_flake().ok_or("{flake} needs PLASMID_FLAKE set to the app's source folder")?;
                installable.replace("{flake}", &flake.to_string_lossy())
            } else {
                installable.clone()
            };
            Ok(vec![
                nix.into(),
                os("--extra-experimental-features"),
                os("nix-command flakes"),
                os("shell"),
                os(&installable),
                os("-c"),
                os("python"),
            ])
        }
        EnvSpec::Python { path } => {
            let py = if path.contains('/') || path.starts_with('~') { Some(expand(path, plugin_dir)) } else { which(path) };
            let py = py.filter(|p| is_executable(p)).ok_or_else(|| format!("no Python at {path}"))?;
            Ok(vec![py.into()])
        }
    }
}

/// The first of `specs` usable here, with the command prefix; else every reason.
pub fn first_usable(specs: &[EnvSpec], plugin_dir: &Path) -> Result<(EnvSpec, Vec<OsString>), Vec<String>> {
    let mut why = Vec::new();
    for s in specs {
        match resolve(s, plugin_dir) {
            Ok(argv) => return Ok((s.clone(), argv)),
            Err(e) => why.push(format!("{}: {e}", s.describe())),
        }
    }
    if specs.is_empty() {
        why.push("the plugin names no environment".into());
    }
    Err(why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_envs_say_why() {
        let dir = Path::new("/nonexistent-plugin");
        let e = resolve(&EnvSpec::Venv { path: ".venv".into() }, dir).unwrap_err();
        assert!(e.contains("/nonexistent-plugin/.venv"), "{e}");
        let e = resolve(&EnvSpec::Python { path: "/no/such/python".into() }, dir).unwrap_err();
        assert!(e.contains("/no/such/python"));
        assert!(first_usable(&[], dir).is_err());
    }

    #[test]
    fn finds_sh_as_a_stand_in_interpreter() {
        // Any executable will do to check the lookup itself.
        let argv = resolve(&EnvSpec::Python { path: "sh".into() }, Path::new("/")).unwrap();
        assert!(argv[0].to_string_lossy().ends_with("/sh"));
    }
}

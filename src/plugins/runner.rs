//! Running a plugin: `<env> plugin.py run request.json` on a pty in a scratch folder,
//! shown live in the task's terminal, with machine-readable events read from a
//! separate file so the plugin's own output can be anything.

use std::ffi::OsString;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;

use crate::model::FeatureRecord;
use crate::tasks::{Output, TaskCtx, TaskRunner};
use crate::terminal::{pump_reader, PtyHandoff, TerminalView};

/// The helper module every plugin can `import edict`.
pub const HELPER: &str = include_str!("../../plugins/edict.py");

/// The exit status edict.py's memory guard stops with.
pub const MEMORY_GUARD_EXIT: u32 = 75;

/// A run's scratch folder under `~/.cache/edict/tasks/`; removed once nothing
/// (the worker, the result) needs it.
pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("edict"))
}

impl Scratch {
    pub fn create(plugin: &str, task: u64) -> Result<Arc<Scratch>> {
        let base = cache_dir().context("no HOME or XDG_CACHE_HOME for a scratch folder")?.join("tasks");
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let dir = base.join(format!("{plugin}-{}-{task}-{stamp}", std::process::id()));
        std::fs::create_dir_all(dir.join(".edict")).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Arc::new(Scratch(dir)))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// What a plugin's run gave back.
pub struct PluginOutput {
    pub result: Produced,
    /// Where it came from: the structure's Source ("ESMFold2-Fast, seed 0"), or the
    /// tool that found the features ("pLannotate v2.0.0").
    pub source: Option<String>,
    /// Shown on the result card: (label, value), e.g. ("pTM", 0.84).
    pub metrics: Vec<(String, f64)>,
    pub elapsed: Duration,
    /// Keeps the run's folder until the result is dropped.
    #[allow(dead_code)]
    pub workdir: Arc<Scratch>,
}

/// The result itself, of the kind the action's `output` names.
pub enum Produced {
    Structure {
        data: Vec<u8>,
        /// "mmcif" or "pdb".
        format: String,
        /// The protein chains, in input order ("A").
        chains: Vec<String>,
        /// Per-residue confidence, 0–100, if the plugin gave one.
        plddt: Vec<f32>,
    },
    /// Features in the coordinates of the bases sent (0 is the region's first base).
    Features(Vec<ProposedFeature>),
}

/// A feature a plugin found, which the user may add.
#[derive(Clone, Debug, Deserialize)]
pub struct ProposedFeature {
    /// `type`, `label`, `strand`, `segments` and `qualifiers`, as in the edit log.
    #[serde(flatten)]
    pub record: FeatureRecord,
    /// One line shown under it ("snapgene · 99.8% identity").
    #[serde(default)]
    pub detail: Option<String>,
    /// Whether it starts ticked.
    #[serde(default = "yes")]
    pub selected: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum Event {
    Progress {
        text: String,
        #[serde(default)]
        fraction: Option<f64>,
    },
    Log {
        text: String,
    },
    Warn {
        text: String,
    },
    Error {
        text: String,
    },
    Result(ResultEvent),
}

#[derive(Deserialize)]
struct ResultEvent {
    kind: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    features: Vec<ProposedFeature>,
    #[serde(default = "mmcif")]
    format: String,
    #[serde(default)]
    chains: Vec<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    metrics: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    plddt: Vec<f32>,
}

fn mmcif() -> String {
    "mmcif".into()
}

pub struct PluginRunner {
    /// The environment's command up to the script (`conda run -n x python`).
    pub env_argv: Vec<OsString>,
    pub script: PathBuf,
    pub request: serde_json::Value,
    /// The result kind the action promises: "structure" or "features".
    pub output: String,
    /// Keeps the run's folder until the result is dropped.
    #[allow(dead_code)]
    pub workdir: Arc<Scratch>,
    /// Where the pty goes once the process has started, for the task's terminal.
    pub pty_tx: async_channel::Sender<PtyHandoff>,
    pub grid: (u16, u16),
}

/// Reads complete lines appended to the events file since the last call.
struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn lines(&mut self) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return Vec::new() };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if let Ok(n) = f.read_to_end(&mut buf) {
            self.offset += n as u64;
        }
        self.partial.extend_from_slice(&buf);
        let mut out = Vec::new();
        while let Some(i) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            if !line.is_empty() {
                out.push(line);
            }
        }
        out
    }
}

/// Signal the run's whole process group (the pty made the child a session leader,
/// so its pid is the group id): conda run, nix shell and python all go together.
#[cfg(unix)]
fn signal_group(pid: u32, sig: i32) {
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}

/// Ask the kernel to choose this run first if memory runs out (Linux).
fn first_to_be_killed(pid: u32) {
    #[cfg(target_os = "linux")]
    std::fs::write(format!("/proc/{pid}/oom_score_adj"), "1000").ok();
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

impl TaskRunner for PluginRunner {
    fn service(&self) -> &'static str {
        // One plugin run at a time: they tend to want the whole GPU.
        "plugin"
    }

    fn run(self: Box<Self>, ctx: &TaskCtx) -> Result<Output> {
        let dir = self.workdir.path().to_path_buf();
        let meta = dir.join(".edict");
        let request_path = meta.join("request.json");
        let events_path = meta.join("events.jsonl");
        std::fs::write(&request_path, serde_json::to_vec_pretty(&self.request)?)?;
        std::fs::write(meta.join("edict.py"), HELPER)?;
        std::fs::write(&events_path, b"")?;

        let (cols, rows) = self.grid;
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let (program, rest) = self.env_argv.split_first().context("no command for the environment")?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(rest);
        cmd.arg(&self.script);
        cmd.arg("run");
        cmd.arg(&request_path);
        cmd.cwd(&dir);
        TerminalView::terminal_env(&mut cmd);
        let pythonpath = match std::env::var_os("PYTHONPATH").filter(|p| !p.is_empty()) {
            Some(old) => {
                let mut p = meta.clone().into_os_string();
                p.push(":");
                p.push(old);
                p
            }
            None => meta.clone().into_os_string(),
        };
        cmd.env("PYTHONPATH", pythonpath);
        cmd.env("PYTHONUNBUFFERED", "1");
        cmd.env("EDICT_EVENTS", &events_path);
        cmd.env("EDICT_REQUEST", &request_path);
        cmd.env("EDICT_WORKDIR", &dir);
        cmd.env("EDICT_API", super::manifest::API.to_string());

        ctx.log(format!("Running {}", shell_words(&self.env_argv, &self.script)));
        let mut child = pair.slave.spawn_command(cmd).context("couldn't start the plugin")?;
        drop(pair.slave);
        let pid = child.process_id();
        if let Some(pid) = pid {
            first_to_be_killed(pid);
        }
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let output = pump_reader(reader, "plugin-pty")?;
        self.pty_tx.send_blocking(PtyHandoff { master: pair.master, writer, killer: child.clone_killer(), output }).ok();

        let started = Instant::now();
        let mut tail = Tail { path: events_path, offset: 0, partial: Vec::new() };
        let mut result: Option<ResultEvent> = None;
        let mut last_error: Option<String> = None;
        let mut term_sent: Option<Instant> = None;
        let handle = |line: String, result: &mut Option<ResultEvent>, last_error: &mut Option<String>| {
            match serde_json::from_str::<Event>(&line) {
                Ok(Event::Progress { text, fraction }) => ctx.progress(match fraction {
                    Some(f) => format!("{text} · {:.0}%", (f * 100.0).clamp(0.0, 100.0)),
                    None => text,
                }),
                Ok(Event::Log { text }) => ctx.log(text),
                Ok(Event::Warn { text }) => ctx.warn(text),
                Ok(Event::Error { text }) => {
                    ctx.warn(&text);
                    *last_error = Some(text);
                }
                Ok(Event::Result(r)) => *result = Some(r),
                Err(e) => ctx.warn(format!("unreadable event from the plugin ({e}): {line}")),
            }
        };
        let status = loop {
            for line in tail.lines() {
                handle(line, &mut result, &mut last_error);
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if ctx.cancelled() {
                #[cfg(unix)]
                if let Some(pid) = pid {
                    match term_sent {
                        None => {
                            signal_group(pid, libc::SIGTERM);
                            term_sent = Some(Instant::now());
                        }
                        Some(t) if t.elapsed() > Duration::from_secs(5) => signal_group(pid, libc::SIGKILL),
                        _ => {}
                    }
                }
                #[cfg(not(unix))]
                if term_sent.is_none() {
                    child.kill().ok();
                    term_sent = Some(Instant::now());
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        // Whatever the script left running in its group goes with it.
        #[cfg(unix)]
        if let Some(pid) = pid {
            signal_group(pid, libc::SIGKILL);
        }
        for line in tail.lines() {
            handle(line, &mut result, &mut last_error);
        }
        let elapsed = started.elapsed();
        ctx.check()?;

        let code = status.exit_code();
        if code == MEMORY_GUARD_EXIT {
            bail!("{}", last_error.unwrap_or_else(|| "stopped by the memory guard".into()));
        }
        if !status.success() {
            let how = match status.signal() {
                Some(sig) => format!("was killed ({sig}){}", if sig.contains("Kill") { ", perhaps for running out of memory" } else { "" }),
                None => format!("exited with status {code}"),
            };
            match last_error {
                Some(e) => bail!("{e}"),
                None => bail!("the plugin {how}; its output above may say why"),
            }
        }
        let Some(r) = result else { bail!("the plugin finished without giving a result") };
        if r.kind != self.output {
            bail!("the plugin gave a {:?} result, but its action promises {:?}", r.kind, self.output);
        }
        let produced = match r.kind.as_str() {
            "structure" => {
                let format = r.format.to_ascii_lowercase();
                if !matches!(format.as_str(), "mmcif" | "pdb") {
                    bail!("the plugin gave a structure as {:?}; expected mmcif or pdb", r.format);
                }
                let Some(name) = r.file.as_deref() else { bail!("the plugin's structure result names no file") };
                let file = dir.join(name);
                let inside = file.canonicalize().ok().zip(dir.canonicalize().ok()).is_some_and(|(f, d)| f.starts_with(d));
                if !inside {
                    bail!("the result file {name:?} isn't in the run's folder");
                }
                let data = std::fs::read(&file).with_context(|| format!("reading the result {name}"))?;
                self.check_structure(&data, &format)?;
                Produced::Structure { data, format, chains: r.chains, plddt: r.plddt }
            }
            "features" => {
                self.check_features(&r.features)?;
                Produced::Features(r.features)
            }
            other => bail!("the plugin gave a {other:?} result, which this version doesn't take"),
        };
        let metrics = r.metrics.iter().filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v))).collect();
        ctx.log(format!("Finished in {:.1} s", elapsed.as_secs_f64()));
        Ok(Arc::new(PluginOutput { result: produced, source: r.source, metrics, elapsed, workdir: self.workdir.clone() }))
    }
}

impl PluginRunner {
    /// The structure must parse, and every protein sent must come back as one of its
    /// chains, residue for residue; otherwise it can't be linked to the feature.
    fn check_structure(&self, data: &[u8], format: &str) -> Result<()> {
        use crate::structure::{chain_letters, Molecule};
        let fmt = if format == "pdb" { crate::edna::StructureFormat::Pdb } else { crate::edna::StructureFormat::Mmcif };
        let mol = Molecule::parse(data, fmt).context("the plugin's structure doesn't parse")?;
        let chains: Vec<Vec<u8>> = (0..mol.chains.len()).map(|ci| chain_letters(&mol, ci).0.to_ascii_uppercase()).collect();
        let sent = self.request["input"]["sequences"].as_array().cloned().unwrap_or_default();
        for s in sent {
            let (id, seq) = (s["id"].as_str().unwrap_or("?"), s["seq"].as_str().unwrap_or_default());
            if !chains.iter().any(|c| c == seq.to_ascii_uppercase().as_bytes()) {
                bail!("the plugin's structure has no chain matching protein {id} ({} aa) as sent", seq.len());
            }
        }
        Ok(())
    }

    /// Every feature must lie within the bases sent, in the edit log's form: segments
    /// `[start, end)` in forward order (through the origin once, if the bases were
    /// sent as circular), a strand of -1, 0 or 1, and a type.
    fn check_features(&self, features: &[ProposedFeature]) -> Result<()> {
        let sent = &self.request["input"]["sequences"][0];
        let len = sent["seq"].as_str().map_or(0, str::len);
        let circular = sent["circular"].as_bool().unwrap_or(false);
        for (i, p) in features.iter().enumerate() {
            let f = &p.record;
            let which = || format!("feature {} ({} {:?})", i + 1, f.kind, f.label);
            if f.kind.trim().is_empty() {
                bail!("the plugin's feature {} has no type", i + 1);
            }
            if !(-1..=1).contains(&f.strand) {
                bail!("{}: strand {} isn't -1, 0 or 1", which(), f.strand);
            }
            if f.segments.is_empty() {
                bail!("{}: no segments", which());
            }
            let (first, mut last, mut wrapped) = (f.segments[0][0], 0, false);
            for &[s, e] in &f.segments {
                if s < last && circular && !wrapped {
                    wrapped = true;
                } else if s < last {
                    bail!("{}: segment [{s}, {e}) isn't in order within the {len} bases sent", which());
                }
                // Past the origin, it mustn't run back into its own start.
                if s >= e || e > len || (wrapped && e > first) {
                    bail!("{}: segment [{s}, {e}) isn't in order within the {len} bases sent", which());
                }
                last = e;
            }
        }
        Ok(())
    }
}

/// The command as a shell would show it, for the log.
pub fn shell_words(env: &[OsString], script: &Path) -> String {
    let quote = |s: &str| {
        if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@#{}".contains(c)) {
            s.to_string()
        } else {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
    };
    env.iter()
        .map(|a| quote(&a.to_string_lossy()))
        .chain([quote(&script.to_string_lossy()), "run".into(), "request.json".into()])
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{debug_msg, TaskCtx};

    fn run_script(script: &str, cancel_after: Option<Duration>) -> (Result<Output>, Vec<String>) {
        run_with(script, serde_json::json!({"api": 1, "input": {"sequences": [{"id": "A", "seq": "MKV"}]}}), cancel_after)
    }

    fn run_with(script: &str, request: serde_json::Value, cancel_after: Option<Duration>) -> (Result<Output>, Vec<String>) {
        run_for("structure", script, request, cancel_after)
    }

    fn run_for(output: &str, script: &str, request: serde_json::Value, cancel_after: Option<Duration>) -> (Result<Output>, Vec<String>) {
        let tmp = std::env::temp_dir().join(format!("edict-plugin-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(tmp.join(".edict")).unwrap();
        let path = tmp.join("plugin.py");
        std::fs::write(&path, script).unwrap();
        let Some(py) = super::super::env::which("python3").or_else(|| super::super::env::which("python")) else {
            eprintln!("no python on PATH; skipping");
            return (Err(anyhow::anyhow!("skipped")), vec!["skipped".into()]);
        };
        let (tx, rx) = async_channel::unbounded();
        let (pty_tx, pty_rx) = async_channel::unbounded();
        let runner = PluginRunner {
            env_argv: vec![py.into()],
            script: path,
            request,
            output: output.into(),
            workdir: Arc::new(Scratch(tmp)),
            pty_tx,
            grid: (80, 24),
        };
        let ctx = TaskCtx::for_test(tx);
        let ctx = Arc::new(ctx);
        if let Some(d) = cancel_after {
            let c = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(d);
                c.cancel_for_test();
            });
        }
        // Drain the pty so the child never blocks writing.
        std::thread::spawn(move || {
            if let Ok(h) = pty_rx.recv_blocking() {
                while h.output.recv_blocking().is_ok() {}
            }
        });
        let out = Box::new(runner).run(&ctx);
        drop(ctx);
        let msgs = std::iter::from_fn(|| rx.try_recv().ok()).map(|m| debug_msg(&m)).collect();
        (out, msgs)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    /// A plugin that "predicts" a three-residue CA trace of MKV, the protein `run_script` sends.
    const MKV_PLUGIN: &str = r#"
import edict, sys
CIF = """data_mkv
loop_
_atom_site.group_PDB
_atom_site.id
_atom_site.type_symbol
_atom_site.label_atom_id
_atom_site.label_comp_id
_atom_site.label_asym_id
_atom_site.label_entity_id
_atom_site.label_seq_id
_atom_site.Cartn_x
_atom_site.Cartn_y
_atom_site.Cartn_z
_atom_site.occupancy
_atom_site.B_iso_or_equiv
_atom_site.auth_seq_id
_atom_site.auth_asym_id
_atom_site.pdbx_PDB_model_num
ATOM 1 C CA MET A 1 1 0.0 0.0 0.0 1.00 90 1 A 1
ATOM 2 C CA LYS A 1 2 3.8 0.0 0.0 1.00 80 2 A 1
ATOM 3 C CA VAL A 1 3 7.6 0.0 0.0 1.00 70 3 A 1
"""
req = edict.request()
assert req["input"]["sequences"][0]["seq"] == "MKV"
print("hello from the plugin")
edict.progress("Folding", 0.5)
open("pred.cif", "w").write(CIF)
edict.result(kind="structure", file="pred.cif", format="mmcif", chains=["A"], metrics={"ptm": 0.8}, plddt=[90, 80, 70])
"#;

    #[test]
    fn a_plugin_gives_a_structure() {
        let (out, msgs) = run_script(MKV_PLUGIN, None);
        if msgs == ["skipped"] {
            return;
        }
        let out = out.unwrap().downcast::<PluginOutput>().ok().unwrap();
        let Produced::Structure { data, chains, plddt, .. } = &out.result else { panic!("not a structure") };
        assert!(data.starts_with(b"data_mkv"));
        assert_eq!(chains, &vec!["A"]);
        assert_eq!(out.metrics, vec![("ptm".to_string(), 0.8)]);
        assert_eq!(plddt, &vec![90.0, 80.0, 70.0]);
        assert!(msgs.iter().any(|m| m == "(Folding · 50%)"), "{msgs:?}");
        let dir = out.workdir.path().to_path_buf();
        drop(out);
        assert!(!dir.exists(), "the scratch folder goes with the result");
    }

    #[test]
    fn failures_say_why() {
        let (out, msgs) = run_script("import edict\nedict.fail('no GPU here')\n", None);
        if msgs == ["skipped"] {
            return;
        }
        assert_eq!(out.err().unwrap().to_string(), "no GPU here");
        let (out, _) = run_script("import sys\nsys.exit(3)\n", None);
        assert!(out.err().unwrap().to_string().contains("status 3"));
        let (out, _) = run_script("import edict\nedict.fail('memory guard: low', edict.MEMORY_GUARD_EXIT)\n", None);
        assert!(out.err().unwrap().to_string().contains("memory guard"));
        let (out, _) = run_script("import edict\nedict.result(kind='structure', file='../../etc/passwd')\n", None);
        assert!(out.err().unwrap().to_string().contains("isn't in the run's folder"));
        let (out, _) = run_script("pass\n", None);
        assert!(out.err().unwrap().to_string().contains("without giving a result"));
        // A structure of some other protein (MKL, not the MKV that was sent).
        let lying = MKV_PLUGIN.replace("CA VAL", "CA LEU");
        assert_ne!(lying, MKV_PLUGIN);
        let (out, _) = run_script(&lying, None);
        assert!(out.err().unwrap().to_string().contains("no chain matching protein A"));
    }

    /// The quick-start plugin in docs/plugins.md loads and works, so the guide stays true.
    #[test]
    fn the_guides_example_plugin_works() {
        let guide = include_str!("../../docs/plugins.md");
        let start = guide.find("   ```python\n").expect("the guide's python block") + "   ```python\n".len();
        let len = guide[start..].find("\n   ```\n").expect("the end of that block");
        let script: String = guide[start..start + len].lines().map(|l| format!("{}\n", l.strip_prefix("   ").unwrap_or(l))).collect();
        let manifest = super::super::manifest::parse(&script).unwrap_or_else(|e| panic!("{e:#}"));
        assert_eq!(manifest.id, "ca-helix");
        let protein = "MSKGEELFTGVVPILVELDGDVNGHKFSVSGEGEGDATYGKLTLKFICTT";
        let request = serde_json::json!({
            "api": 1,
            "action": "build",
            "settings": {"rise": 1.5},
            "input": {"sequences": [{"id": "A", "seq": protein}], "feature": {"label": "GFP", "kind": "CDS"}, "doc_name": "test"},
        });
        let (out, msgs) = run_with(&script, request, None);
        if msgs == ["skipped"] {
            return;
        }
        let out = out.unwrap().downcast::<PluginOutput>().ok().unwrap();
        assert!(matches!(&out.result, Produced::Structure { chains, .. } if chains == &vec!["A"]));
        assert!(out.source.as_deref().is_some_and(|s| s.contains("1.5")));
    }

    fn run_features(script: &str) -> (Result<Output>, Vec<String>) {
        let request = serde_json::json!({"api": 1, "input": {"sequences": [{"id": "dna", "seq": "ACGTACGTAC", "circular": false}]}});
        run_for("features", script, request, None)
    }

    #[test]
    fn a_plugin_gives_features() {
        let script = r#"
import edict
edict.result(kind="features", source="Toy 1", features=[
    {"type": "promoter", "label": "P1", "strand": 1, "segments": [[0, 4]], "qualifiers": [["note", "strong"]], "detail": "99% identity"},
    {"type": "CDS", "label": "orf", "strand": -1, "segments": [[2, 5], [6, 10]], "selected": False},
])
"#;
        let (out, msgs) = run_features(script);
        if msgs == ["skipped"] {
            return;
        }
        let out = out.unwrap_or_else(|e| panic!("{e:#}")).downcast::<PluginOutput>().ok().unwrap();
        let Produced::Features(fs) = &out.result else { panic!("not features") };
        assert_eq!(fs.len(), 2);
        assert_eq!(fs[0].record.qualifiers, vec![("note".to_string(), Some("strong".to_string()))]);
        assert_eq!(fs[0].detail.as_deref(), Some("99% identity"));
        assert!(fs[0].selected && !fs[1].selected);
        assert_eq!(fs[1].record.segments, vec![[2, 5], [6, 10]]);
        assert_eq!(out.source.as_deref(), Some("Toy 1"));
    }

    #[test]
    fn bad_features_are_refused() {
        let give = |f: &str| format!("import edict\nedict.result(kind='features', features=[{f}])\n");
        for (f, why) in [
            ("{'type': 'CDS', 'label': 'x', 'segments': [[0, 11]]}", "within the 10 bases"),
            ("{'type': 'CDS', 'label': 'x', 'segments': [[4, 4]]}", "within the 10 bases"),
            ("{'type': 'CDS', 'label': 'x', 'segments': [[5, 8], [0, 2]]}", "in order"),
            ("{'type': 'CDS', 'label': 'x', 'segments': []}", "no segments"),
            ("{'type': '', 'label': 'x', 'segments': [[0, 2]]}", "no type"),
            ("{'type': 'CDS', 'label': 'x', 'strand': 2, 'segments': [[0, 2]]}", "strand"),
        ] {
            let (out, msgs) = run_features(&give(f));
            if msgs == ["skipped"] {
                return;
            }
            let e = out.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(e.contains(why), "{f}: {e}");
        }
        // Through the origin only on circular DNA, and only once.
        let across = "{'type': 'rep_origin', 'label': 'ori', 'segments': [[8, 10], [0, 2]]}";
        let (out, _) = run_features(&give(across));
        assert!(out.is_err());
        let request = serde_json::json!({"api": 1, "input": {"sequences": [{"id": "dna", "seq": "ACGTACGTAC", "circular": true}]}});
        let (out, _) = run_for("features", &give(across), request.clone(), None);
        assert!(out.is_ok());
        let twice = "{'type': 'x', 'label': 'x', 'segments': [[8, 10], [0, 2], [1, 3]]}";
        assert!(run_for("features", &give(twice), request.clone(), None).0.is_err());
        let overlapping = "{'type': 'x', 'label': 'x', 'segments': [[5, 10], [0, 7]]}";
        assert!(run_for("features", &give(overlapping), request, None).0.is_err());
        // A structure where features were promised.
        let (out, _) = run_features("import edict\nedict.result(kind='structure', file='x.cif')\n");
        assert!(out.err().unwrap().to_string().contains("promises"));
    }

    #[test]
    fn cancel_stops_the_whole_group() {
        let began = Instant::now();
        // A grandchild process too: the group signal reaches it, so the pty closes.
        let (out, msgs) = run_script("import subprocess, time\nsubprocess.Popen(['sleep', '60'])\ntime.sleep(60)\n", Some(Duration::from_millis(500)));
        if msgs == ["skipped"] {
            return;
        }
        assert!(out.is_err());
        assert!(began.elapsed() < Duration::from_secs(10), "took {:?}", began.elapsed());
    }
}

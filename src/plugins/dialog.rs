//! A plugin task's view: the plugin's settings, then its terminal while it runs (it
//! can be backgrounded like a BLAST search), then the structure it made, which can
//! be attached to the feature it was run on.

use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, Subscription, WeakEntity, Window};

use super::form::{FormEvent, SettingsForm};
use super::manifest::Action;
use super::runner::{PluginOutput, PluginRunner, Scratch};
use super::{Plugin, ResolvedEnv};
use crate::editor::{thousands, EditorEvent, SeqEditor};
use crate::model::Feature;
use crate::pgx::{self, ChainLinking, StructureFormat};
use crate::settings;
use crate::tasks::{self, TaskId, TaskStatus};
use crate::terminal::TerminalView;
use crate::theme;

/// What the plugin was opened on: one coding feature and its protein.
#[derive(Clone)]
pub struct Target {
    pub feature_ix: usize,
    pub feature: Feature,
    pub protein: String,
    pub doc_name: String,
}

impl Target {
    /// The protein a coding feature encodes: no trailing stop, and no internal ones.
    pub fn protein(seq: &[u8], f: &Feature) -> Result<String, String> {
        let (aa, _) = crate::genetic_code::translate(seq, f);
        let mut aa = String::from_utf8_lossy(&aa).into_owned();
        while aa.ends_with('*') {
            aa.pop();
        }
        if aa.is_empty() {
            return Err("The feature doesn't encode any amino acids".into());
        }
        if let Some(i) = aa.find('*') {
            return Err(format!("The feature has a stop codon at residue {} of {}", i + 1, aa.len()));
        }
        Ok(aa)
    }

    /// Where the feature is now: edits may have moved it in the list.
    fn find(&self, doc: &crate::model::Document) -> Option<usize> {
        let f = &self.feature;
        let same = |g: &Feature| g.kind == f.kind && g.segments == f.segments && g.strand == f.strand;
        if doc.features.get(self.feature_ix).is_some_and(same) {
            Some(self.feature_ix)
        } else {
            doc.features.iter().position(same)
        }
    }

    /// The bases to lock while the plugin runs, as `(start, end)`, wrapping through the
    /// origin when `start > end`: the feature's span, leaving out the widest gap between
    /// its segments (on a circular sequence, possibly the one across the origin).
    fn lock_span(&self, len: usize, circular: bool) -> Option<(usize, usize)> {
        let mut segs = self.feature.segments.clone();
        segs.sort_unstable();
        let (first, last) = (*segs.first()?, *segs.iter().max_by_key(|s| s.1)?);
        let mut best = (if circular { len - last.1 + first.0 } else { usize::MAX }, (first.0, last.1));
        for w in segs.windows(2) {
            let gap = w[1].0.saturating_sub(w[0].1);
            if circular && gap > best.0 {
                best = (gap, (w[1].0, w[0].1));
            }
        }
        Some(best.1)
    }

    fn name(&self) -> String {
        if self.feature.label.trim().is_empty() { self.feature.kind.clone() } else { self.feature.label.clone() }
    }
}

pub struct PluginView {
    id: TaskId,
    editor: WeakEntity<SeqEditor>,
    plugin: Arc<Plugin>,
    action: Action,
    target: Target,
    env: ResolvedEnv,
    form: Entity<SettingsForm>,
    terminal: Entity<TerminalView>,
    error: Option<String>,
    note: Option<String>,
    /// The structure id once attached.
    attached: Option<String>,
    show_output: bool,
    _subs: Vec<Subscription>,
}

impl PluginView {
    pub fn new(id: TaskId, editor: WeakEntity<SeqEditor>, plugin: Arc<Plugin>, action: Action, target: Target, cx: &mut Context<Self>) -> Self {
        let settings: Vec<_> = plugin.manifest.settings.iter().filter(|s| s.applies_to(&action.id)).cloned().collect();
        let saved = plugin.saved();
        let form = cx.new(|cx| SettingsForm::new(settings, &saved, cx));
        let terminal = cx.new(TerminalView::output);
        let subs = vec![
            cx.observe(&tasks::manager(cx), |this: &mut Self, _, cx| {
                // Mark the end in the terminal's corner, once.
                let ended = match this.status(cx) {
                    TaskStatus::Done => Some("Finished"),
                    TaskStatus::Failed(_) => Some("Failed"),
                    TaskStatus::Cancelled => Some("Stopped"),
                    _ => None,
                };
                if let Some(s) = ended.filter(|_| this.terminal.read(cx).status_is_none()) {
                    this.terminal.update(cx, |t, cx| t.set_status(Some(s.into()), cx));
                }
                cx.notify()
            }),
            cx.subscribe(&form, |this, _, ev: &FormEvent, cx| match ev {
                FormEvent::Submit => this.run(cx),
            }),
        ];
        let env = plugin.resolve_env();
        PluginView {
            id,
            editor,
            plugin,
            action,
            target,
            env,
            form,
            terminal,
            error: None,
            note: None,
            attached: None,
            show_output: false,
            _subs: subs,
        }
    }

    fn status(&self, cx: &gpui::App) -> TaskStatus {
        tasks::manager(cx).read(cx).task(self.id).map(|t| t.status.clone()).unwrap_or(TaskStatus::Cancelled)
    }

    fn output(&self, cx: &gpui::App) -> Option<Arc<PluginOutput>> {
        tasks::manager(cx).read(cx).task(self.id)?.output.clone()?.downcast::<PluginOutput>().ok()
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.status(cx) != TaskStatus::Draft {
            return;
        }
        self.error = None;
        // The feature, as it is now: it is locked from here until the result is
        // dismissed, so the structure still matches it when attached.
        let span = self.editor.upgrade().map(|ed| {
            let doc = &ed.read(cx).doc;
            match self.target.find(doc).map(|ix| Target::protein(&doc.seq, &doc.features[ix])) {
                Some(Ok(p)) if p == self.target.protein => self.target.lock_span(doc.len(), doc.circular).ok_or(()),
                _ => Err(()),
            }
        });
        let span = match span {
            Some(Ok(span)) => span,
            None => {
                self.error = Some("The sequence's tab has closed".into());
                return cx.notify();
            }
            Some(Err(())) => {
                self.error = Some(format!("{} has changed since this dialog opened. Close it and run the plugin again.", self.target.name()));
                return cx.notify();
            }
        };
        let argv = match &self.env {
            Ok((_, argv)) => argv.clone(),
            Err(why) => {
                self.error = Some(format!("No environment to run {} in: {}", self.plugin.manifest.name, why.join("; ")));
                return cx.notify();
            }
        };
        let (values, texts) = match self.form.update(cx, |f, cx| f.values(cx)) {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                return cx.notify();
            }
        };
        let pid = self.plugin.id().to_string();
        if let Err(e) = settings::update_shared(|s| s.plugins.entry(pid.clone()).or_default().extend(texts)) {
            eprintln!("couldn't save the plugin's settings: {e:#}");
        }
        let workdir = match Scratch::create(&pid, self.id as u64) {
            Ok(w) => w,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return cx.notify();
            }
        };
        let request = serde_json::json!({
            "api": super::manifest::API,
            "action": self.action.id,
            "settings": values,
            "input": {
                "sequences": [{"id": "A", "seq": self.target.protein}],
                "feature": {"label": self.target.name(), "kind": self.target.feature.kind},
                "doc_name": self.target.doc_name,
            },
            "workdir": workdir.path(),
        });

        let (pty_tx, pty_rx) = async_channel::bounded(1);
        let terminal = self.terminal.downgrade();
        let command = format!("$ {}\n", super::runner::shell_words(&argv, &self.plugin.script));
        cx.spawn(async move |_, cx| {
            if let Ok(pty) = pty_rx.recv().await {
                terminal
                    .update(cx, |t, cx| {
                        t.print(&command, cx);
                        t.attach(pty, cx)
                    })
                    .ok();
            }
        })
        .detach();
        let grid = self.terminal.read(cx).grid();
        let runner = PluginRunner { env_argv: argv, script: self.plugin.script.clone(), request, workdir, pty_tx, grid };
        let title = format!("{} · {}", self.action.label.trim_end_matches('…'), self.target.name());
        let id = self.id;
        let kind = self.plugin.manifest.name.clone();
        let r = tasks::manager(cx).update(cx, |m, cx| {
            m.set_title(id, kind, title, cx);
            m.submit(id, Box::new(runner), Some(span), cx)
        });
        self.error = r.err().map(|e| format!("{e:#}"));
        cx.notify();
    }

    /// Add the structure to the document and link the feature's chain to it.
    fn attach(&mut self, cx: &mut Context<Self>) {
        let Some(out) = self.output(cx) else { return };
        let Some(editor) = self.editor.upgrade() else {
            self.error = Some("The sequence's tab has closed".into());
            return cx.notify();
        };
        // Find the feature again: edits may have moved it in the list.
        let found = {
            let doc = &editor.read(cx).doc;
            self.target.find(doc).map(|ix| (ix, Target::protein(&doc.seq, &doc.features[ix])))
        };
        let ix = match found {
            None => {
                self.error = Some(format!("{} is no longer in the document", self.target.name()));
                return cx.notify();
            }
            Some((_, Err(e))) => {
                self.error = Some(e);
                return cx.notify();
            }
            Some((_, Ok(p))) if p != self.target.protein => {
                self.error = Some(format!(
                    "{}'s protein has changed since the prediction, so the structure no longer matches it. Save the .cif instead, or run again.",
                    self.target.name()
                ));
                return cx.notify();
            }
            Some((ix, Ok(_))) => ix,
        };
        let format = if out.format == "pdb" { StructureFormat::Pdb } else { StructureFormat::Mmcif };
        let stem = format!("{}_{}", self.target.name(), self.plugin.id());
        let name = format!("{} ({})", self.target.name(), self.plugin.manifest.name);
        let source = out.source.clone().unwrap_or_else(|| self.plugin.manifest.name.clone());
        let chain = out.chains.first().cloned().unwrap_or_else(|| "A".into());
        let sid = editor.update(cx, |ed, cx| {
            let id = pgx::structure_id(&stem, &ed.doc.structures);
            ed.doc.add_structure(pgx::Structure {
                id: id.clone(),
                name,
                format,
                source: Some(source),
                data: out.data.clone().into(),
                linking: ChainLinking::Features,
            });
            let mut f = ed.doc.features[ix].clone();
            f.set_structure_chains(&id, &[chain]);
            ed.doc.edit_features(vec![(ix, f)], vec![]);
            cx.emit(EditorEvent::OpenStructure(id.clone()));
            cx.notify();
            id
        });
        self.attached = Some(sid);
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(out) = self.output(cx) else { return };
        let dir = self
            .editor
            .upgrade()
            .and_then(|e| e.read(cx).doc.path.parent().map(|p| p.to_path_buf()))
            .filter(|p| p.is_dir())
            .or_else(|| std::env::var_os("HOME").map(Into::into))
            .unwrap_or_else(|| ".".into());
        let ext = if out.format == "pdb" { "pdb" } else { "cif" };
        let suggested = format!("{}.{ext}", self.target.name().replace(['/', '\\'], "_"));
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => match crate::model::write_atomic(&path, &out.data) {
                        Ok(()) => this.note = Some(format!("Saved {}", path.display())),
                        Err(e) => this.error = Some(format!("Couldn't save: {e:#}")),
                    },
                    Ok(Err(e)) => this.error = Some(format!("Couldn't open the save dialog: {e:#}")),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----- rendering -----

    fn env_line(&self) -> AnyElement {
        let (text, bad) = match &self.env {
            Ok((spec, _)) => (format!("Runs in {}", spec.describe()), false),
            Err(why) => (format!("No usable environment. Tried: {}", why.join("; ")), true),
        };
        div().text_xs().text_color(if bad { theme::error() } else { theme::muted_text() }).child(text).into_any_element()
    }

    fn terminal(&self, tall: bool) -> AnyElement {
        div()
            .h(px(if tall { 340.0 } else { 220.0 }))
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(self.terminal.clone())
            .into_any_element()
    }

    fn result_card(&self, out: &PluginOutput) -> AnyElement {
        let metric = |label: String, value: String| {
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(theme::muted_text()).child(label))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(value))
        };
        let mut metrics = div().flex().flex_row().gap_6().child(metric("Residues".into(), thousands(self.target.protein.len())));
        for (k, v) in &out.metrics {
            metrics = metrics.child(metric(k.clone(), if *v <= 1.0 { format!("{v:.2}") } else { format!("{v:.1}") }));
        }
        metrics = metrics.child(metric("Time".into(), format!("{:.0} s", out.elapsed.as_secs_f64())));
        let mut d = div().flex().flex_col().gap_3().p_3().rounded_md().border_1().border_color(theme::border()).bg(theme::editor_bg()).child(metrics);
        if !out.plddt.is_empty() {
            d = d.child(plddt_strip(&out.plddt)).child(
                div().flex().flex_row().gap_3().text_xs().text_color(theme::muted_text()).children(
                    PLDDT_BANDS.iter().map(|&(_, c, l)| div().flex().flex_row().items_center().gap_1().child(div().size(px(8.0)).bg(gpui::rgb(c))).child(l)),
                ),
            );
        }
        d.into_any_element()
    }
}

/// AlphaFold's confidence colours: (lower bound, colour, legend).
const PLDDT_BANDS: [(f32, u32, &str); 4] =
    [(90.0, 0x0053d6, "Very high (>90)"), (70.0, 0x65cbf3, "Confident"), (50.0, 0xffdb13, "Low"), (f32::MIN, 0xff7d45, "Very low (<50)")];

fn plddt_color(v: f32) -> u32 {
    PLDDT_BANDS.iter().find(|(lo, _, _)| v > *lo).map(|b| b.1).unwrap_or(0xff7d45)
}

/// The confidence along the chain, one bar per residue (binned when long).
fn plddt_strip(values: &[f32]) -> AnyElement {
    let bins = values.len().min(300);
    let bars = (0..bins).map(|b| {
        let (lo, hi) = (b * values.len() / bins, ((b + 1) * values.len() / bins).max(b * values.len() / bins + 1));
        let mean = values[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
        div().flex_1().h_full().bg(gpui::rgb(plddt_color(mean)))
    });
    div().h(px(14.0)).w_full().flex().flex_row().rounded_sm().overflow_hidden().children(bars).into_any_element()
}

impl Render for PluginView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let manager = tasks::manager(cx);
        let (status, title, progress, ahead) = {
            let m = manager.read(cx);
            match m.task(self.id) {
                Some(t) => (t.status.clone(), t.title.clone(), t.progress.clone(), m.ahead("plugin")),
                None => (TaskStatus::Cancelled, String::new(), None, 0),
            }
        };
        let id = self.id;
        let button = |bid: &'static str, text: String, primary: bool| {
            div()
                .id(bid)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .border_1()
                .cursor_pointer()
                .when(primary, |d| d.border_color(theme::accent()).bg(theme::accent()).text_color(theme::editor_bg()))
                .when(!primary, |d| d.border_color(theme::border()).hover(|s| s.bg(theme::hover())))
                .child(text)
        };
        let background = || button("background", "Background".into(), false).on_click(|_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.background(cx)));
        let dismiss = |text: &str| button("dismiss", text.into(), false).on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.dismiss(id, cx)));

        let mut footer = div().flex().flex_row().items_center().gap_2();
        let body: AnyElement = match &status {
            TaskStatus::Draft => {
                let can_run = self.env.is_ok();
                footer = footer
                    .child(div().flex_1().min_w_0().child(self.env_line()))
                    .child(button("cancel", "Cancel".into(), false).on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.dismiss(id, cx))))
                    .when(can_run, |f| {
                        f.child(button("run", if ahead > 0 { "Queue".into() } else { "Run".into() }, true).on_click(cx.listener(|this, _, _, cx| this.run(cx))))
                    });
                let mut d = div().flex().flex_col().gap_3();
                if let Some(desc) = self.action.description.as_ref().or(self.plugin.manifest.description.as_ref()) {
                    d = d.child(div().text_xs().text_color(theme::muted_text()).child(desc.trim().replace('\n', " ")));
                }
                d = d.child(
                    div()
                        .text_sm()
                        .child(format!("{} · {} aa protein from {} of {}", self.target.feature.kind, thousands(self.target.protein.len()), self.target.name(), self.target.doc_name)),
                );
                if !self.form.read(cx).is_empty() {
                    d = d.child(self.form.clone());
                }
                if ahead > 0 {
                    d = d.child(div().text_xs().text_color(theme::muted_text()).child(format!(
                        "{ahead} plugin run{} ahead; this one will wait its turn.",
                        if ahead == 1 { "" } else { "s" }
                    )));
                }
                d.into_any_element()
            }
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling => {
                let cancelling = status == TaskStatus::Cancelling;
                footer = footer
                    .child(div().flex_1())
                    .when(!cancelling, |f| f.child(button("cancel", "Cancel".into(), false).on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.cancel(id, cx)))))
                    .child(background());
                let line = match &status {
                    TaskStatus::Queued => format!("Queued: waiting for {ahead} plugin run{} ahead", if ahead == 1 { "" } else { "s" }),
                    TaskStatus::Cancelling => "Cancelling…".into(),
                    _ => progress.unwrap_or_else(|| "Starting…".into()),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(line))
                    .child(self.terminal(true))
                    .child(div().text_xs().text_color(theme::muted_text()).child("You can background this and keep working; the sidebar shows when it's done."))
                    .into_any_element()
            }
            TaskStatus::Done => {
                let out = self.output(cx);
                footer = footer
                    .child(
                        div()
                            .id("toggle-output")
                            .text_xs()
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .child(if self.show_output { "Hide output" } else { "Show output" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_output ^= true;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(dismiss("Dismiss"))
                    .child(button("save", "Save file…".into(), false).on_click(cx.listener(|this, _, _, cx| this.save(cx))));
                footer = match &self.attached {
                    None => footer.child(button("attach", format!("Attach to {}", self.target.name()), true).on_click(cx.listener(|this, _, _, cx| this.attach(cx)))),
                    Some(_) => footer.child(button("close", "Close".into(), true).on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.dismiss(id, cx)))),
                };
                let mut d = div().flex().flex_col().gap_3();
                if let Some(out) = &out {
                    d = d.child(self.result_card(out));
                }
                if let Some(sid) = &self.attached {
                    d = d.child(div().text_sm().child(format!("Attached as structure {sid}; {}'s chain A is linked to it.", self.target.name())));
                } else {
                    d = d.child(div().text_xs().text_color(theme::muted_text()).child(
                        "Attach adds the structure to this document and links the feature to its chain. Nothing is added until you do.",
                    ));
                }
                if self.show_output {
                    d = d.child(self.terminal(false));
                }
                d.into_any_element()
            }
            TaskStatus::Failed(why) => {
                footer = footer.child(div().flex_1()).child(dismiss("Dismiss"));
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().text_color(theme::error()).child(format!("{} failed: {why}", self.plugin.manifest.name)))
                    .child(self.terminal(true))
                    .into_any_element()
            }
            TaskStatus::Cancelled => {
                footer = footer.child(div().flex_1()).child(dismiss("Dismiss"));
                div().flex().flex_col().gap_2().child(div().text_sm().child("Cancelled.")).child(self.terminal(true)).into_any_element()
            }
        };

        div()
            .id("plugin-dialog")
            .w(px(760.0))
            .max_w(gpui::relative(0.95))
            .max_h(gpui::relative(0.92))
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .text_color(theme::text())
            .shadow_lg()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(self.plugin.manifest.name.clone()))
                    .child(
                        div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().text_color(theme::muted_text()).child(
                            if matches!(status, TaskStatus::Draft) { self.action.label.trim_end_matches('…').to_string() } else { title },
                        ),
                    )
                    .child(div().text_xs().text_color(theme::muted_text()).child(status.label())),
            )
            .child(
                div()
                    .id("plugin-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .py_3()
                    .child(body)
                    .children(self.note.clone().map(|n| div().pt_2().text_sm().text_color(theme::muted_text()).child(n)))
                    .children(self.error.clone().map(|e| div().pt_2().text_sm().text_color(theme::error()).child(e))),
            )
            .child(div().px_4().py_3().border_t_1().border_color(theme::border()).child(footer))
    }
}

#[cfg(test)]
mod tests {
    use super::Target;
    use crate::model::Feature;

    fn span(segments: Vec<(usize, usize)>, len: usize, circular: bool) -> Option<(usize, usize)> {
        let feature = Feature { kind: "CDS".into(), label: String::new(), segments, strand: 1, source: None };
        Target { feature_ix: 0, feature, protein: String::new(), doc_name: String::new() }.lock_span(len, circular)
    }

    #[test]
    fn the_lock_covers_the_feature() {
        assert_eq!(span(vec![(100, 400)], 1000, true), Some((100, 400)));
        // Across the origin, in either order: a wrapping lock, not the whole plasmid.
        assert_eq!(span(vec![(900, 1000), (0, 50)], 1000, true), Some((900, 50)));
        assert_eq!(span(vec![(0, 50), (900, 1000)], 1000, true), Some((900, 50)));
        // A join keeps its intron locked too: those bases are inside the feature.
        assert_eq!(span(vec![(100, 200), (300, 400)], 1000, true), Some((100, 400)));
        // On a linear sequence nothing wraps.
        assert_eq!(span(vec![(0, 50), (900, 1000)], 1000, false), Some((0, 1000)));
    }
}

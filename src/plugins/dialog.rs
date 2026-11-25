//! A plugin task's view: the plugin's settings, then its terminal while it runs (it
//! can be backgrounded like a BLAST search), then what it made: a structure, which
//! can be attached to the feature it was run on, or features found in a region,
//! which the user ticks and adds.

use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, Subscription, WeakEntity, Window};

use super::form::{FormEvent, SettingsForm};
use super::manifest::Action;
use super::runner::{PluginOutput, PluginRunner, Produced, ProposedFeature, Scratch};
use super::{Plugin, ResolvedEnv};
use crate::editor::{thousands, EditorEvent, SeqEditor, Selection};
use crate::model::{Document, Feature};
use crate::edna::{self, ChainLinking, StructureFormat};
use crate::map::{self, MapMode, Tone};
use crate::settings;
use crate::tasks::{self, TaskId, TaskStatus};
use crate::terminal::TerminalView;
use crate::theme;

/// What the plugin was opened on.
#[derive(Clone)]
pub enum Subject {
    Feature(Target),
    Region(Region),
}

impl Subject {
    fn name(&self) -> String {
        match self {
            Subject::Feature(t) => t.name(),
            Subject::Region(r) => r.name(),
        }
    }
}

/// A stretch of DNA: the selection, or the whole sequence.
#[derive(Clone)]
pub struct Region {
    /// Where it was when the dialog opened: `[start, end)`, through the origin when
    /// `start > end`. The whole sequence is `0..len`.
    pub start: usize,
    pub end: usize,
    /// All of the sequence (nothing was selected, or everything was).
    pub whole: bool,
    /// Sent as circular DNA: the whole of a circular sequence.
    pub circular: bool,
    pub bases: Vec<u8>,
    pub doc_name: String,
}

impl Region {
    /// The region of `doc` the selection `sel` names, or all of it when empty.
    pub fn of(doc: &Document, sel: Selection) -> Region {
        let len = doc.len();
        let whole = sel.is_empty() || sel.bp(len) == len;
        let (start, end) = if whole { (0, len) } else { (sel.start, sel.end) };
        let bases = Selection { start, end }.bases(&doc.seq);
        Region { start, end, whole, circular: whole && doc.circular, bases, doc_name: doc.name.clone() }
    }

    /// Its bases are still where they were.
    fn unchanged(&self, doc: &Document) -> bool {
        doc.len() >= self.end.max(self.start) && Selection { start: self.start, end: self.end }.bases(&doc.seq) == self.bases
    }

    fn lock_span(&self) -> (usize, usize) {
        (self.start, self.end)
    }

    fn name(&self) -> String {
        if self.whole {
            self.doc_name.clone()
        } else {
            format!("{}–{} of {}", thousands(self.start + 1), thousands(self.end), self.doc_name)
        }
    }
}

/// Region coordinates (0 is the region's first base) to the document's, given where
/// the region's first base is now: through the origin where they pass its end, and
/// with segments that meet joined.
fn place(segments: &[[usize; 2]], offset: usize, len: usize) -> Vec<(usize, usize)> {
    let mut pieces = Vec::new();
    for &[s, e] in segments {
        let (a, b) = (s + offset, e + offset);
        if b <= len {
            pieces.push((a, b));
        } else if a >= len {
            pieces.push((a - len, b - len));
        } else {
            pieces.extend([(a, len), (0, b - len)]);
        }
    }
    merged(pieces)
}

/// Segments with the ones that meet joined into one.
fn merged(segments: impl IntoIterator<Item = (usize, usize)>) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in segments {
        match out.last_mut() {
            Some(last) if last.1 == s => last.1 = e,
            _ => out.push((s, e)),
        }
    }
    out
}

/// A feature already in the document covers the same bases on the same strand, and
/// has the same type or the same name.
fn already_there(existing: &[Feature], p: &ProposedFeature, segments: &[(usize, usize)]) -> bool {
    let (kind, label) = (p.record.kind.trim(), p.record.label.trim());
    existing.iter().any(|g| {
        g.strand == p.record.strand
            && merged(g.segments.iter().copied()) == segments
            && (g.kind.trim().eq_ignore_ascii_case(kind) || (!label.is_empty() && g.label.trim().eq_ignore_ascii_case(label)))
    })
}

/// "1,201–2,061 →": 1-based, inclusive, with the strand.
fn location(segments: &[(usize, usize)], strand: i8) -> String {
    let spans: Vec<String> = segments.iter().map(|&(s, e)| format!("{}–{}", thousands(s + 1), thousands(e))).collect();
    let arrow = match strand {
        1 => " →",
        -1 => " ←",
        _ => "",
    };
    format!("{}{arrow}", spans.join(", "))
}

/// The features a run found, and which the user has ticked.
struct Picks {
    /// Where each would go in the document (as of the result), for display.
    segments: Vec<Vec<(usize, usize)>>,
    /// Already in the document.
    dup: Vec<bool>,
    on: Vec<bool>,
    show_dups: bool,
    /// The map of what was found: the document's own features for context, and
    /// where each feature found goes, in the map's coordinates.
    map: MapView,
}

/// What the result's map shows: the whole sequence in its own shape, or just the
/// region, drawn linear.
struct MapView {
    mode: MapMode,
    len: usize,
    name: String,
    context: Vec<Feature>,
    found: Vec<Vec<(usize, usize)>>,
}

impl MapView {
    fn new(r: &Region, doc: &Document, found: &[ProposedFeature], segments: &[Vec<(usize, usize)>], offset: usize) -> MapView {
        if r.whole {
            return MapView {
                mode: if doc.circular { MapMode::Circular } else { MapMode::Linear },
                len: doc.len(),
                name: doc.name.clone(),
                context: doc.features.to_vec(),
                found: segments.to_vec(),
            };
        }
        let rlen = r.bases.len();
        let context = doc
            .features
            .iter()
            .filter_map(|f| {
                let segments = map::clip_to_region(&f.segments, offset, rlen, doc.len());
                (!segments.is_empty()).then(|| Feature { segments, ..f.clone() })
            })
            .collect();
        let found = found.iter().map(|p| p.record.segments.iter().map(|g| (g[0], g[1])).collect()).collect();
        MapView { mode: MapMode::Linear, len: rlen, name: r.name(), context, found }
    }
}

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
    subject: Subject,
    env: ResolvedEnv,
    form: Entity<SettingsForm>,
    terminal: Entity<TerminalView>,
    error: Option<String>,
    note: Option<String>,
    /// The structure id once attached.
    attached: Option<String>,
    /// The features found, once shown.
    picks: Option<Picks>,
    /// How many features were added.
    added: Option<usize>,
    /// The feature found whose row the pointer is over, to mark on the map.
    hover: Option<usize>,
    show_output: bool,
    _subs: Vec<Subscription>,
}

impl PluginView {
    pub fn new(id: TaskId, editor: WeakEntity<SeqEditor>, plugin: Arc<Plugin>, action: Action, subject: Subject, cx: &mut Context<Self>) -> Self {
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
            subject,
            env,
            form,
            terminal,
            error: None,
            note: None,
            attached: None,
            picks: None,
            added: None,
            hover: None,
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
        // The feature or region, as it is now: it is locked from here until the result
        // is dismissed, so what comes back still matches its bases.
        let span = self.editor.upgrade().map(|ed| {
            let doc = &ed.read(cx).doc;
            match &self.subject {
                Subject::Feature(t) => match t.find(doc).map(|ix| Target::protein(&doc.seq, &doc.features[ix])) {
                    Some(Ok(p)) if p == t.protein => t.lock_span(doc.len(), doc.circular).ok_or(()),
                    _ => Err(()),
                },
                Subject::Region(r) if r.unchanged(doc) => Ok(r.lock_span()),
                Subject::Region(_) => Err(()),
            }
        });
        let span = match span {
            Some(Ok(span)) => span,
            None => {
                self.error = Some("The sequence's tab has closed".into());
                return cx.notify();
            }
            Some(Err(())) => {
                self.error = Some(format!("{} has changed since this dialog opened. Close it and run the plugin again.", self.subject.name()));
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
        let input = match &self.subject {
            Subject::Feature(t) => serde_json::json!({
                "sequences": [{"id": "A", "seq": t.protein}],
                "feature": {"label": t.name(), "kind": t.feature.kind},
                "doc_name": t.doc_name,
            }),
            Subject::Region(r) => serde_json::json!({
                "sequences": [{"id": "dna", "seq": String::from_utf8_lossy(&r.bases), "circular": r.circular}],
                "region": {"start": r.start, "end": r.end, "whole": r.whole},
                "doc_name": r.doc_name,
            }),
        };
        let request = serde_json::json!({
            "api": super::manifest::API,
            "action": self.action.id,
            "settings": values,
            "input": input,
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
        let runner =
            PluginRunner { env_argv: argv, script: self.plugin.script.clone(), request, output: self.action.output.clone(), workdir, pty_tx, grid };
        let title = format!("{} · {}", self.action.label.trim_end_matches('…'), self.subject.name());
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
        let (Subject::Feature(target), Produced::Structure { data, format, chains, .. }) = (&self.subject, &out.result) else { return };
        let target = target.clone();
        let Some(editor) = self.editor.upgrade() else {
            self.error = Some("The sequence's tab has closed".into());
            return cx.notify();
        };
        // Find the feature again: edits may have moved it in the list.
        let found = {
            let doc = &editor.read(cx).doc;
            target.find(doc).map(|ix| (ix, Target::protein(&doc.seq, &doc.features[ix])))
        };
        let ix = match found {
            None => {
                self.error = Some(format!("{} is no longer in the document", target.name()));
                return cx.notify();
            }
            Some((_, Err(e))) => {
                self.error = Some(e);
                return cx.notify();
            }
            Some((_, Ok(p))) if p != target.protein => {
                self.error = Some(format!(
                    "{}'s protein has changed since the prediction, so the structure no longer matches it. Save the .cif instead, or run again.",
                    target.name()
                ));
                return cx.notify();
            }
            Some((ix, Ok(_))) => ix,
        };
        let format = if format == "pdb" { StructureFormat::Pdb } else { StructureFormat::Mmcif };
        let stem = format!("{}_{}", target.name(), self.plugin.id());
        let name = format!("{} ({})", target.name(), self.plugin.manifest.name);
        let source = out.source.clone().unwrap_or_else(|| self.plugin.manifest.name.clone());
        let chain = chains.first().cloned().unwrap_or_else(|| "A".into());
        let sid = editor.update(cx, |ed, cx| {
            let id = edna::structure_id(&stem, &ed.doc.structures);
            ed.doc.add_structure(edna::Structure {
                id: id.clone(),
                name,
                format,
                source: Some(source),
                data: data.clone().into(),
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
        let Produced::Structure { format, .. } = &out.result else { return };
        let dir = self
            .editor
            .upgrade()
            .and_then(|e| e.read(cx).doc.path.parent().map(|p| p.to_path_buf()))
            .filter(|p| p.is_dir())
            .or_else(|| std::env::var_os("HOME").map(Into::into))
            .unwrap_or_else(|| ".".into());
        let ext = if format == "pdb" { "pdb" } else { "cif" };
        let suggested = format!("{}.{ext}", self.subject.name().replace(['/', '\\'], "_"));
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => match crate::model::write_atomic(&path, structure_data(&out)) {
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

    /// Where the region's first base is now, if its bases are unchanged: the features
    /// found are placed from there.
    fn placement(&self, doc: &Document) -> Result<usize, String> {
        let Subject::Region(r) = &self.subject else { return Err("not a region".into()) };
        let changed = || format!("{} has changed since the plugin ran, so its features no longer line up. Run it again.", r.name());
        if r.whole {
            // The whole sequence: rotating it, or adding bases at its ends, would move
            // where the features go, so it must be exactly as it was.
            return if doc.seq.as_slice() == r.bases.as_slice() { Ok(0) } else { Err(changed()) };
        }
        match doc.locks().iter().find(|l| l.id == self.id) {
            Some(l) if l.bases(&doc.seq) == r.bases => Ok(l.start),
            _ => Err(changed()),
        }
    }

    /// Work out, once, where each feature found goes and which are already there.
    fn ensure_picks(&mut self, cx: &mut Context<Self>) {
        if self.picks.is_some() {
            return;
        }
        let Some(out) = self.output(cx) else { return };
        let Produced::Features(found) = &out.result else { return };
        let Some(editor) = self.editor.upgrade() else { return };
        let Subject::Region(region) = &self.subject else { return };
        let doc = &editor.read(cx).doc;
        let offset = self.placement(doc).ok();
        let segments: Vec<Vec<(usize, usize)>> =
            found.iter().map(|p| place(&p.record.segments, offset.unwrap_or(0), doc.len().max(1))).collect();
        let dup: Vec<bool> = found.iter().zip(&segments).map(|(p, s)| offset.is_some() && already_there(&doc.features, p, s)).collect();
        let on = found.iter().zip(&dup).map(|(p, d)| p.selected && !d).collect();
        let map = MapView::new(region, doc, found, &segments, offset.unwrap_or(region.start));
        self.picks = Some(Picks { segments, dup, on, show_dups: false, map });
    }

    /// Add the ticked features to the document, as one undo step.
    fn add_features(&mut self, cx: &mut Context<Self>) {
        let Some(out) = self.output(cx) else { return };
        let (Produced::Features(found), Some(picks)) = (&out.result, &self.picks) else { return };
        let Some(editor) = self.editor.upgrade() else {
            self.error = Some("The sequence's tab has closed".into());
            return cx.notify();
        };
        let (offset, len) = {
            let doc = &editor.read(cx).doc;
            (self.placement(doc), doc.len())
        };
        let offset = match offset {
            Ok(o) => o,
            Err(e) => {
                self.error = Some(e);
                return cx.notify();
            }
        };
        let features: Vec<Feature> = found
            .iter()
            .zip(&picks.on)
            .filter(|(_, on)| **on)
            .map(|(p, _)| {
                let mut rec = p.record.clone();
                rec.segments = place(&rec.segments, offset, len).into_iter().map(|(s, e)| [s, e]).collect();
                // A save writes the qualifiers, so the name must be one of them.
                if !rec.label.trim().is_empty() && !rec.qualifiers.iter().any(|(k, _)| k == "label") {
                    rec.qualifiers.insert(0, ("label".into(), Some(rec.label.clone())));
                }
                rec.to_feature()
            })
            .collect();
        if features.is_empty() {
            return;
        }
        let n = features.len();
        editor.update(cx, |ed, cx| {
            ed.doc.edit_features(vec![], features);
            cx.notify();
        });
        self.added = Some(n);
        self.error = None;
        cx.notify();
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

    fn result_card(&self, out: &PluginOutput, cx: &mut Context<Self>) -> AnyElement {
        let (Subject::Feature(target), Produced::Structure { plddt, .. }) = (&self.subject, &out.result) else {
            return self.features_card(out, cx);
        };
        let metric = |label: String, value: String| {
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(theme::muted_text()).child(label))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(value))
        };
        let mut metrics = div().flex().flex_row().gap_6().child(metric("Residues".into(), thousands(target.protein.len())));
        for (k, v) in &out.metrics {
            metrics = metrics.child(metric(k.clone(), if *v <= 1.0 { format!("{v:.2}") } else { format!("{v:.1}") }));
        }
        metrics = metrics.child(metric("Time".into(), format!("{:.0} s", out.elapsed.as_secs_f64())));
        let mut d = div().flex().flex_col().gap_3().p_3().rounded_md().border_1().border_color(theme::border()).bg(theme::editor_bg()).child(metrics);
        if !plddt.is_empty() {
            d = d.child(plddt_strip(plddt)).child(
                div().flex().flex_row().gap_3().text_xs().text_color(theme::muted_text()).children(
                    PLDDT_BANDS.iter().map(|&(_, c, l)| div().flex().flex_row().items_center().gap_1().child(div().size(px(8.0)).bg(gpui::rgb(c))).child(l)),
                ),
            );
        }
        d.into_any_element()
    }

    /// The features found, each with a tick box; ones already in the document are
    /// gathered, unticked, under a heading that opens to show them.
    fn features_card(&self, out: &PluginOutput, cx: &mut Context<Self>) -> AnyElement {
        let (Produced::Features(found), Some(picks)) = (&out.result, &self.picks) else { return div().into_any_element() };
        let locked = self.added.is_some();
        let new: Vec<usize> = (0..found.len()).filter(|&i| !picks.dup[i]).collect();
        let dups: Vec<usize> = (0..found.len()).filter(|&i| picks.dup[i]).collect();
        let tick = |on: bool| {
            div()
                .flex_none()
                .mt(px(2.0))
                .size(px(14.0))
                .rounded_sm()
                .border_1()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .border_color(if on { theme::accent() } else { theme::border() })
                .when(on, |d| d.bg(theme::accent()).text_color(theme::editor_bg()).child("✓"))
        };
        let row = |i: usize, cx: &mut Context<Self>| {
            let p = &found[i];
            let on = picks.on[i];
            let mut line = location(&picks.segments[i], p.record.strand);
            if let Some(d) = p.detail.as_deref().filter(|d| !d.trim().is_empty()) {
                line = format!("{line} · {}", d.trim());
            }
            let name = if p.record.label.trim().is_empty() { p.record.kind.clone() } else { p.record.label.clone() };
            div()
                .id(("plugin-feature", i))
                .flex()
                .flex_row()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_sm()
                .when(self.hover == Some(i), |d| d.bg(theme::hover()))
                .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                    let hover = if *over { Some(i) } else { this.hover.filter(|&h| h != i) };
                    if this.hover != hover {
                        this.hover = hover;
                        cx.notify();
                    }
                }))
                .when(!locked, |d| {
                    d.cursor_pointer().hover(|s| s.bg(theme::hover())).on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(p) = this.picks.as_mut() {
                            p.on[i] ^= true;
                        }
                        cx.notify();
                    }))
                })
                .child(tick(on))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                                .child(div().text_color(theme::muted_text()).child(p.record.kind.clone())),
                        )
                        .child(div().text_xs().text_color(theme::muted_text()).child(line)),
                )
                .into_any_element()
        };
        let link = |id: &'static str, text: &'static str| div().id(id).text_xs().text_color(theme::accent()).cursor_pointer().child(text);
        let ticked = picks.on.iter().filter(|&&o| o).count();
        let source = out.source.clone().unwrap_or_else(|| self.plugin.manifest.name.clone());
        let mut d = div().flex().flex_col().gap_2().p_3().rounded_md().border_1().border_color(theme::border()).bg(theme::editor_bg());
        d = d.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().flex_1().text_sm().child(format!(
                    "{source} found {} feature{} in {:.0} s{}",
                    found.len(),
                    if found.len() == 1 { "" } else { "s" },
                    out.elapsed.as_secs_f64(),
                    if dups.is_empty() { String::new() } else { format!(", {} new", new.len()) },
                )))
                .when(!locked && !new.is_empty(), |h| {
                    h.child(link("pick-all", "All").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.picks.as_mut() {
                            for i in 0..p.on.len() {
                                p.on[i] = !p.dup[i];
                            }
                        }
                        cx.notify();
                    })))
                    .child(link("pick-none", "None").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.picks.as_mut() {
                            p.on.iter_mut().for_each(|o| *o = false);
                        }
                        cx.notify();
                    })))
                }),
        );
        if found.is_empty() {
            d = d.child(div().text_sm().text_color(theme::muted_text()).child("Nothing recognised in these bases."));
        } else {
            d = d.child(self.features_map(found, picks, locked, cx));
        }
        for &i in &new {
            d = d.child(row(i, cx));
        }
        if !dups.is_empty() {
            let open = picks.show_dups;
            d = d.child(
                div()
                    .id("show-dups")
                    .flex()
                    .flex_row()
                    .gap_2()
                    .px_2()
                    .pt_1()
                    .text_xs()
                    .text_color(theme::muted_text())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::text()))
                    .child(if open { "▾" } else { "▸" })
                    .child(format!("{} already in the document", dups.len()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(p) = this.picks.as_mut() {
                            p.show_dups ^= true;
                        }
                        cx.notify();
                    })),
            );
            if open {
                for &i in &dups {
                    d = d.child(row(i, cx));
                }
            }
        }
        if locked {
            let n = self.added.unwrap_or(0);
            d = d.child(div().pt_1().text_sm().child(format!("Added {n} feature{}; Undo takes them out again.", if n == 1 { "" } else { "s" })));
        } else if ticked == 0 && !found.is_empty() {
            d = d.child(div().pt_1().text_xs().text_color(theme::muted_text()).child("Tick the features to add."));
        }
        d.into_any_element()
    }
}

impl PluginView {
    /// The plugin's reminder to cite the work it runs, with links and a Copy.
    fn citations(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cites = &self.plugin.manifest.citations;
        if cites.is_empty() {
            return None;
        }
        let all = cites.iter().map(|c| c.full()).collect::<Vec<_>>().join("\n\n");
        let name = self.plugin.manifest.name.clone();
        let mut d = div().flex().flex_col().gap_1().px_3().py_2().rounded_md().bg(theme::hover()).text_xs().child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(div().flex_1().text_color(theme::muted_text()).child(format!(
                    "If you publish results from {name}, please cite {}:",
                    if cites.len() == 1 { "this" } else { "these" }
                )))
                .child(
                    div()
                        .id("copy-citation")
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .child("Copy")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(all.clone()));
                            this.note = Some("Citation copied.".into());
                            cx.notify();
                        })),
                ),
        );
        for (i, c) in cites.iter().enumerate() {
            let text = c.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut line = div().flex().flex_row().flex_wrap().gap_x_1().child(text);
            if let Some(link) = c.link() {
                let shown = c.doi.as_ref().map(|doi| format!("doi:{doi}")).unwrap_or_else(|| link.clone());
                line = line.child(
                    div()
                        .id(("citation-link", i))
                        .text_color(theme::accent())
                        .cursor_pointer()
                        .child(shown)
                        .on_click(move |_, _, cx| cx.open_url(&link)),
                );
            }
            d = d.child(line);
        }
        Some(d.into_any_element())
    }

    /// The features found on a map, over the document's own (faded). Ticked ones are
    /// drawn in full, unticked ones faint; clicking one ticks or unticks it.
    fn features_map(&self, found: &[ProposedFeature], picks: &Picks, locked: bool, cx: &mut Context<Self>) -> AnyElement {
        let m = &picks.map;
        let n = m.context.len();
        // Duplicates are drawn only when they'd be added, or their group is open:
        // otherwise they repeat what is already there.
        let shown: Vec<usize> = (0..found.len()).filter(|&i| !picks.dup[i] || picks.on[i] || picks.show_dups).collect();
        let mut features = m.context.clone();
        let mut tones = vec![Tone::Faded; n];
        for &i in &shown {
            let mut f = found[i].record.to_feature();
            f.segments = m.found[i].clone();
            features.push(f);
            tones.push(if picks.on[i] || locked { Tone::Normal } else { Tone::Unchosen });
        }
        let clickable: Vec<bool> = (0..features.len()).map(|f| !locked && f >= n).collect();
        let marked = self.hover.and_then(|h| shown.iter().position(|&i| i == h)).map(|k| n + k);
        let me = cx.entity().downgrade();
        let on_click: map::OnFeatureClick = std::rc::Rc::new(move |f, _, cx| {
            let Some(&i) = f.checked_sub(n).and_then(|k| shown.get(k)) else { return };
            me.update(cx, |this, cx| {
                if let Some(p) = this.picks.as_mut() {
                    p.on[i] ^= true;
                }
                cx.notify();
            })
            .ok();
        });
        let height = if m.mode == MapMode::Circular { 340.0 } else { 180.0 };
        div()
            .rounded_sm()
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(map::preview(map::Preview {
                mode: m.mode,
                len: m.len,
                name: m.name.clone(),
                features,
                tones,
                marked,
                band: None,
                height,
                clickable,
                on_click: Some(on_click),
            }))
            .into_any_element()
    }
}

/// A structure result's file contents (empty for any other kind).
fn structure_data(out: &PluginOutput) -> &[u8] {
    match &out.result {
        Produced::Structure { data, .. } => data,
        Produced::Features(_) => &[],
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
                d = d.child(div().text_sm().child(match &self.subject {
                    Subject::Feature(t) => format!("{} · {} aa protein from {} of {}", t.feature.kind, thousands(t.protein.len()), t.name(), t.doc_name),
                    Subject::Region(r) if r.whole => format!(
                        "{} bp · the whole of {}, as {} DNA",
                        thousands(r.bases.len()),
                        r.doc_name,
                        if r.circular { "circular" } else { "linear" }
                    ),
                    Subject::Region(r) => format!(
                        "{} bp · bases {}–{} of {}, as linear DNA (select nothing to use the whole sequence)",
                        thousands(r.bases.len()),
                        thousands(r.start + 1),
                        thousands(r.end),
                        r.doc_name
                    ),
                }));
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
                self.ensure_picks(cx);
                let out = self.output(cx);
                let structure = matches!(out.as_deref(), Some(PluginOutput { result: Produced::Structure { .. }, .. }));
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
                    .child(div().flex_1());
                let close = || button("close", "Close".into(), true).on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.dismiss(id, cx)));
                let name = self.subject.name();
                footer = if structure {
                    let footer = footer
                        .child(dismiss("Dismiss"))
                        .child(button("save", "Save file…".into(), false).on_click(cx.listener(|this, _, _, cx| this.save(cx))));
                    match &self.attached {
                        None => footer.child(button("attach", format!("Attach to {name}"), true).on_click(cx.listener(|this, _, _, cx| this.attach(cx)))),
                        Some(_) => footer.child(close()),
                    }
                } else {
                    let ticked = self.picks.as_ref().map_or(0, |p| p.on.iter().filter(|&&o| o).count());
                    match self.added {
                        None if ticked > 0 => footer.child(dismiss("Dismiss")).child(
                            button("add", format!("Add {ticked} feature{}", if ticked == 1 { "" } else { "s" }), true)
                                .on_click(cx.listener(|this, _, _, cx| this.add_features(cx))),
                        ),
                        None => footer.child(dismiss("Dismiss")),
                        Some(_) => footer.child(close()),
                    }
                };
                let mut d = div().flex().flex_col().gap_3();
                if let Some(out) = &out {
                    d = d.child(self.result_card(out, cx));
                }
                if !structure {
                } else if let Some(sid) = &self.attached {
                    d = d.child(div().text_sm().child(format!("Attached as structure {sid}; {name}'s chain A is linked to it.")));
                } else {
                    d = d.child(div().text_xs().text_color(theme::muted_text()).child(
                        "Attach adds the structure to this document and links the feature to its chain. Nothing is added until you do.",
                    ));
                }
                if let Some(cite) = self.citations(cx) {
                    d = d.child(cite);
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
    use super::{already_there, location, merged, place, Target};
    use crate::model::{Feature, FeatureRecord};
    use crate::plugins::runner::ProposedFeature;

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

    #[test]
    fn found_features_go_where_the_region_is() {
        // A region starting at base 100: everything moves along.
        assert_eq!(place(&[[0, 10], [20, 30]], 100, 1000), vec![(100, 110), (120, 130)]);
        // A region through the origin (900..50): features after its 100th base wrap.
        assert_eq!(place(&[[10, 20]], 900, 1000), vec![(910, 920)]);
        assert_eq!(place(&[[120, 140]], 900, 1000), vec![(20, 40)]);
        assert_eq!(place(&[[90, 110]], 900, 1000), vec![(990, 1000), (0, 10)]);
        // A whole circular sequence's feature across the origin stays that way.
        assert_eq!(place(&[[990, 1000], [0, 10]], 0, 1000), vec![(990, 1000), (0, 10)]);
        // Segments that meet are one.
        assert_eq!(merged([(1, 5), (5, 9), (12, 14)]), vec![(1, 9), (12, 14)]);
        assert_eq!(location(&[(990, 1000), (0, 10)], -1), "991–1,000, 1–10 ←");
    }

    #[test]
    fn duplicates_are_recognised() {
        let have = |kind: &str, label: &str, segments: Vec<(usize, usize)>, strand| Feature { kind: kind.into(), label: label.into(), segments, strand, source: None };
        let found = |kind: &str, label: &str, strand| ProposedFeature {
            record: FeatureRecord { kind: kind.into(), label: label.into(), strand, segments: vec![], qualifiers: vec![] },
            detail: None,
            selected: true,
        };
        let existing = vec![have("CDS", "bla", vec![(100, 200)], -1), have("rep_origin", "ori", vec![(900, 1000), (0, 20)], 1)];
        // Same bases and strand, and the same type or name.
        assert!(already_there(&existing, &found("CDS", "AmpR", -1), &[(100, 200)]));
        assert!(already_there(&existing, &found("gene", "BLA", -1), &[(100, 200)]));
        assert!(!already_there(&existing, &found("promoter", "AmpR promoter", -1), &[(100, 200)]));
        assert!(!already_there(&existing, &found("CDS", "AmpR", 1), &[(100, 200)]));
        assert!(!already_there(&existing, &found("CDS", "AmpR", -1), &[(100, 201)]));
        assert!(already_there(&existing, &found("rep_origin", "ori", 1), &[(900, 1000), (0, 20)]));
    }
}

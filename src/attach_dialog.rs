//! The Attach Structure / Link Chains dialog.
//!
//! Each protein chain of a structure can be tied to a feature of the sequence: an
//! existing one (the features its residues align to best are offered first), a new
//! CDS made from where the chain aligns in any of the six reading frames, or nothing.
//! The choices are stored as `/structure="<id>:<chain>"` qualifiers on the features.

use std::collections::BTreeMap;
use std::rc::Rc;

use gb_io::seq::Feature as GbFeature;
use gpui::{
    anchored, deferred, div, point, prelude::*, px, Context, Entity, EventEmitter, FocusHandle, FontWeight, SharedString,
    Window,
};

use crate::editor::{thousands, SeqEditor};
use crate::model::{self, Feature};
use crate::pgx::ChainLinking;
use crate::structure::{self, Molecule, Proposal, StructureFile};
use crate::theme;
use crate::workspace::Dismiss;

pub enum AttachEvent {
    /// Applied; the structure to show.
    Done(String),
    Cancel,
}

/// What a chain is tied to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    Feature(usize),
    New,
    None,
}

/// A feature a chain could be tied to, and how well the chain aligns to it.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub feature: usize,
    pub identity: f32,
    pub aligned: usize,
    pub convincing: bool,
}

/// One protein chain and the ways it could be linked.
#[derive(Clone, Debug)]
pub struct ChainRow {
    pub chain: String,
    pub residues: usize,
    /// Best first.
    pub candidates: Vec<Candidate>,
    pub proposal: Option<Proposal>,
    pub choice: Choice,
}

/// Align every protein chain of `mol` to the CDS features and to the six frames, and
/// pick a starting choice for each: the features already naming it (`bound`), else the
/// best convincing CDS, else a new feature if the chain is encoded somewhere, else none.
pub fn analyze(mol: &Molecule, seq: &[u8], features: &[Feature], circular: bool, bound: &[(usize, String)]) -> Vec<ChainRow> {
    let cds: Vec<(usize, &Feature)> = features
        .iter()
        .enumerate()
        .filter(|(i, f)| f.kind.eq_ignore_ascii_case("CDS") || bound.iter().any(|(b, _)| b == i))
        .collect();
    let mut rows = Vec::new();
    for ci in structure::protein_chains(mol) {
        let chain = mol.chains[ci].id.clone();
        let (letters, _) = structure::chain_letters(mol, ci);
        let mut candidates: Vec<Candidate> = cds
            .iter()
            .filter_map(|&(fi, f)| {
                let link = structure::link_chain_to_feature(mol, ci, seq, fi, f)?;
                let protein_len = f.segments.iter().map(|(s, e)| e - s).sum::<usize>() / 3;
                Some(Candidate { feature: fi, identity: link.identity, aligned: link.aligned, convincing: link.convincing(letters.len(), protein_len) })
            })
            .collect();
        candidates.sort_by(|a, b| {
            b.convincing.cmp(&a.convincing).then((b.aligned as f32 * b.identity).total_cmp(&(a.aligned as f32 * a.identity)))
        });
        let proposal = structure::propose_feature(mol, ci, seq, circular);
        let choice = if let Some((fi, _)) = bound.iter().find(|(_, c)| *c == chain) {
            Choice::Feature(*fi)
        } else if let Some(c) = candidates.first().filter(|c| c.convincing) {
            Choice::Feature(c.feature)
        } else if proposal.is_some() {
            Choice::New
        } else {
            Choice::None
        };
        rows.push(ChainRow { chain, residues: letters.len(), candidates, proposal, choice });
    }
    rows
}

/// What a dialog is attaching or relinking.
enum Target {
    /// A file not yet in the document; it's added when the dialog is confirmed.
    New(Rc<StructureFile>),
    /// A structure already attached, by id.
    Existing(String),
}

pub struct AttachDialog {
    editor: Entity<SeqEditor>,
    target: Target,
    /// The file's name and stem, for titles and new feature labels.
    name: String,
    stem: String,
    /// None while the chains are being aligned.
    rows: Option<Vec<ChainRow>>,
    /// The document version the rows were worked out for.
    version: u64,
    /// The row whose choices are showing.
    open: Option<usize>,
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<AttachEvent> for AttachDialog {}

impl AttachDialog {
    /// A dialog for a newly read file.
    pub fn attach(editor: Entity<SeqEditor>, file: StructureFile, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (name, stem) = (file.name.clone(), file.stem.clone());
        let file = Rc::new(file);
        let mol = file.mol.clone();
        Self::new(editor, Target::New(file), name, stem, mol, window, cx)
    }

    /// A dialog to change how attached structure `id` is linked.
    pub fn relink(editor: Entity<SeqEditor>, id: String, mol: Molecule, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = editor.read(cx).doc.structures.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s).to_string();
        Self::new(editor, Target::Existing(id), name, stem, mol, window, cx)
    }

    fn new(editor: Entity<SeqEditor>, target: Target, name: String, stem: String, mol: Molecule, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let doc = &editor.read(cx).doc;
        let (seq, features, circular, version) = (doc.seq.to_vec(), doc.features.to_vec(), doc.circular, doc.version);
        // Chains already tied to features, for relinking; an automatically linked
        // structure starts from what it links to now.
        let mut bound: Vec<(usize, String)> = Vec::new();
        if let Target::Existing(id) = &target {
            for (fi, f) in features.iter().enumerate() {
                bound.extend(f.structure_refs().into_iter().filter(|(s, _)| s == id).map(|(_, c)| (fi, c)));
            }
            let linking = doc.structures.iter().find(|s| &s.id == id).map(|s| s.linking);
            if linking == Some(ChainLinking::Auto) {
                bound.extend(structure::link_chains(&mol, &seq, &features).into_iter().map(|l| (l.feature, mol.chains[l.chain].id.clone())));
            }
        }
        let auto_none = matches!(&target, Target::Existing(id) if doc.structures.iter().any(|s| &s.id == id && s.linking == ChainLinking::None));
        let task = cx.background_executor().spawn(async move { analyze(&mol, &seq, &features, circular, &bound) });
        cx.spawn(async move |this, cx| {
            let mut rows = task.await;
            if auto_none {
                for r in &mut rows {
                    r.choice = Choice::None;
                }
            }
            this.update(cx, |d, cx| {
                d.rows = Some(rows);
                cx.notify();
            })
            .ok();
        })
        .detach();
        let focus = cx.focus_handle();
        focus.focus(window);
        Self { editor, target, name, stem, rows: None, version, open: None, error: None, focus }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(AttachEvent::Cancel);
    }

    fn choose(&mut self, row: usize, choice: Choice, cx: &mut Context<Self>) {
        if let Some(r) = self.rows.as_mut().and_then(|rows| rows.get_mut(row)) {
            r.choice = choice;
        }
        self.open = None;
        cx.notify();
    }

    /// Attach the structure if it's new, and store the links.
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = self.rows.clone() else { return };
        if self.editor.read(cx).doc.version != self.version {
            self.error = Some("The document changed while this was open. Cancel and try again.".into());
            cx.notify();
            return;
        }
        let (target, stem) = (&self.target, self.stem.clone());
        let id = self.editor.update(cx, |ed, cx| {
            let linked = rows.iter().any(|r| r.choice != Choice::None);
            let linking = if linked { ChainLinking::Features } else { ChainLinking::None };
            let id = match target {
                Target::New(file) => {
                    let s = file.to_structure(&ed.doc.structures, linking);
                    let id = s.id.clone();
                    ed.doc.add_structure(s);
                    id
                }
                Target::Existing(id) => {
                    ed.doc.set_structure_linking(id, linking);
                    id.clone()
                }
            };
            let (replace, add) = plan(&ed.doc.features, &id, &stem, &rows);
            ed.doc.edit_features(replace, add);
            cx.notify();
            id
        });
        cx.emit(AttachEvent::Done(id));
    }
}

/// The feature edits that make `rows`' choices the links of structure `id`: features
/// replaced (by index), and new CDS features. Chains proposing the same stretch of
/// sequence, like the copies of a homodimer, share one new feature.
pub fn plan(features: &[Feature], id: &str, stem: &str, rows: &[ChainRow]) -> (Vec<(usize, Feature)>, Vec<Feature>) {
    let mut chosen: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut new: Vec<(&Proposal, Vec<String>)> = Vec::new();
    for r in rows {
        match r.choice {
            Choice::Feature(fi) => chosen.entry(fi).or_default().push(r.chain.clone()),
            Choice::New => {
                let Some(p) = &r.proposal else { continue };
                match new.iter_mut().find(|(q, _)| q.segments == p.segments && q.strand == p.strand) {
                    Some((_, chains)) => chains.push(r.chain.clone()),
                    None => new.push((p, vec![r.chain.clone()])),
                }
            }
            Choice::None => {}
        }
    }
    let mut replace = Vec::new();
    for (fi, f) in features.iter().enumerate() {
        let chains = chosen.remove(&fi).unwrap_or_default();
        let had: Vec<String> = f.structure_refs().into_iter().filter(|(s, _)| s == id).map(|(_, c)| c).collect();
        if had != chains {
            let mut f = f.clone();
            f.set_structure_chains(id, &chains);
            replace.push((fi, f));
        }
    }
    let add = new
        .into_iter()
        .filter_map(|(p, chains)| {
            let label = format!("{stem} chain {}", chains.join(""));
            let location = model::segments_location(&p.segments, p.strand)?;
            let note = format!("Found by aligning chain {} of {stem} ({:.0}% identical)", chains.join(", "), p.identity * 100.0);
            let source = GbFeature {
                kind: "CDS".into(),
                location,
                qualifiers: vec![("label".into(), Some(label.clone())), ("note".into(), Some(note))],
            };
            let mut f = Feature { kind: "CDS".into(), label, segments: p.segments.clone(), strand: p.strand, source: Some(source) };
            f.set_structure_chains(id, &chains);
            Some(f)
        })
        .collect();
    (replace, add)
}

/// 1-based inclusive positions, as the dialog shows them.
fn span_text(segments: &[(usize, usize)], len: usize) -> String {
    // A wrapped location is stored as [(0, b), (a, len)].
    match segments {
        [(0, b), (a, e)] if *e == len => format!("{}–{}", thousands(a + 1), thousands(*b)),
        _ => {
            let (s, e) = (segments.first().map_or(0, |s| s.0), segments.last().map_or(0, |s| s.1));
            format!("{}–{}", thousands(s + 1), thousands(e))
        }
    }
}

impl Render for AttachDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let doc = &self.editor.read(cx).doc;
        let (features, seq_len) = (doc.features.clone(), doc.len());

        let hint = |text: SharedString| div().text_xs().text_color(theme::muted_text()).child(text);
        let button = |id: &'static str, text: &'static str, primary: bool, enabled: bool| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .border_1()
                .when(!enabled, |d| d.opacity(0.5))
                .when(enabled, |d| d.cursor_pointer())
                .when(primary, |d| d.border_color(theme::accent()).bg(theme::accent()).text_color(theme::editor_bg()))
                .when(!primary, |d| d.border_color(theme::border()).hover(|s| s.bg(theme::hover())))
                .child(text)
        };
        let feature_name = |fi: usize| features.get(fi).map_or_else(|| "?".to_string(), |f| f.label.clone());
        let describe = |row: &ChainRow, choice: Choice| -> (String, String) {
            match choice {
                Choice::Feature(fi) => {
                    let c = row.candidates.iter().find(|c| c.feature == fi);
                    let detail = c.map_or(String::new(), |c| format!("{:.0}% identical over {} residues", c.identity * 100.0, c.aligned));
                    (feature_name(fi), detail)
                }
                Choice::New => match &row.proposal {
                    Some(p) => (
                        "New CDS from alignment".into(),
                        format!(
                            "{} {} · {:.0}% identical",
                            span_text(&p.segments, seq_len),
                            if p.strand > 0 { "→" } else { "←" },
                            p.identity * 100.0
                        ),
                    ),
                    None => ("New CDS from alignment".into(), "The chain isn't encoded in the sequence".into()),
                },
                Choice::None => ("Don't link".into(), String::new()),
            }
        };

        let body = match &self.rows {
            None => div().p_4().child(hint("Aligning chains to the sequence…".into())),
            Some(rows) if rows.is_empty() => div().p_4().child(hint(
                "This structure has no protein chains to link. It will be attached without links to the sequence.".into(),
            )),
            Some(rows) => {
                let mut list = div().flex().flex_col().gap_3().p_4().child(hint(
                    "Choose the feature each chain is encoded by. Residues of linked chains select their codons, and the other way round."
                        .into(),
                ));
                for (i, row) in rows.iter().enumerate() {
                    let (title, detail) = describe(row, row.choice);
                    let open = self.open == Some(i);
                    let picker = div()
                        .id(("chain-choice", i))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(if open { theme::accent() } else { theme::border() })
                        .bg(theme::editor_bg())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .child(div().flex_none().child(title))
                        .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme::muted_text()).child(detail))
                        .child(div().flex_none().text_xs().text_color(theme::muted_text()).child(if open { "▴" } else { "▾" }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open = if this.open == Some(i) { None } else { Some(i) };
                            cx.notify();
                        }));
                    let mut entry = div().flex().flex_col().gap_1().child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(110.0))
                                    .flex()
                                    .flex_col()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Chain {}", row.chain)))
                                    .child(hint(format!("{} residues", thousands(row.residues)).into())),
                            )
                            .child(picker),
                    );
                    if open {
                        let mut options: Vec<Choice> = row.candidates.iter().map(|c| Choice::Feature(c.feature)).collect();
                        options.push(Choice::New);
                        options.push(Choice::None);
                        let first_weak = row.candidates.iter().position(|c| !c.convincing);
                        let mut menu = div()
                            .ml(px(122.0))
                            .p_1()
                            .flex()
                            .flex_col()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::panel())
                            .shadow_md();
                        for (k, choice) in options.into_iter().enumerate() {
                            if Some(k) == first_weak {
                                menu = menu.child(div().px_2().pt_1().child(hint("Weaker matches".into())));
                            }
                            if choice == Choice::New {
                                menu = menu.child(div().my_1().h(px(1.0)).bg(theme::border()));
                            }
                            let enabled = choice != Choice::New || row.proposal.is_some();
                            let (title, detail) = describe(row, choice);
                            menu = menu.child(
                                div()
                                    .id(("chain-option", k))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py(px(3.0))
                                    .rounded_sm()
                                    .when(choice == row.choice, |d| d.bg(theme::active()))
                                    .when(!enabled, |d| d.opacity(0.5))
                                    .when(enabled, |d| {
                                        d.cursor_pointer()
                                            .hover(|s| s.bg(theme::hover()))
                                            .on_click(cx.listener(move |this, _, _, cx| this.choose(i, choice, cx)))
                                    })
                                    .child(div().flex_none().child(title))
                                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme::muted_text()).child(detail)),
                            );
                        }
                        if row.candidates.is_empty() {
                            menu = menu.child(div().px_2().py_1().child(hint("No CDS features to choose from".into())));
                        }
                        entry = entry.child(menu);
                    }
                    list = list.child(entry);
                }
                list
            }
        };

        let ready = self.rows.is_some();
        let (title, action) = match self.target {
            Target::New(_) => ("Attach Structure", "Attach"),
            Target::Existing(_) => ("Link Chains", "Apply"),
        };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(theme::border())
            .child(div().flex_1().text_sm().text_color(theme::error()).children(self.error.clone()))
            .child(button("attach-cancel", "Cancel", false, true).on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
            .child(button("attach-confirm", action, true, ready).when(ready, |d| d.on_click(cx.listener(|this, _, _, cx| this.confirm(cx)))));

        let panel = div()
            .id("attach-dialog")
            .track_focus(&self.focus)
            .occlude()
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.cancel(cx)))
            .flex()
            .flex_col()
            .w(px(600.0))
            .max_h(viewport.height - px(80.0))
            .rounded_lg()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .shadow_lg()
            .text_sm()
            .text_color(theme::text())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(hint(self.name.clone().into())),
            )
            .child(div().id("attach-dialog-body").flex_1().min_h_0().overflow_y_scroll().child(body))
            .child(footer);

        deferred(
            anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .id("attach-dialog-backdrop")
                    .occlude()
                    .w(viewport.width)
                    .h(viewport.height)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                    .child(panel),
            ),
        )
        .with_priority(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Document;
    use crate::pgx::StructureFormat;

    fn sample() -> (Document, Molecule) {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/samples/");
        let doc = Document::open(std::path::Path::new(&format!("{dir}pEGFP-N1.gb"))).unwrap();
        let data = std::fs::read(format!("{dir}2Y0G.pdb")).unwrap();
        (doc, Molecule::parse(&data, StructureFormat::Pdb).unwrap())
    }

    #[test]
    fn chains_default_to_their_cds_and_new_features_are_shared() {
        let (doc, mol) = sample();
        let rows = analyze(&mol, &doc.seq, &doc.features, doc.circular, &[]);
        assert!(!rows.is_empty());
        let egfp = doc.features.iter().position(|f| f.kind == "CDS" && f.segments == vec![(678, 1398)]).unwrap();
        for r in &rows {
            assert_eq!(r.choice, Choice::Feature(egfp), "chain {}", r.chain);
            let p = r.proposal.as_ref().expect("EGFP is encoded");
            assert_eq!(p.strand, 1);
            assert!(p.segments[0].0 >= 678 && p.segments[0].1 <= 1398, "{p:?}");
        }

        // Linking to the existing CDS adds a qualifier and nothing else.
        let (replace, add) = plan(&doc.features, "2y0g", "2Y0G", &rows);
        assert!(add.is_empty());
        assert_eq!(replace.len(), 1);
        let chains: Vec<_> = rows.iter().map(|r| ("2y0g".to_string(), r.chain.clone())).collect();
        assert_eq!(replace[0].1.structure_refs(), chains);

        // Every chain asking for a new feature over the same codons gets the same one.
        let mut rows = rows;
        for r in &mut rows {
            r.choice = Choice::New;
        }
        let (replace, add) = plan(&doc.features, "2y0g", "2Y0G", &rows);
        assert!(replace.is_empty());
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].kind, "CDS");
        assert_eq!(add[0].structure_refs(), chains);

        // Unlinking takes the qualifiers off again.
        let mut features = doc.features.to_vec();
        for (ix, f) in plan(&features, "2y0g", "2Y0G", &rows.iter().map(|r| ChainRow { choice: Choice::Feature(egfp), ..r.clone() }).collect::<Vec<_>>()).0 {
            features[ix] = f;
        }
        for r in &mut rows {
            r.choice = Choice::None;
        }
        let (replace, add) = plan(&features, "2y0g", "2Y0G", &rows);
        assert!(add.is_empty());
        assert_eq!(replace.len(), 1);
        assert!(replace[0].1.structure_refs().is_empty());
    }

    #[test]
    fn bound_chains_keep_their_feature() {
        let (doc, mol) = sample();
        let chain = mol.chains[structure::protein_chains(&mol)[0]].id.clone();
        let rows = analyze(&mol, &doc.seq, &doc.features, doc.circular, &[(0, chain.clone())]);
        assert_eq!(rows.iter().find(|r| r.chain == chain).unwrap().choice, Choice::Feature(0));
    }
}

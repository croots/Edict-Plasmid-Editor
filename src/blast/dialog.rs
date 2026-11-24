//! The BLAST task's view: the options form, then the run log, then the results.
//! It's shown in the workspace's task overlay whenever its task is in front.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, Div, Entity, FontWeight, ScrollHandle, SharedString, Stateful,
    Subscription, UniformListScrollHandle, WeakEntity, Window,
};

use super::results::{self, Column};
use super::{BlastResults, BlastRunner, Options, Program};
use crate::editor::{editor_font, thousands, SeqEditor};
use crate::settings;
use crate::tasks::{self, LogKind, TaskId, TaskStatus};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;

/// The bases a search was opened on, as they were then.
#[derive(Clone)]
pub struct Region {
    pub start: usize,
    pub end: usize,
    pub bases: Vec<u8>,
    pub doc_name: String,
    /// The document's version when the region was taken, so a stale dialog can't run.
    pub version: u64,
}

impl Region {
    /// "1,234 bp of pUC19 (101..1334)", in 1-based coordinates.
    pub fn describe(&self) -> String {
        format!("{} bp of {} ({}..{})", thousands(self.bases.len()), self.doc_name, self.start + 1, self.end)
    }
}

/// Genetic codes for translated searches (NCBI's numbering).
const GENETIC_CODES: &[(u8, &str)] = &[
    (1, "Standard"),
    (2, "Vertebrate mitochondrial"),
    (3, "Yeast mitochondrial"),
    (4, "Mold / mycoplasma"),
    (5, "Invertebrate mitochondrial"),
    (6, "Ciliate nuclear"),
    (9, "Echinoderm mitochondrial"),
    (10, "Euplotid nuclear"),
    (11, "Bacterial and plant plastid"),
    (12, "Alternative yeast"),
    (13, "Ascidian mitochondrial"),
    (14, "Alt. flatworm mitochondrial"),
    (15, "Blepharisma"),
    (16, "Chlorophycean mitochondrial"),
];

pub struct BlastView {
    id: TaskId,
    editor: WeakEntity<SeqEditor>,
    region: Region,
    options: Options,
    organisms: Entity<TextInput>,
    exclude: Entity<TextInput>,
    expect: Entity<TextInput>,
    title: Entity<TextInput>,
    error: Option<String>,
    sort: (Column, bool),
    /// The hit whose alignments are open.
    open: Option<usize>,
    show_log: bool,
    log_scroll: ScrollHandle,
    log_seen: usize,
    table_scroll: UniformListScrollHandle,
    align_scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl BlastView {
    pub fn new(id: TaskId, editor: WeakEntity<SeqEditor>, region: Region, cx: &mut Context<Self>) -> Self {
        let mut options = Options::from_map(&settings::shared().blast);
        options.title.clear();
        let field = |text: &str, placeholder: String, cx: &mut Context<Self>| cx.new(|cx| TextInput::new(text, placeholder, cx));
        let organisms = field(&options.organisms, "e.g. Escherichia coli, or a taxid like 562; commas for several".into(), cx);
        let exclude = field(&options.exclude_organisms, "Organisms to leave out".into(), cx);
        let expect = field(&options.expect, "0.05".into(), cx);
        let title = field("", region.describe(), cx);
        let mut subs = vec![cx.observe(&tasks::manager(cx), |_, _, cx| cx.notify())];
        let sync = |input: &Entity<TextInput>, set: fn(&mut Options, String), cx: &mut Context<Self>| {
            cx.subscribe(input, move |this: &mut Self, input, ev: &InputEvent, cx| match ev {
                InputEvent::Changed => {
                    set(&mut this.options, input.read(cx).text().to_string());
                    this.error = None;
                    cx.notify();
                }
                InputEvent::Submit => this.run(cx),
                _ => {}
            })
        };
        subs.push(sync(&organisms, |o, t| o.organisms = t, cx));
        subs.push(sync(&exclude, |o, t| o.exclude_organisms = t, cx));
        subs.push(sync(&expect, |o, t| o.expect = t, cx));
        subs.push(sync(&title, |o, t| o.title = t, cx));
        BlastView {
            id,
            editor,
            region,
            options,
            organisms,
            exclude,
            expect,
            title,
            error: None,
            sort: (Column::Evalue, true),
            open: None,
            show_log: false,
            log_scroll: ScrollHandle::new(),
            log_seen: 0,
            table_scroll: UniformListScrollHandle::new(),
            align_scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    fn manager(&self, cx: &App) -> Entity<tasks::TaskManager> {
        tasks::manager(cx)
    }

    /// Send the search, or queue it behind another.
    fn run(&mut self, cx: &mut Context<Self>) {
        let manager = self.manager(cx);
        if manager.read(cx).task(self.id).map(|t| t.status.clone()) != Some(TaskStatus::Draft) {
            return;
        }
        let Some(editor) = self.editor.upgrade() else {
            self.error = Some("The sequence's tab has closed".into());
            return cx.notify();
        };
        if editor.read(cx).doc.version != self.region.version {
            self.error = Some("The sequence has changed since this dialog opened. Select the region again.".into());
            return cx.notify();
        }
        if let Some(p) = self.options.problem() {
            self.error = Some(p);
            return cx.notify();
        }
        let Some(email) = settings::shared().email.filter(|e| !e.trim().is_empty()) else {
            self.error = Some("Add your email address in Settings → Keys first; NCBI asks for one.".into());
            return cx.notify();
        };
        if let Err(e) = settings::update_shared(|s| s.blast = self.options.to_map()) {
            eprintln!("couldn't save the BLAST options: {e:#}");
        }
        let mut options = self.options.clone();
        options.title = options.title.trim().to_string();
        let shown = if options.title.is_empty() { self.region.describe() } else { options.title.clone() };
        if options.title.is_empty() {
            options.title = shown.clone();
        }
        let runner = BlastRunner { options, query: String::from_utf8_lossy(&self.region.bases).into_owned(), email };
        let (id, span) = (self.id, (self.region.start, self.region.end));
        let r = manager.update(cx, |m, cx| {
            m.set_title(id, "BLAST", format!("{} · {shown}", self.options.program.id()), cx);
            m.submit(id, Box::new(runner), Some(span), cx)
        });
        self.error = r.err().map(|e| format!("{e:#}"));
        cx.notify();
    }

    fn results(&self, cx: &App) -> Option<Arc<BlastResults>> {
        let out = self.manager(cx).read(cx).task(self.id)?.output.clone()?;
        out.downcast::<BlastResults>().ok()
    }

    /// Open the Create Feature dialog for one match.
    fn convert(&mut self, hit: usize, hsp: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(res) = self.results(cx) else { return };
        let Some(editor) = self.editor.upgrade() else {
            self.error = Some("The sequence's tab has closed".into());
            return cx.notify();
        };
        let manager = self.manager(cx);
        let finished = manager.read(cx).task(self.id).and_then(|t| t.finished_at).unwrap_or_else(std::time::SystemTime::now);
        let draft = {
            let doc = &editor.read(cx).doc;
            let Some(lock) = doc.lock_of(self.id) else {
                self.error = Some("This search no longer holds its region".into());
                return cx.notify();
            };
            if super::hash(&lock.bases(&doc.seq)) != res.bases_hash {
                self.error = Some("The searched bases have changed, so the match can't be placed".into());
                return cx.notify();
            }
            let (Some(h), Some(p)) = (res.hits.get(hit), res.hits.get(hit).and_then(|h| h.hsps.get(hsp))) else { return };
            let author = settings::shared().effective_author();
            results::draft(&res, h, p, &lock, doc.len(), author.as_deref(), &super::date(finished))
        };
        manager.update(cx, |m, cx| m.background(cx));
        editor.update(cx, |ed, cx| {
            ed.create_feature_from(&draft.segments, draft.strand, &draft.kind, &draft.name, &draft.qualifiers, window, cx)
        });
    }

    // ----- rendering -----

    fn chips<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        items: Vec<(T, String)>,
        current: T,
        set: fn(&mut Options, T),
        cx: &mut Context<Self>,
    ) -> Div {
        div().flex().flex_row().flex_wrap().gap_1().children(items.into_iter().enumerate().map(|(i, (value, text))| {
            let on = value == current;
            div()
                .id((id, i))
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .border_1()
                .text_xs()
                .cursor_pointer()
                .when(on, |d| d.border_color(theme::accent()).bg(theme::accent()).text_color(theme::editor_bg()))
                .when(!on, |d| d.border_color(theme::border()).hover(|s| s.bg(theme::hover())))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    set(&mut this.options, value);
                    this.error = None;
                    cx.notify();
                }))
        }))
    }

    fn check(&self, id: &'static str, text: &'static str, on: bool, flip: fn(&mut Options), cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .child(
                div()
                    .size(px(14.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(if on { theme::accent() } else { theme::border() })
                    .when(on, |d| d.bg(theme::accent()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme::editor_bg())
                    .child(if on { "✓" } else { "" }),
            )
            .child(text)
            .on_click(cx.listener(move |this, _, _, cx| {
                flip(&mut this.options);
                cx.notify();
            }))
    }

    fn form(&self, cx: &mut Context<Self>) -> Div {
        let o = &self.options;
        let p = o.program;
        let label = |text: &'static str| div().w(px(130.0)).flex_none().pt(px(2.0)).text_sm().text_color(theme::muted_text()).child(text);
        let row = |l: &'static str, content: AnyElement| div().flex().flex_row().gap_3().child(label(l)).child(div().flex_1().child(content));
        let input = |e: &Entity<TextInput>| {
            div().px_2().py_1().rounded_md().border_1().border_color(theme::border()).bg(theme::app_bg()).text_sm().child(e.clone())
        };
        let heading = |text: &'static str| {
            div().pt_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme::muted_text()).child(text)
        };
        let note = |text: String| div().text_xs().text_color(theme::muted_text()).child(text);

        let programs = Program::ALL.iter().map(|&p| (p, p.title().to_string())).collect();
        let dbs = p.databases().iter().map(|(id, name)| (*id, format!("{name} ({id})"))).collect();
        let current_db: &'static str = p.databases().iter().map(|(id, _)| *id).find(|id| *id == o.database).unwrap_or("");
        let words = p.word_sizes().iter().map(|&w| (w, w.to_string())).collect();
        let targets = super::MAX_TARGETS.iter().map(|&n| (n, n.to_string())).collect();

        let mut d = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("Program", self.chips("program", programs, p, |o, v| o.set_program(v), cx).into_any_element()))
            .child(row("", note(p.blurb().to_string()).into_any_element()))
            .child(row(
                "Database",
                self.chips("db", dbs, current_db, |o, v| o.database = v.to_string(), cx).into_any_element(),
            ))
            .child(heading("LIMIT TO"))
            .child(row("Organisms", input(&self.organisms).into_any_element()))
            .child(row("Exclude", input(&self.exclude).into_any_element()))
            .child(row(
                "",
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .child(self.check("x-models", "Models (XM/XP)", o.exclude_models, |o| o.exclude_models ^= true, cx))
                    .child(self.check(
                        "x-uncult",
                        "Uncultured / environmental samples",
                        o.exclude_uncultured,
                        |o| o.exclude_uncultured ^= true,
                        cx,
                    ))
                    .into_any_element(),
            ))
            .child(heading("SCORING"))
            .child(row("E-value threshold", div().w(px(140.0)).child(input(&self.expect)).into_any_element()))
            .child(row("Max targets", self.chips("targets", targets, o.max_targets, |o, v| o.max_targets = v, cx).into_any_element()))
            .child(row("Word size", self.chips("word", words, o.word_size, |o, v| o.word_size = v, cx).into_any_element()));

        if p.translated() {
            let matrices = super::MATRICES.iter().map(|&m| (m, m.to_string())).collect();
            let current: &'static str = super::MATRICES.iter().copied().find(|m| *m == o.matrix).unwrap_or("BLOSUM62");
            let codes = GENETIC_CODES.iter().map(|&(n, name)| (n, format!("{n} · {name}"))).collect();
            d = d
                .child(row("Matrix", self.chips("matrix", matrices, current, |o, v| o.matrix = v.to_string(), cx).into_any_element()))
                .child(row("Genetic code", self.chips("code", codes, o.genetic_code, |o, v| o.genetic_code = v, cx).into_any_element()));
        } else {
            let scores = super::SCORES.iter().map(|&(a, b)| ((a, b), format!("{a}, {b}"))).collect();
            d = d.child(row(
                "Match, mismatch",
                self.chips("scores", scores, o.scores, |o, v| o.scores = v, cx).into_any_element(),
            ));
        }
        if p.gapped() {
            let list = if p.protein_db() { super::PROT_GAPS } else { super::NUCL_GAPS };
            let gaps = list
                .iter()
                .map(|&g| (g, if g == "linear" { "Linear".to_string() } else { format!("Open {} extend {}", g.split(' ').next().unwrap_or(""), g.split(' ').nth(1).unwrap_or("")) }))
                .collect();
            let current: &'static str = list.iter().copied().find(|g| *g == o.gaps).unwrap_or(list[0]);
            d = d.child(row("Gap costs", self.chips("gaps", gaps, current, |o, v| o.gaps = v.to_string(), cx).into_any_element()));
        }
        if p == Program::Blastx {
            let comp = super::COMPOSITION.iter().map(|&(n, l)| (n, l.to_string())).collect();
            d = d.child(row(
                "Compositional adj.",
                self.chips("comp", comp, o.composition, |o, v| o.composition = v, cx).into_any_element(),
            ));
        }
        d = d
            .child(heading("FILTERS AND MASKING"))
            .child(row(
                "",
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.check("f-low", "Low-complexity regions", o.low_complexity, |o| o.low_complexity ^= true, cx))
                    .when(!p.protein_db(), |d| {
                        d.child(self.check(
                            "f-rep",
                            "Species-specific repeats",
                            o.species_repeats,
                            |o| o.species_repeats ^= true,
                            cx,
                        ))
                    })
                    .child(self.check(
                        "f-lookup",
                        "Mask for lookup table only",
                        o.mask_lookup_only,
                        |o| o.mask_lookup_only ^= true,
                        cx,
                    ))
                    .child(self.check("f-lower", "Mask lower-case letters", o.mask_lowercase, |o| o.mask_lowercase ^= true, cx))
                    .child(self.check(
                        "f-short",
                        "Adjust for short queries",
                        o.short_query,
                        |o| o.short_query ^= true,
                        cx,
                    ))
                    .into_any_element(),
            ))
            .child(heading("JOB"))
            .child(row("Title", input(&self.title).into_any_element()));
        if p == Program::Tblastx && self.region.bases.len() > super::TBLASTX_LONG {
            d = d.child(
                div().text_xs().text_color(theme::error()).child(format!(
                    "TBLASTX translates both sides in six frames; NCBI may take a long time or refuse a {} bp query. \
                     Consider BLASTX, or a shorter region.",
                    thousands(self.region.bases.len())
                )),
            );
        }
        d
    }

    fn log(&mut self, cx: &mut Context<Self>, tall: bool) -> AnyElement {
        let m = self.manager(cx);
        let m = m.read(cx);
        let Some(t) = m.task(self.id) else { return div().into_any_element() };
        if t.log.len() != self.log_seen {
            self.log_seen = t.log.len();
            self.log_scroll.scroll_to_bottom();
        }
        let lines: Vec<AnyElement> = t
            .log
            .iter()
            .map(|l| {
                div()
                    .text_color(if l.kind == LogKind::Error { theme::error() } else { theme::text() })
                    .child(l.text.clone())
                    .into_any_element()
            })
            .collect();
        div()
            .id("blast-log")
            .h(px(if tall { 300.0 } else { 160.0 }))
            .overflow_y_scroll()
            .track_scroll(&self.log_scroll)
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::editor_bg())
            .font(editor_font())
            .text_xs()
            .children(lines)
            .into_any_element()
    }

    fn table(&self, res: &Arc<BlastResults>, cx: &mut Context<Self>) -> AnyElement {
        let (by, asc) = self.sort;
        let order = Rc::new(results::sorted(&res.hits, by, asc, res.query_len));
        let width = |c: Column| match c {
            Column::Description => None,
            Column::Organism => Some(170.0),
            Column::Accession => Some(105.0),
            Column::Evalue => Some(70.0),
            _ => Some(64.0),
        };
        let cell = move |c: Column, d: Div| match width(c) {
            None => d.flex_1().min_w_0(),
            Some(w) => d.w(px(w)).flex_none(),
        };
        let header = div().flex().flex_row().gap_2().px_2().py_1().border_b_1().border_color(theme::border()).children(
            Column::ALL.iter().enumerate().map(|(i, &c)| {
                let arrow = if c == by { if asc { " ▲" } else { " ▼" } } else { "" };
                cell(c, div())
                    .id(("col", i))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if c == by { theme::text() } else { theme::muted_text() })
                    .cursor_pointer()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(format!("{}{arrow}", c.title()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort = if this.sort.0 == c { (c, !this.sort.1) } else { (c, c.default_ascending()) };
                        cx.notify();
                    }))
            }),
        );
        let res2 = res.clone();
        let list = uniform_list(
            "blast-hits",
            order.len(),
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|row| {
                        let ix = order[row];
                        let hit = &res2.hits[ix];
                        div()
                            .id(("hit", ix))
                            .flex()
                            .flex_row()
                            .gap_2()
                            .px_2()
                            .h(px(24.0))
                            .items_center()
                            .text_xs()
                            .cursor_pointer()
                            .when(row % 2 == 1, |d| d.bg(theme::app_bg()))
                            .hover(|s| s.bg(theme::hover()))
                            .children(Column::ALL.iter().map(|&c| {
                                cell(c, div()).overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.cell(hit, res2.query_len))
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open = Some(ix);
                                this.align_scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                                cx.notify();
                            }))
                    })
                    .collect()
            }),
        )
        .track_scroll(self.table_scroll.clone())
        .h(px(380.0));
        div()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::editor_bg())
            .child(header)
            .child(list)
            .into_any_element()
    }

    fn alignment(&self, res: &Arc<BlastResults>, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(hit) = res.hits.get(ix) else { return div().into_any_element() };
        let program = res.options.program;
        let acc = hit.accession_version();
        let protein = program.subject_aa();
        let link = |id: SharedString, text: String| {
            div().id(id).text_xs().text_color(theme::accent()).cursor_pointer().hover(|s| s.underline()).child(text)
        };
        let url = super::record_url(&acc, protein);
        let mut hsps = div().flex().flex_col().gap_4();
        for (h, hsp) in hit.hsps.iter().enumerate() {
            let blocks = results::blocks(hsp, program);
            let w = results::coord_width(&blocks);
            let summary = results::hsp_summary(hsp, program);
            let (lo, hi) = hsp.query_span();
            hsps = hsps.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(format!(
                                "Match {} of {} · query {}..{}",
                                h + 1,
                                hit.hsps.len(),
                                thousands(lo),
                                thousands(hi)
                            )))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id(("convert", h))
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::accent())
                                    .text_xs()
                                    .text_color(theme::accent())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::hover()))
                                    .child("Create feature from match…")
                                    .on_click(cx.listener(move |this, _, window, cx| this.convert(ix, h, window, cx))),
                            ),
                    )
                    .children(summary.into_iter().map(|s| div().text_xs().text_color(theme::muted_text()).child(s)))
                    .child(
                        div().pt_1().font(editor_font()).text_xs().flex().flex_col().children(blocks.iter().map(|b| {
                            let [q, m, s] = results::block_lines(b, w);
                            div()
                                .pb_2()
                                .flex()
                                .flex_col()
                                .child(div().whitespace_nowrap().child(q))
                                .child(div().whitespace_nowrap().text_color(theme::muted_text()).child(m))
                                .child(div().whitespace_nowrap().child(s))
                        })),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(link("back".into(), "← All results".into()).on_click(cx.listener(|this, _, _, cx| {
                        this.open = None;
                        cx.notify();
                    })))
                    .child(div().flex_1())
                    .child(link("record".into(), format!("{acc} at NCBI ↗")).on_click(move |_, _, cx| cx.open_url(&url))),
            )
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(hit.title()))
            .child(div().text_xs().text_color(theme::muted_text()).child(format!(
                "{} · {}{} · length {}",
                acc,
                if hit.sciname().is_empty() { "organism unknown".to_string() } else { hit.sciname() },
                hit.first().and_then(|d| d.taxid).map(|t| format!(" (taxid {t})")).unwrap_or_default(),
                thousands(hit.len)
            )))
            .child(
                div()
                    .id("blast-align")
                    .h(px(400.0))
                    .overflow_y_scroll()
                    .overflow_x_scroll()
                    .track_scroll(&self.align_scroll)
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::editor_bg())
                    .child(hsps),
            )
            .into_any_element()
    }
}

use gpui::App;
use std::rc::Rc;

impl Render for BlastView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let manager = self.manager(cx);
        let (status, title, progress, ahead) = {
            let m = manager.read(cx);
            match m.task(self.id) {
                Some(t) => (t.status.clone(), t.title.clone(), t.progress.clone(), m.ahead("ncbi")),
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
        let background = || {
            button("background", "Background".into(), false)
                .on_click(|_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.background(cx)))
        };
        let dismiss = |text: &str| {
            button("dismiss", text.into(), false).on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.dismiss(id, cx)))
        };

        let res = self.results(cx);
        let mut footer = div().flex().flex_row().items_center().gap_2();
        let body: AnyElement = match &status {
            TaskStatus::Draft => {
                let email_missing = settings::shared().email.is_none_or(|e| e.trim().is_empty());
                footer = footer
                    .child(div().flex_1().text_xs().text_color(theme::muted_text()).child(if ahead > 0 {
                        format!("{ahead} NCBI search{} ahead; this one will wait its turn.", if ahead == 1 { "" } else { "es" })
                    } else {
                        "NCBI runs one search at a time from here.".to_string()
                    }))
                    .child(button("cancel", "Cancel".into(), false).on_click(|_, _, cx| {
                        tasks::manager(cx).update(cx, |m, cx| m.background(cx))
                    }))
                    .child(
                        button("run", if ahead > 0 { "Queue".into() } else { "Run BLAST".into() }, true)
                            .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                    );
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(email_missing, |d| {
                        d.child(
                            div()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(theme::error())
                                .text_xs()
                                .child("NCBI asks for a contact email with each search. Add yours in Settings → Keys."),
                        )
                    })
                    .child(self.form(cx))
                    .into_any_element()
            }
            TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling => {
                let cancelling = status == TaskStatus::Cancelling;
                footer = footer.child(div().flex_1()).when(!cancelling, |f| {
                    f.child(
                        button("cancel", "Cancel search".into(), false)
                            .on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.cancel(id, cx))),
                    )
                });
                footer = footer.child(background());
                let line = match &status {
                    TaskStatus::Queued => format!("Queued: waiting for {ahead} search{} ahead", if ahead == 1 { "" } else { "es" }),
                    TaskStatus::Cancelling => "Cancelling…".into(),
                    _ => progress.unwrap_or_else(|| "Running…".into()),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(line))
                    .child(self.log(cx, true))
                    .child(div().text_xs().text_color(theme::muted_text()).child(
                        "The searched bases are locked until the results are dismissed. You can background this and keep working.",
                    ))
                    .into_any_element()
            }
            TaskStatus::Done => {
                footer = footer
                    .child(
                        div()
                            .id("toggle-log")
                            .text_xs()
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .child(if self.show_log { "Hide log" } else { "Show log" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_log ^= true;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(dismiss("Dismiss results"))
                    .child(button("close", "Close".into(), true).on_click(|_, _, cx| {
                        tasks::manager(cx).update(cx, |m, cx| m.background(cx))
                    }));
                let mut d = div().flex().flex_col().gap_2();
                if let Some(res) = &res {
                    let report = res.report_url();
                    d = d.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .text_xs()
                            .child(div().text_color(theme::muted_text()).child(format!(
                                "{} hit{} · {} · {} · RID {}",
                                res.hits.len(),
                                if res.hits.len() == 1 { "" } else { "s" },
                                res.options.program.title(),
                                res.database,
                                res.rid
                            )))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("report")
                                    .text_color(theme::accent())
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .child("NCBI report ↗")
                                    .on_click(move |_, _, cx| cx.open_url(&report)),
                            ),
                    );
                    d = if res.hits.is_empty() {
                        d.child(div().py_6().text_sm().text_color(theme::muted_text()).child(
                            res.message.clone().unwrap_or_else(|| "No significant similarity found.".into()),
                        ))
                    } else if let Some(ix) = self.open {
                        d.child(self.alignment(res, ix, cx))
                    } else {
                        d.child(self.table(res, cx)).child(
                            div().text_xs().text_color(theme::muted_text()).child("Click a hit to see its alignments."),
                        )
                    };
                }
                if self.show_log {
                    d = d.child(self.log(cx, false));
                }
                d.into_any_element()
            }
            TaskStatus::Failed(why) => {
                footer = footer.child(div().flex_1()).child(dismiss("Dismiss"));
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().text_color(theme::error()).child(format!("The search failed: {why}")))
                    .child(self.log(cx, true))
                    .into_any_element()
            }
            TaskStatus::Cancelled => {
                footer = footer.child(div().flex_1()).child(dismiss("Dismiss"));
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child("Cancelled."))
                    .child(self.log(cx, true))
                    .into_any_element()
            }
        };

        div()
            .id("blast-dialog")
            .w(px(if matches!(status, TaskStatus::Done) { 980.0 } else { 720.0 }))
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
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("BLAST"))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_sm().text_color(theme::muted_text()).child(
                        if matches!(status, TaskStatus::Draft) { self.region.describe() } else { title },
                    ))
                    .child(div().text_xs().text_color(theme::muted_text()).child(status.label())),
            )
            .child(
                div()
                    .id("blast-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .py_3()
                    .child(body)
                    .children(self.error.clone().map(|e| div().pt_2().text_sm().text_color(theme::error()).child(e))),
            )
            .child(div().px_4().py_3().border_t_1().border_color(theme::border()).child(footer))
    }
}

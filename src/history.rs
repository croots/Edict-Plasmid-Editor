//! The History pane: the document's edit log, newest first, with its save points.
//!
//! The log (see `pgx`) records every change to the bases, undo and redo included, each
//! with the bases it inserted or removed. Clicking an entry selects where that change
//! sits in today's sequence, found by carrying its position forward through every
//! later edit. Feature changes aren't in the log, so they don't appear here.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{div, prelude::*, px, uniform_list, Context, Entity, FontWeight, SharedString, Subscription, Window};

use crate::editor::{editor_font, thousands, SeqEditor};
use crate::model::{Edit, Format, Op};
use crate::pgx::SavePoint;
use crate::theme;

const ROW_H: f32 = 38.0;
/// How many bases of an edit the list shows before eliding the rest.
const PREVIEW_BASES: usize = 40;

/// One line of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// The document as it is now: its revision, and whether that's been saved.
    Now { rev: u64, unsaved: bool },
    /// `history[ix]`.
    Edit { ix: usize },
    Save { rev: u64, time: u64, label: Option<String> },
    /// Below the oldest edit: how far back the log goes.
    Start { edits: usize },
}

/// Newest first. A save point sits just above the edit it was saved at, so everything
/// between two save lines went into the later save.
pub fn rows(history: &[Edit], saves: &[SavePoint], revision: u64, dirty: bool) -> Vec<Row> {
    let unsaved = unsaved(saves, revision, dirty);
    let mut out = vec![Row::Now { rev: revision, unsaved }];
    let mut saves: Vec<&SavePoint> = saves.iter().collect();
    saves.sort_by_key(|s| std::cmp::Reverse((s.rev, s.time)));
    let mut saves = saves.into_iter().peekable();
    for (ix, e) in history.iter().enumerate().rev() {
        while let Some(s) = saves.next_if(|s| s.rev >= e.rev) {
            out.push(Row::Save { rev: s.rev, time: s.time, label: s.label.clone() });
        }
        out.push(Row::Edit { ix });
    }
    out.extend(saves.map(|s| Row::Save { rev: s.rev, time: s.time, label: s.label.clone() }));
    out.push(Row::Start { edits: history.len() });
    out
}

fn unsaved(saves: &[SavePoint], revision: u64, dirty: bool) -> bool {
    let last_save = saves.iter().map(|s| s.rev).max();
    dirty || last_save.map_or(revision > 0, |r| r < revision)
}

/// Carry `[s, e)` across `op` the way the document moves feature segments: an insert at
/// or before the start shifts it, one inside it grows it, and a delete shrinks it (to
/// an empty span where it removed all of it).
fn map_span((s, e): (usize, usize), op: &Op) -> (usize, usize) {
    match op {
        Op::Insert { pos, bases } => {
            let n = bases.len();
            if *pos <= s {
                (s + n, e + n)
            } else if *pos < e {
                (s, e + n)
            } else {
                (s, e)
            }
        }
        Op::Delete { pos, bases } => {
            let (a, b) = (*pos, pos + bases.len());
            let m = |x: usize| if x <= a { x } else if x >= b { x - (b - a) } else { a };
            (m(s), m(e))
        }
    }
}

/// Where `history[ix]` is in the current sequence: the bases it inserted, or the gap a
/// deletion left (an empty span). `None` if `ix` is out of range.
pub fn locate(history: &[Edit], ix: usize, len: usize) -> Option<(usize, usize)> {
    let edit = history.get(ix)?;
    let span = match &edit.op {
        Op::Insert { pos, bases } => (*pos, pos + bases.len()),
        Op::Delete { pos, .. } => (*pos, *pos),
    };
    let (s, e) = history[ix + 1..].iter().fold(span, |span, later| map_span(span, &later.op));
    Some((s.min(len), e.min(len)))
}

/// What an edit did, in a few words, with 1-based positions.
pub fn summary(op: &Op) -> String {
    match op {
        Op::Insert { pos, bases } => format!("Inserted {} bp at {}", thousands(bases.len()), thousands(pos + 1)),
        Op::Delete { pos, bases } if bases.len() == 1 => format!("Deleted base {}", thousands(pos + 1)),
        Op::Delete { pos, bases } => {
            format!("Deleted {} bp, {}–{}", thousands(bases.len()), thousands(pos + 1), thousands(pos + bases.len()))
        }
    }
}

fn preview(bases: &str) -> String {
    if bases.len() <= PREVIEW_BASES {
        bases.to_string()
    } else {
        format!("{}…", &bases[..PREVIEW_BASES])
    }
}

/// `time` relative to `now` (both Unix seconds): "just now", "5 min ago", "3 h ago",
/// "2 d ago", then the UTC date.
pub fn when(time: u64, now: u64) -> String {
    if time == 0 {
        return String::new();
    }
    let d = now.saturating_sub(time);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86400 => format!("{} h ago", d / 3600),
        86400..604800 => format!("{} d ago", d / 86400),
        _ => {
            let (y, m, day) = civil_from_days((time / 86400) as i64);
            format!("{y:04}-{m:02}-{day:02}")
        }
    }
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub struct HistoryView {
    editor: Entity<SeqEditor>,
    /// The rows, a copy of the log they index, and the (document version, revision,
    /// unsaved) they were built for.
    cached: Option<((u64, u64, bool), Rc<Vec<Row>>, Rc<[Edit]>)>,
    /// The revision of the entry last clicked.
    selected: Option<u64>,
    _observe: Subscription,
}

impl HistoryView {
    pub fn new(editor: Entity<SeqEditor>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&editor, |_, _, cx| cx.notify());
        Self { editor, cached: None, selected: None, _observe: observe }
    }

    fn rows(&mut self, cx: &mut Context<Self>) -> (Rc<Vec<Row>>, Rc<[Edit]>) {
        let doc = &self.editor.read(cx).doc;
        // A save changes `dirty` and the save points without a new version.
        let key = (doc.version, doc.revision, unsaved(&doc.saves, doc.revision, doc.dirty));
        if let Some((k, rows, history)) = &self.cached {
            if *k == key {
                return (rows.clone(), history.clone());
            }
        }
        let rows = Rc::new(rows(&doc.history, &doc.saves, doc.revision, doc.dirty));
        let history: Rc<[Edit]> = doc.history.clone().into();
        self.cached = Some((key, rows.clone(), history.clone()));
        (rows, history)
    }

    /// Select where edit `ix` is now in the sequence editor.
    fn reveal(&mut self, ix: usize, cx: &mut Context<Self>) {
        let doc = &self.editor.read(cx).doc;
        let Some(edit) = doc.history.get(ix) else { return };
        let Some((s, e)) = locate(&doc.history, ix, doc.len()) else { return };
        let mut message = format!("Revision {} · {}", edit.rev, summary(&edit.op).to_lowercase());
        if matches!(edit.op, Op::Insert { .. }) && s == e {
            message.push_str(" · those bases were removed later");
        }
        self.selected = Some(edit.rev);
        self.editor.update(cx, |ed, cx| ed.select(s, e, false, Some(message.into()), cx));
        cx.notify();
    }
}

impl Render for HistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (rows, history) = self.rows(cx);
        let format = self.editor.read(cx).doc.format;
        let selected = self.selected;
        let now = now();

        let notice = (format != Format::Native).then(|| {
            div()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(theme::border())
                .text_xs()
                .text_color(theme::muted_text())
                .child(format!(
                    "{} files don't keep edit history: these edits are lost when this file is saved. Save As .pgx to keep them.",
                    format.name()
                ))
        });

        let list = uniform_list(
            "history-rows",
            rows.len(),
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|i| {
                        let base = div().id(("history-row", i)).h(px(ROW_H)).px_3().flex().flex_row().items_center().gap_2();
                        match &rows[i] {
                            Row::Now { rev, unsaved } => base
                                .border_b_1()
                                .border_color(theme::border())
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Now"))
                                .child(div().text_xs().text_color(theme::muted_text()).child(format!("revision {rev}")))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if *unsaved { theme::accent() } else { theme::muted_text() })
                                        .child(if *unsaved { "unsaved changes" } else { "saved" }),
                                ),
                            Row::Save { rev, time, label } => {
                                let title: SharedString = match label {
                                    Some(l) => format!("Saved · {l}").into(),
                                    None => "Saved".into(),
                                };
                                base.child(div().w(px(14.0)).flex().justify_center().text_color(theme::accent()).child("●"))
                                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme::accent()).child(title))
                                    .child(div().h(px(1.0)).flex_1().bg(theme::border()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::muted_text())
                                            .child(format!("rev {rev} · {}", when(*time, now))),
                                    )
                            }
                            Row::Edit { ix } => {
                                let ix = *ix;
                                let e = &history[ix];
                                let (sign, color, bases) = match &e.op {
                                    Op::Insert { bases, .. } => ("+", theme::accent(), bases),
                                    Op::Delete { bases, .. } => ("−", theme::error(), bases),
                                };
                                let is_selected = selected == Some(e.rev);
                                base.cursor_pointer()
                                    .when(is_selected, |d| d.bg(theme::active()))
                                    .hover(|s| s.bg(theme::hover()))
                                    .child(
                                        div()
                                            .w(px(14.0))
                                            .flex()
                                            .justify_center()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(color)
                                            .child(sign),
                                    )
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
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .text_ellipsis()
                                                            .when(is_selected, |d| d.text_color(theme::accent()))
                                                            .child(summary(&e.op)),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_shrink_0()
                                                            .text_xs()
                                                            .text_color(theme::muted_text())
                                                            .child(format!("rev {} · {}", e.rev, when(e.time, now))),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .overflow_hidden()
                                                    .whitespace_nowrap()
                                                    .text_ellipsis()
                                                    .font(editor_font())
                                                    .text_xs()
                                                    .text_color(theme::muted_text())
                                                    .child(preview(bases)),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| this.reveal(ix, cx)))
                            }
                            Row::Start { edits } => base.justify_center().text_xs().text_color(theme::muted_text()).child(
                                if *edits == 0 {
                                    "No sequence edits yet. Changes to the bases will be listed here.".to_string()
                                } else {
                                    format!(
                                        "Start of history · {} edit{} · feature changes aren't recorded",
                                        thousands(*edits),
                                        if *edits == 1 { "" } else { "s" }
                                    )
                                },
                            ),
                        }
                    })
                    .collect()
            }),
        )
        .flex_1();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::editor_bg())
            .text_color(theme::text())
            .children(notice)
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Document;

    fn edit(rev: u64, op: Op) -> Edit {
        Edit { rev, time: 0, op }
    }

    fn ins(pos: usize, b: &str) -> Op {
        Op::Insert { pos, bases: b.into() }
    }

    fn del(pos: usize, b: &str) -> Op {
        Op::Delete { pos, bases: b.into() }
    }

    #[test]
    fn rows_put_save_points_above_the_edit_they_saved() {
        let history = vec![edit(1, ins(0, "A")), edit(2, ins(0, "C")), edit(3, del(0, "C"))];
        let saves = vec![SavePoint { rev: 2, time: 5, label: None }, SavePoint { rev: 0, time: 1, label: None }];
        let r = rows(&history, &saves, 3, true);
        assert_eq!(
            r,
            vec![
                Row::Now { rev: 3, unsaved: true },
                Row::Edit { ix: 2 },
                Row::Save { rev: 2, time: 5, label: None },
                Row::Edit { ix: 1 },
                Row::Edit { ix: 0 },
                Row::Save { rev: 0, time: 1, label: None },
                Row::Start { edits: 3 },
            ]
        );
        let saved = vec![SavePoint { rev: 3, time: 9, label: None }];
        assert_eq!(rows(&history, &saved, 3, false)[0], Row::Now { rev: 3, unsaved: false });
        assert_eq!(rows(&[], &[], 0, false), vec![Row::Now { rev: 0, unsaved: false }, Row::Start { edits: 0 }]);
    }

    /// Replaying the log for real and comparing against `locate` keeps the two honest.
    #[test]
    fn locate_follows_an_edit_through_later_ones() {
        let mut d = Document::new_untitled(std::path::Path::new("x.pgx"));
        d.insert(0, b"AAAAAAAAAA");
        d.insert(4, b"GGG"); // AAAAGGGAAAAAA, ix 1
        d.insert(0, b"TT"); // shifts it right by 2
        d.insert(7, b"C"); // inside the GGG: grows it
        d.delete(12, 14); // after it: no effect
        d.delete(1, 2); // before it: shifts it left by 1
        let len = d.len();
        let (s, e) = locate(&d.history, 1, len).unwrap();
        assert_eq!(&d.seq[s..e], b"GCGG");

        // A deletion is a gap: here, between the bases on either side of it.
        let ix = d.history.len() - 2;
        let (s, e) = locate(&d.history, ix, len).unwrap();
        assert_eq!(s, e);
        assert_eq!(s, 11);

        // Bases later removed collapse to where they were.
        d.delete(0, d.len());
        assert_eq!(locate(&d.history, 1, d.len()), Some((0, 0)));
        assert_eq!(locate(&d.history, 99, d.len()), None);
    }

    #[test]
    fn undo_appears_as_its_own_edit() {
        let mut d = Document::new_untitled(std::path::Path::new("x.pgx"));
        d.insert(0, b"ACGT");
        d.undo();
        let r = rows(&d.history, &d.saves, d.revision, d.dirty);
        assert_eq!(r.len(), 4);
        assert_eq!(summary(&d.history[1].op), "Deleted 4 bp, 1–4");
        assert_eq!(summary(&d.history[0].op), "Inserted 4 bp at 1");
        assert_eq!(summary(&del(1233, "A")), "Deleted base 1,234");
    }

    #[test]
    fn times_read_naturally() {
        let now = 1_795_000_000;
        assert_eq!(when(now - 5, now), "just now");
        assert_eq!(when(now - 600, now), "10 min ago");
        assert_eq!(when(now - 7200, now), "2 h ago");
        assert_eq!(when(now - 3 * 86400, now), "3 d ago");
        assert_eq!(when(0, now), "");
        // 2026-11-23 00:00 UTC
        assert_eq!(when(1_795_392_000, now + 86400 * 400), "2026-11-23");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn previews_are_capped() {
        assert_eq!(preview("ACGT"), "ACGT");
        let long = "A".repeat(PREVIEW_BASES + 5);
        assert_eq!(preview(&long).chars().count(), PREVIEW_BASES + 1);
    }
}

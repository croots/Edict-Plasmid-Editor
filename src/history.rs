//! The History pane: the document's change log, one entry per save, newest first.
//!
//! The log (see `edna`) records every change a save keeps: bases (with the bases
//! inserted or removed), features, attached structures and the record's name, undo
//! and redo included, each with who made it. The pane squashes everything between two
//! saves into one entry showing its net effect, so typing a base and deleting it again
//! leaves nothing, and an entry expands to the individual changes. Clicking selects
//! where a change sits in today's sequence, found by carrying its position forward
//! through every later edit. Right-clicking offers reverting the document to the
//! version before or after an entry, as one undo step.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    anchored, deferred, div, prelude::*, px, uniform_list, Context, Entity, FontWeight, Hsla, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Subscription, Window,
};

use crate::editor::{editor_font, menu_panel, thousands, SeqEditor};
use crate::model::{Edit, FeatureRecord, Format, Op};
use crate::edna::{ChainLinking, SavePoint};
use crate::theme;

const ROW_H: f32 = 40.0;
/// How many bases of an edit the list shows before eliding the rest.
const PREVIEW_BASES: usize = 40;

/// The changes that went into one save, or the ones not saved yet (`save: None`).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub save: Option<SavePoint>,
    /// Indices into the log.
    pub edits: Range<usize>,
}

impl Group {
    /// Stable across rebuilds: the save's revision, or `u64::MAX` for unsaved changes.
    fn id(&self) -> u64 {
        self.save.as_ref().map_or(u64::MAX, |s| s.rev)
    }
}

/// Groups the log by save, newest first. A save made without new changes (saving
/// twice, or the first save of a new file) adds no group.
pub fn groups(history: &[Edit], saves: &[SavePoint]) -> Vec<Group> {
    let mut saves: Vec<&SavePoint> = saves.iter().collect();
    saves.sort_by_key(|s| (s.rev, s.time));
    let mut out = Vec::new();
    let mut start = 0;
    for s in saves {
        let end = history.partition_point(|e| e.rev <= s.rev).max(start);
        if end > start {
            out.push(Group { save: Some(s.clone()), edits: start..end });
            start = end;
        }
    }
    if start < history.len() {
        out.push(Group { save: None, edits: start..history.len() });
    }
    out.reverse();
    out
}

/// What a group of edits did to the bases, all told: `removed` bases at `pos` became
/// `added` bases, in the coordinates of the group's end.
#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub pos: usize,
    pub removed: usize,
    pub added: usize,
}

impl Net {
    fn between(before: &[u8], after: &[u8]) -> Option<Net> {
        if before == after {
            return None;
        }
        let p = before.iter().zip(after).take_while(|(a, b)| a == b).count();
        let s = before[p..].iter().rev().zip(after[p..].iter().rev()).take_while(|(a, b)| a == b).count();
        Some(Net { pos: p, removed: before.len() - p - s, added: after.len() - p - s })
    }

    pub fn describe(&self) -> String {
        let span = |n: usize| format!("{}–{}", thousands(self.pos + 1), thousands(self.pos + n));
        match (self.removed, self.added) {
            (0, n) => format!("Inserted {} bp at {}", thousands(n), thousands(self.pos + 1)),
            (1, 0) => format!("Deleted base {}", thousands(self.pos + 1)),
            (n, 0) => format!("Deleted {} bp, {}", thousands(n), span(n)),
            (r, a) => format!("Replaced {} bp at {} with {} bp", thousands(r), span(r), thousands(a)),
        }
    }
}

/// A group's effect: its net change to the bases, the other changes that weren't
/// undone within it, and who made them.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Summary {
    pub net: Option<Net>,
    pub other: Vec<Op>,
    pub authors: Vec<String>,
}

fn is_feature(op: &Op) -> bool {
    matches!(op, Op::AddFeature { .. } | Op::RemoveFeature { .. } | Op::EditFeature { .. })
}

impl Summary {
    pub fn describe(&self) -> String {
        let reverts = self.other.iter().filter(|o| matches!(o, Op::Revert { .. })).map(summary);
        let mut parts: Vec<String> = reverts.chain(self.net.iter().map(Net::describe)).collect();
        let kinds: [(fn(&Op) -> bool, &str); 3] = [
            (|o| matches!(o, Op::AddFeature { .. }), "added"),
            (|o| matches!(o, Op::RemoveFeature { .. }), "removed"),
            (|o| matches!(o, Op::EditFeature { .. }), "edited"),
        ];
        for (is, verb) in kinds {
            let ops: Vec<&Op> = self.other.iter().filter(|o| is(o)).collect();
            match ops.as_slice() {
                [] => {}
                [one] => parts.push(summary(one)),
                many => parts.push(format!("{} features {verb}", many.len())),
            }
        }
        parts.extend(self.other.iter().filter(|o| !is_feature(o) && !matches!(o, Op::Revert { .. })).map(summary));
        if parts.is_empty() {
            "No net change".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// Undo one sequence step on `seq` (other steps don't touch the bases).
fn unapply(seq: &mut Vec<u8>, op: &Op) {
    match op {
        Op::Insert { pos, bases } => {
            let a = (*pos).min(seq.len());
            let b = (pos + bases.len()).min(seq.len());
            seq.drain(a..b);
        }
        Op::Delete { pos, bases } => {
            let a = (*pos).min(seq.len());
            seq.splice(a..a, bases.bytes());
        }
        _ => {}
    }
}

/// The non-sequence changes in `edits` that weren't reversed later within them: an
/// undo logs the exact inverse of the step it undid, and the two cancel.
fn surviving(edits: &[Edit]) -> Vec<Op> {
    let mut kept: Vec<Op> = Vec::new();
    for e in edits.iter().filter(|e| !e.op.is_sequence() && !e.op.is_bookkeeping()) {
        let cancels = e.op.reversal().and_then(|inv| kept.iter().rposition(|k| *k == inv));
        match cancels {
            Some(i) => {
                kept.remove(i);
            }
            None => kept.push(e.op.clone()),
        }
    }
    kept
}

/// Summaries for `groups` (newest first, as [`groups`] returns them), found by
/// replaying the log backwards from `seq`, today's bases. A saved group never changes
/// once written, so `memo` (keyed by its log range) spares summarizing it again, and
/// the replay stops at the oldest group not in it.
pub fn summarize(
    history: &[Edit],
    groups: &[Group],
    seq: &[u8],
    memo: &mut HashMap<(usize, usize), Rc<Summary>>,
) -> Vec<Rc<Summary>> {
    let key = |g: &Group| (g.edits.start, g.edits.end);
    let cached = |g: &Group, memo: &HashMap<_, Rc<Summary>>| g.save.as_ref().and(memo.get(&key(g)).cloned());
    let depth = groups.iter().rposition(|g| cached(g, memo).is_none()).map_or(0, |i| i + 1);
    let mut cur = seq.to_vec();
    let mut out = Vec::with_capacity(groups.len());
    for (i, g) in groups.iter().enumerate() {
        let edits = &history[g.edits.clone()];
        if let Some(s) = cached(g, memo) {
            if i < depth {
                for e in edits.iter().rev() {
                    unapply(&mut cur, &e.op);
                }
            }
            out.push(s);
            continue;
        }
        let after = cur.clone();
        for e in edits.iter().rev() {
            unapply(&mut cur, &e.op);
        }
        let mut authors: Vec<String> = Vec::new();
        for a in edits.iter().filter_map(|e| e.author.as_ref()).chain(g.save.as_ref().and_then(|s| s.author.as_ref())) {
            if !authors.contains(a) {
                authors.push(a.clone());
            }
        }
        let s = Rc::new(Summary { net: Net::between(&cur, &after), other: surviving(edits), authors });
        if g.save.is_some() {
            memo.insert(key(g), s.clone());
        }
        out.push(s);
    }
    out
}

/// One line of the list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Row {
    /// `groups[g]`.
    Group { g: usize },
    /// `history[ix]`, under its expanded group.
    Edit { ix: usize },
    /// Below the oldest entry: how far back the log goes.
    Start { edits: usize },
}

/// The list's lines. Bookkeeping entries (see [`Op::is_bookkeeping`]) aren't shown.
pub fn rows(history: &[Edit], groups: &[Group], expanded: &HashSet<u64>) -> Vec<Row> {
    let mut out = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        out.push(Row::Group { g });
        if expanded.contains(&group.id()) {
            out.extend(group.edits.clone().rev().filter(|&ix| !history[ix].op.is_bookkeeping()).map(|ix| Row::Edit { ix }));
        }
    }
    out.push(Row::Start { edits: changes(history) });
    out
}

/// How many of `edits` are changes shown in the list.
fn changes(edits: &[Edit]) -> usize {
    edits.iter().filter(|e| !e.op.is_bookkeeping()).count()
}

/// The revision just before `history[ix]`: the state a revert to "before it" restores.
fn rev_before(history: &[Edit], ix: usize) -> u64 {
    match ix.checked_sub(1) {
        Some(p) => history[p].rev,
        None => history.first().map_or(0, |e| e.rev.saturating_sub(1)),
    }
}

/// Carry `[s, e)` across `op` the way the document moves feature segments: an insert at
/// or before the start shifts it, one inside it grows it, and a delete shrinks it (to
/// an empty span where it removed all of it). Other steps leave positions alone.
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
        _ => (s, e),
    }
}

/// Carry `span`, as it stood after `history[..from]`, forward to today's sequence.
fn forward(history: &[Edit], from: usize, span: (usize, usize), len: usize) -> (usize, usize) {
    let (s, e) = history[from.min(history.len())..].iter().fold(span, |span, later| map_span(span, &later.op));
    (s.min(len), e.min(len))
}

/// Where `history[ix]` is in today's sequence: the bases it inserted, the gap a
/// deletion left (an empty span), or the feature it touched. `None` if `ix` is out of
/// range or the change has no place in the sequence (structures, renames).
pub fn locate(history: &[Edit], ix: usize, len: usize) -> Option<(usize, usize)> {
    let edit = history.get(ix)?;
    let span = match &edit.op {
        Op::Insert { pos, bases } => (*pos, pos + bases.len()),
        Op::Delete { pos, .. } => (*pos, *pos),
        Op::AddFeature { feature, .. } | Op::RemoveFeature { feature, .. } | Op::EditFeature { after: feature, .. } => {
            feature.extent()?
        }
        _ => return None,
    };
    Some(forward(history, ix + 1, span, len))
}

fn quoted(f: &FeatureRecord) -> String {
    if f.label.is_empty() {
        f.kind.clone()
    } else {
        format!("“{}”", f.label)
    }
}

/// What a change did, in a few words, with 1-based positions.
pub fn summary(op: &Op) -> String {
    match op {
        Op::Insert { pos, bases } => format!("Inserted {} bp at {}", thousands(bases.len()), thousands(pos + 1)),
        Op::Delete { pos, bases } if bases.len() == 1 => format!("Deleted base {}", thousands(pos + 1)),
        Op::Delete { pos, bases } => {
            format!("Deleted {} bp, {}–{}", thousands(bases.len()), thousands(pos + 1), thousands(pos + bases.len()))
        }
        Op::AddFeature { feature, .. } => format!("Added {} {}", feature.kind, quoted(feature)),
        Op::RemoveFeature { feature, .. } => format!("Removed {} {}", feature.kind, quoted(feature)),
        Op::EditFeature { before, after, .. } => {
            if before.label != after.label {
                format!("Renamed {} to {}", quoted(before), quoted(after))
            } else if before.segments != after.segments {
                format!("Moved {}", quoted(after))
            } else if before.kind != after.kind {
                format!("Made {} a {}", quoted(after), after.kind)
            } else if before.strand != after.strand {
                format!("Flipped {}", quoted(after))
            } else {
                format!("Edited {}", quoted(after))
            }
        }
        Op::AttachStructure { name, .. } => format!("Attached structure “{name}”"),
        Op::RemoveStructure { name, .. } => format!("Removed structure “{name}”"),
        Op::LinkStructure { name, after, .. } => format!("Linked “{name}” by {}", linking(*after)),
        Op::Rename { before, after } => format!("Renamed “{before}” to “{after}”"),
        Op::Revert { to } => format!("Reverted to revision {}", thousands(*to as usize)),
        Op::Features { .. } => "Recorded features".into(),
    }
}

fn linking(l: ChainLinking) -> &'static str {
    match l {
        ChainLinking::Auto => "alignment",
        ChainLinking::Features => "feature qualifiers",
        ChainLinking::None => "nothing (unlinked)",
    }
}

/// The second line of a change: its bases, or the feature's type and place.
fn detail(op: &Op) -> String {
    let place = |f: &FeatureRecord| {
        let mut s = f.kind.clone();
        if let Some((a, b)) = f.extent() {
            s.push_str(&format!(" · {}–{}", thousands(a + 1), thousands(b)));
        }
        match f.strand {
            1 => s.push_str(" · forward"),
            -1 => s.push_str(" · reverse"),
            _ => {}
        }
        s
    };
    match op {
        Op::Insert { bases, .. } | Op::Delete { bases, .. } => preview(bases),
        Op::AddFeature { feature, .. } | Op::RemoveFeature { feature, .. } => place(feature),
        Op::EditFeature { before, after, .. } if before.segments != after.segments => {
            format!("{} → {}", place(before), place(after))
        }
        Op::EditFeature { after, .. } => place(after),
        Op::AttachStructure { format, source, bytes, .. } => {
            let kb = bytes.div_ceil(1024);
            let s = format!("{} · {} KB", format!("{format:?}").to_uppercase(), thousands(kb));
            match source {
                Some(src) => format!("{src} · {s}"),
                None => s,
            }
        }
        Op::LinkStructure { before, .. } => format!("was linked by {}", linking(*before)),
        Op::Revert { .. } => "the bases and features as they were; the changes below did it".into(),
        Op::RemoveStructure { .. } | Op::Rename { .. } | Op::Features { .. } => String::new(),
    }
}

fn mark(op: &Op) -> (&'static str, Hsla) {
    match op {
        Op::Revert { .. } => ("↺", theme::accent()),
        Op::Insert { .. } | Op::AddFeature { .. } | Op::AttachStructure { .. } => ("+", theme::accent()),
        Op::Delete { .. } | Op::RemoveFeature { .. } | Op::RemoveStructure { .. } => ("−", theme::error()),
        _ => ("~", theme::muted_text()),
    }
}

fn preview(bases: &str) -> String {
    if bases.len() <= PREVIEW_BASES {
        bases.to_string()
    } else {
        format!("{}…", &bases[..PREVIEW_BASES])
    }
}

/// `s` with its first letter lower-cased, to follow a "·" mid-sentence.
fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or_default()
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

#[derive(Clone, Copy, PartialEq)]
enum Selected {
    /// A group, by [`Group::id`].
    Group(u64),
    /// An edit, by revision.
    Edit(u64),
}

/// What the list draws, built for one state of the document.
struct Built {
    /// (document version, revision, number of saves, dirty).
    key: (u64, u64, usize, bool),
    groups: Rc<Vec<(Group, Rc<Summary>)>>,
    rows: Rc<Vec<Row>>,
    history: Rc<[Edit]>,
}

/// One entry of a row's right-click menu: put the document back as it was at `rev`.
#[derive(Clone, Debug, PartialEq)]
pub struct Revert {
    pub label: &'static str,
    pub rev: u64,
    /// The version, for the status line: "Reverted to {what}".
    pub what: String,
}

/// The reverts row `row` offers, for a document at `revision`. Reverting to the
/// current revision would do nothing, so it isn't offered.
pub fn reverts(row: Row, groups: &[Group], history: &[Edit], revision: u64, now: u64) -> Vec<Revert> {
    let mut out = Vec::new();
    let mut offer = |label, rev: u64, what: String| {
        if rev < revision {
            out.push(Revert { label, rev, what });
        }
    };
    match row {
        Row::Group { g } => {
            let group = &groups[g];
            let before = rev_before(history, group.edits.start);
            match &group.save {
                Some(s) => {
                    let saved = when(s.time, now);
                    offer("Revert to this save", s.rev, format!("the version saved {saved}"));
                    offer("Revert to before this save", before, format!("the version before the save {saved}"));
                }
                None if group.edits.start == 0 => offer("Discard all changes", before, "the start of the history".into()),
                None => offer("Discard unsaved changes", before, "the last saved version".into()),
            }
        }
        Row::Edit { ix } => {
            let rev = history[ix].rev;
            offer("Revert to just after this change", rev, format!("revision {}", thousands(rev as usize)));
            offer("Revert to just before this change", rev_before(history, ix), format!("just before revision {}", thousands(rev as usize)));
        }
        Row::Start { .. } if !history.is_empty() => {
            offer("Revert to the start of history", rev_before(history, 0), "the start of the history".into())
        }
        Row::Start { .. } => {}
    }
    out
}

struct Menu {
    position: Point<Pixels>,
    items: Vec<Revert>,
}

pub struct HistoryView {
    editor: Entity<SeqEditor>,
    built: Option<Built>,
    memo: HashMap<(usize, usize), Rc<Summary>>,
    expanded: HashSet<u64>,
    selected: Option<Selected>,
    menu: Option<Menu>,
    _observe: Subscription,
}

impl HistoryView {
    pub fn new(editor: Entity<SeqEditor>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&editor, |_, _, cx| cx.notify());
        Self { editor, built: None, memo: HashMap::new(), expanded: HashSet::new(), selected: None, menu: None, _observe: observe }
    }

    fn open_menu(&mut self, row: Row, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (groups, _, history) = self.build(cx);
        let groups: Vec<Group> = groups.iter().map(|(g, _)| g.clone()).collect();
        let revision = self.editor.read(cx).doc.revision;
        let items = reverts(row, &groups, &history, revision, now());
        self.menu = (!items.is_empty()).then_some(Menu { position, items });
        cx.notify();
    }

    fn revert(&mut self, item: &Revert, cx: &mut Context<Self>) {
        self.menu = None;
        self.selected = None;
        self.editor.update(cx, |ed, cx| ed.revert_to(item.rev, &item.what, cx));
        cx.notify();
    }

    fn menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let menu = self.menu.as_ref()?;
        let items = menu.items.iter().enumerate().map(|(i, item)| {
            let item = item.clone();
            div()
                .id(("history-revert", i))
                .flex()
                .flex_row()
                .gap_6()
                .px_3()
                .py(px(3.0))
                .rounded_sm()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(div().flex_1().child(item.label))
                .child(div().text_xs().text_color(theme::muted_text()).child(format!("rev {}", thousands(item.rev as usize))))
                .on_click(cx.listener(move |this, _, _, cx| this.revert(&item, cx)))
        });
        Some(
            deferred(
                anchored().position(menu.position).snap_to_window_with_margin(px(8.0)).child(
                    menu_panel()
                        .id("history-menu")
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            cx.notify();
                        }))
                        .children(items),
                ),
            )
            .with_priority(1),
        )
    }

    fn build(&mut self, cx: &mut Context<Self>) -> (Rc<Vec<(Group, Rc<Summary>)>>, Rc<Vec<Row>>, Rc<[Edit]>) {
        let doc = &self.editor.read(cx).doc;
        // A save adds a save point without a new version.
        let key = (doc.version, doc.revision, doc.saves.len(), doc.dirty);
        if let Some(b) = self.built.as_ref().filter(|b| b.key == key) {
            return (b.groups.clone(), b.rows.clone(), b.history.clone());
        }
        let gs = groups(&doc.history, &doc.saves);
        let sums = summarize(&doc.history, &gs, &doc.seq, &mut self.memo);
        let rows = Rc::new(rows(&doc.history, &gs, &self.expanded));
        let groups: Rc<Vec<_>> = Rc::new(gs.into_iter().zip(sums).collect());
        let history: Rc<[Edit]> = doc.history.clone().into();
        self.built = Some(Built { key, groups: groups.clone(), rows: rows.clone(), history: history.clone() });
        (groups, rows, history)
    }

    fn select(&mut self, span: Option<(usize, usize)>, message: String, cx: &mut Context<Self>) {
        if let Some((s, e)) = span {
            self.editor.update(cx, |ed, cx| ed.select(s, e, false, Some(message.into()), cx));
        }
        cx.notify();
    }

    /// Expand or collapse group `g`, and select its net change to the bases (or the
    /// first feature it touched).
    fn open_group(&mut self, g: &Group, sum: &Summary, cx: &mut Context<Self>) {
        let id = g.id();
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.built = None;
        self.selected = Some(Selected::Group(id));
        let doc = &self.editor.read(cx).doc;
        let span = match &sum.net {
            Some(n) => Some(forward(&doc.history, g.edits.end, (n.pos, n.pos + n.added), doc.len())),
            None => g.edits.clone().rev().filter(|&ix| sum.other.contains(&doc.history[ix].op)).find_map(|ix| locate(&doc.history, ix, doc.len())),
        };
        let title = match &g.save {
            Some(s) => format!("Saved {}", when(s.time, now())),
            None => "Unsaved changes".into(),
        };
        let mut message = format!("{title} · {}", lower_first(&sum.describe()));
        if matches!(&sum.net, Some(n) if n.added > 0) && span.is_some_and(|(s, e)| s == e) {
            message.push_str(" · those bases were removed later");
        }
        self.select(span, message, cx);
    }

    /// Select where edit `ix` is now in the sequence editor.
    fn reveal(&mut self, ix: usize, cx: &mut Context<Self>) {
        let doc = &self.editor.read(cx).doc;
        let Some(edit) = doc.history.get(ix) else { return };
        let span = locate(&doc.history, ix, doc.len());
        let mut message = format!("Revision {} · {}", edit.rev, lower_first(&summary(&edit.op)));
        if matches!(edit.op, Op::Insert { .. }) && span.is_some_and(|(s, e)| s == e) {
            message.push_str(" · those bases were removed later");
        }
        self.selected = Some(Selected::Edit(edit.rev));
        self.select(span, message, cx);
    }
}

fn two_lines(title: impl IntoElement, meta: String, second: impl IntoElement) -> impl IntoElement {
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
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                .child(div().flex_shrink_0().text_xs().text_color(theme::muted_text()).child(meta)),
        )
        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_xs().text_color(theme::muted_text()).child(second))
}

impl Render for HistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (groups, rows, history) = self.build(cx);
        let format = self.editor.read(cx).doc.format;
        let selected = self.selected;
        let expanded = self.expanded.clone();
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
                    "{} files don't keep a change history: these changes are lost when this file is saved. Save As .edna to keep them.",
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
                        let row = rows[i];
                        let el = match row {
                            Row::Group { g } => {
                                let (group, sum) = &groups[g];
                                let open = expanded.contains(&group.id());
                                let is_selected = selected == Some(Selected::Group(group.id()));
                                let n = changes(&history[group.edits.clone()]);
                                let (title, color, time): (SharedString, Hsla, u64) = match &group.save {
                                    Some(s) => (
                                        s.label.as_ref().map_or("Saved".into(), |l| format!("Saved · {l}").into()),
                                        theme::text(),
                                        s.time,
                                    ),
                                    None => ("Unsaved changes".into(), theme::accent(), history[group.edits.end - 1].time),
                                };
                                let meta = format!("{} change{} · {}", thousands(n), if n == 1 { "" } else { "s" }, when(time, now));
                                let mut second = sum.describe();
                                if !sum.authors.is_empty() {
                                    second = format!("{} · {second}", sum.authors.join(", "));
                                }
                                let (group, sum) = (group.clone(), sum.clone());
                                base.cursor_pointer()
                                    .border_b_1()
                                    .border_color(theme::border())
                                    .when(is_selected, |d| d.bg(theme::active()))
                                    .hover(|s| s.bg(theme::hover()))
                                    .child(
                                        div()
                                            .w(px(14.0))
                                            .flex()
                                            .justify_center()
                                            .text_xs()
                                            .text_color(theme::muted_text())
                                            .child(if open { "▾" } else { "▸" }),
                                    )
                                    .child(two_lines(
                                        div().font_weight(FontWeight::SEMIBOLD).text_color(color).child(title),
                                        meta,
                                        second,
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_group(&group, &sum, cx)))
                            }
                            Row::Edit { ix } => {
                                let e = &history[ix];
                                let (sign, color) = mark(&e.op);
                                let is_selected = selected == Some(Selected::Edit(e.rev));
                                let mut meta = format!("rev {}", e.rev);
                                if let Some(a) = &e.author {
                                    meta.push_str(&format!(" · {a}"));
                                }
                                meta.push_str(&format!(" · {}", when(e.time, now)));
                                base.pl(px(26.0))
                                    .cursor_pointer()
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
                                    .child(two_lines(
                                        div().when(is_selected, |d| d.text_color(theme::accent())).child(summary(&e.op)),
                                        meta,
                                        div().when(e.op.is_sequence(), |d| d.font(editor_font())).child(detail(&e.op)),
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| this.reveal(ix, cx)))
                            }
                            Row::Start { edits } => base.justify_center().text_xs().text_color(theme::muted_text()).child(
                                if edits == 0 {
                                    "No changes yet. Edits to the bases, features and structures will be listed here."
                                        .to_string()
                                } else {
                                    format!("Start of history · {} change{}", thousands(edits), if edits == 1 { "" } else { "s" })
                                },
                            ),
                        };
                        el.on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, ev: &MouseDownEvent, _, cx| this.open_menu(row, ev.position, cx)),
                        )
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
            .children(self.menu(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Document, Feature};
    use std::path::Path;

    fn edit(rev: u64, op: Op) -> Edit {
        Edit { rev, time: 0, author: None, op }
    }

    fn ins(pos: usize, b: &str) -> Op {
        Op::Insert { pos, bases: b.into() }
    }

    fn del(pos: usize, b: &str) -> Op {
        Op::Delete { pos, bases: b.into() }
    }

    fn save(rev: u64) -> SavePoint {
        SavePoint { rev, time: rev * 10, author: None, label: None }
    }

    fn feature(label: &str, s: usize, e: usize) -> Feature {
        Feature { kind: "CDS".into(), label: label.into(), segments: vec![(s, e)], strand: 1, source: None }
    }

    /// Summaries for the whole document, oldest group last.
    fn summaries(d: &Document) -> Vec<Rc<Summary>> {
        summarize(&d.history, &groups(&d.history, &d.saves), &d.seq, &mut HashMap::new())
    }

    #[test]
    fn groups_split_the_log_at_saves() {
        let history = vec![edit(1, ins(0, "A")), edit(2, ins(0, "C")), edit(3, del(0, "C"))];
        // Saves at 0 and twice at 2 add nothing of their own.
        let saves = vec![save(2), save(0), save(2)];
        let g = groups(&history, &saves);
        assert_eq!(g, vec![Group { save: None, edits: 2..3 }, Group { save: Some(save(2)), edits: 0..2 }]);
        assert_eq!(groups(&history, &[save(3)]), vec![Group { save: Some(save(3)), edits: 0..3 }]);
        assert_eq!(groups(&[], &[save(0)]), vec![]);

        let mut open = HashSet::new();
        assert_eq!(rows(&history, &g, &open), vec![Row::Group { g: 0 }, Row::Group { g: 1 }, Row::Start { edits: 3 }]);
        open.insert(2);
        assert_eq!(
            rows(&history, &g, &open),
            vec![Row::Group { g: 0 }, Row::Group { g: 1 }, Row::Edit { ix: 1 }, Row::Edit { ix: 0 }, Row::Start { edits: 3 }]
        );
    }

    #[test]
    fn a_save_shows_its_net_change() {
        let mut d = Document::new_untitled(Path::new("x.edna"));
        d.insert(0, b"AAAACCCC");
        d.saves.push(SavePoint { rev: d.revision, time: 1, author: None, label: None });
        // Type three bases, delete one of them again, and undo a stray deletion.
        d.insert(4, b"GGG");
        d.delete(5, 6);
        d.delete(0, 1);
        d.undo();
        let s = summaries(&d);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].net, Some(Net { pos: 4, removed: 0, added: 2 }));
        assert_eq!(s[0].describe(), "Inserted 2 bp at 5");
        assert_eq!(s[1].describe(), "Inserted 8 bp at 1");

        // Typing and deleting the same bases leaves nothing.
        d.saves.push(SavePoint { rev: d.revision, time: 2, author: None, label: None });
        d.insert(2, b"T");
        d.delete(2, 3);
        assert_eq!(summaries(&d)[0].describe(), "No net change");

        assert_eq!(Net { pos: 9, removed: 3, added: 5 }.describe(), "Replaced 3 bp at 10–12 with 5 bp");
        assert_eq!(Net { pos: 0, removed: 1, added: 0 }.describe(), "Deleted base 1");
    }

    #[test]
    fn memoized_saves_match_a_fresh_replay() {
        let mut d = Document::new_untitled(Path::new("x.edna"));
        let mut memo = HashMap::new();
        for round in 0..4u8 {
            d.insert(0, &[b'A' + round; 3]);
            d.delete(1, 2);
            d.saves.push(SavePoint { rev: d.revision, time: round.into(), author: None, label: None });
            d.insert(d.len(), b"T");
            let g = groups(&d.history, &d.saves);
            assert_eq!(summarize(&d.history, &g, &d.seq, &mut memo), summaries(&d));
            d.saves.push(SavePoint { rev: d.revision, time: 9, author: None, label: None });
        }
        assert!(!memo.is_empty());
    }

    #[test]
    fn feature_and_structure_changes_are_logged_and_undo_cancels_them() {
        let mut d = Document::new_untitled(Path::new("x.edna"));
        d.insert(0, b"ACGTACGTACGT");
        d.saves.push(SavePoint { rev: d.revision, time: 1, author: None, label: None });
        d.add_feature(feature("lacZ", 2, 8));
        d.add_feature(feature("oops", 0, 3));
        d.undo();
        let mut moved = feature("lacZ", 4, 10);
        moved.strand = -1;
        d.replace_feature(0, moved);
        d.set_structure_linking("none", ChainLinking::None);

        let s = summaries(&d);
        assert_eq!(s[0].other.len(), 2, "{:?}", s[0].other);
        assert_eq!(s[0].describe(), "Added CDS “lacZ” · Moved “lacZ”");
        assert!(matches!(d.history.last().map(|e| &e.op), Some(Op::EditFeature { .. })));
        let last = d.history.len() - 1;
        assert_eq!(locate(&d.history, last, d.len()), Some((4, 10)));
        assert_eq!(detail(&d.history[last].op), "CDS · 3–8 · forward → CDS · 5–10 · reverse");

        d.insert(0, b"GG");
        assert_eq!(locate(&d.history, last, d.len()), Some((6, 12)), "followed through later inserts");

        d.remove_feature(0);
        d.undo();
        assert_eq!(summaries(&d)[0].other.len(), 2, "remove + undo cancel");
    }

    #[test]
    fn authors_are_recorded_and_listed_once() {
        crate::model::set_author(Some("  ada ".into()));
        let mut d = Document::new_untitled(Path::new("x.edna"));
        d.insert(0, b"ACGT");
        d.delete(0, 1);
        crate::model::set_author(Some("grace".into()));
        d.insert(0, b"T");
        assert_eq!(d.history[0].author.as_deref(), Some("ada"));
        assert_eq!(summaries(&d)[0].authors, vec!["ada".to_string(), "grace".to_string()]);
        crate::model::set_author(None);
        d.insert(0, b"T");
        assert_eq!(d.history.last().unwrap().author, None);
    }

    /// Replaying the log for real and comparing against `locate` keeps the two honest.
    #[test]
    fn locate_follows_an_edit_through_later_ones() {
        let mut d = Document::new_untitled(Path::new("x.edna"));
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
    fn rows_offer_reverts_to_the_versions_around_them() {
        let history = vec![edit(1, ins(0, "A")), edit(2, ins(0, "C")), edit(3, del(0, "C"))];
        let g = groups(&history, &[save(2)]);
        let offered = |row| reverts(row, &g, &history, 3, 0).into_iter().map(|r| (r.label, r.rev)).collect::<Vec<_>>();
        assert_eq!(offered(Row::Group { g: 0 }), [("Discard unsaved changes", 2)]);
        assert_eq!(offered(Row::Group { g: 1 }), [("Revert to this save", 2), ("Revert to before this save", 0)]);
        assert_eq!(offered(Row::Edit { ix: 2 }), [("Revert to just before this change", 2)]);
        assert_eq!(offered(Row::Edit { ix: 0 }), [("Revert to just after this change", 1), ("Revert to just before this change", 0)]);
        assert_eq!(offered(Row::Start { edits: 3 }), [("Revert to the start of history", 0)]);
        assert_eq!(reverts(Row::Start { edits: 0 }, &[], &[], 0, 0), vec![]);

        // Discarding unsaved changes puts the saved bases back, as one undo step.
        let mut d = Document::new_untitled(Path::new("x.edna"));
        d.insert(0, b"ACGT");
        d.saves.push(save(d.revision));
        d.delete(1, 3);
        d.insert(0, b"TT");
        let gs = groups(&d.history, &d.saves);
        let [discard] = reverts(Row::Group { g: 0 }, &gs, &d.history, d.revision, 0).try_into().unwrap();
        assert!(d.revert_to(discard.rev));
        assert_eq!(&d.seq[..], b"ACGT");
        assert_eq!(summary(&d.history.iter().find(|e| matches!(e.op, Op::Revert { .. })).unwrap().op), "Reverted to revision 1");
        d.undo();
        assert_eq!(&d.seq[..], b"TTAT");
    }

    #[test]
    fn undo_appears_as_its_own_edit() {
        let mut d = Document::new_untitled(Path::new("x.edna"));
        d.insert(0, b"ACGT");
        d.undo();
        assert_eq!(d.history.len(), 2);
        assert_eq!(summary(&d.history[1].op), "Deleted 4 bp, 1–4");
        assert_eq!(summary(&d.history[0].op), "Inserted 4 bp at 1");
        assert_eq!(summary(&del(1233, "A")), "Deleted base 1,234");
        assert_eq!(lower_first("Inserted"), "inserted");
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

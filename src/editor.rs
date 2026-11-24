//! The wrapped, double-stranded sequence view.
//!
//! Each wrapped row is one `list` item painted by a `canvas`: top strand, bottom
//! strand, ruler, then however many feature lanes that row needs. Only visible rows
//! are built, so a 50 kb phage scrolls as smoothly as a 3 kb plasmid.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    anchored, canvas, deferred, div, fill, list, point, prelude::*, px, size, AnyElement, App, Bounds, ClipboardItem,
    Context, ContentMask, Div, Entity, FocusHandle, Font, Hitbox, HitboxBehavior, Hsla, KeyDownEvent, ListAlignment,
    ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    PromptLevel, SharedString, Subscription, TextRun, WeakEntity, Window,
};

use crate::feature_dialog::{DialogEvent, FeatureDialog};
use crate::layout::{self, Layout};
use crate::map_layout;
use crate::model::{self, Document, Feature, Format};
use crate::pgx::{self, ChainLinking};
use crate::genetic_code;
use crate::structure::{self, Molecule};
use crate::theme;
use crate::workspace::Dismiss;

const PAD_X: f32 = 20.0;
const PAD_TOP: f32 = 10.0;
const RULER_GAP: f32 = 4.0;
const RULER_H: f32 = 22.0;
const ROW_GAP: f32 = 16.0;
const MIN_FONT: f32 = 9.0;
const MAX_FONT: f32 = 24.0;

pub struct SeqEditor {
    pub doc: Document,
    focus: FocusHandle,
    rows: ListState,
    anchor: usize,
    head: usize,
    /// On a circular sequence, the selection runs the other way round: from the
    /// larger of anchor/head through the origin to the smaller.
    wraps: bool,
    dragging: bool,
    font_size: f32,
    viewport_w: f32,
    cached: Option<(u64, usize, Rc<Layout>)>,
    message: Option<SharedString>,
    /// A background write is in flight; further Ctrl+S presses wait for it.
    saving: bool,
    /// The user chose to keep this file in its non-native format; don't ask on every save.
    keep_format: bool,
    /// The right-click menu, while it's open.
    menu: Option<ContextMenu>,
    /// The open Create/Edit Feature dialog, and the document version it was opened on.
    dialog: Option<(Entity<FeatureDialog>, u64, Subscription)>,
    /// For one document version, the translation of each CDS/ORF feature: (position of
    /// the middle base of each codon, amino acid), sorted by position.
    translations: Option<(u64, Translations)>,
    /// For one document version, the features each automatically linked structure's
    /// chains align to: (structure id, feature indices).
    auto_links: Option<(u64, Vec<(String, Vec<usize>)>)>,
}

/// Things the editor asks of the panes around it.
pub enum EditorEvent {
    /// Show structure `id` in a 3D pane.
    OpenStructure(String),
}

impl gpui::EventEmitter<EditorEvent> for SeqEditor {}

/// A context menu command: the editor, the menu it was chosen from, the window.
type MenuFn = fn(&mut SeqEditor, &ContextMenu, &mut Window, &mut Context<SeqEditor>);

/// An open right-click menu. Which items it offers depends on whether there was a
/// selection when it opened.
struct ContextMenu {
    /// Window coordinates of the click.
    position: Point<Pixels>,
    selection: bool,
    /// The feature that was right-clicked, if any.
    feature: Option<usize>,
    /// The structures linked to the right-clicked feature: (id, name).
    structures: Vec<(String, String)>,
    plugins_open: bool,
}

/// A selected span in sequence coordinates. `start > end` only on a circular
/// sequence, where the span runs from `start` through the origin to `end`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn wraps(&self) -> bool {
        self.start > self.end
    }

    /// The selected ranges in reading order; the second is empty unless it wraps.
    pub fn parts(&self, len: usize) -> [Range<usize>; 2] {
        if self.wraps() {
            [self.start..len, 0..self.end]
        } else {
            [self.start..self.end, 0..0]
        }
    }

    pub fn bp(&self, len: usize) -> usize {
        self.parts(len).iter().map(|r| r.len()).sum()
    }

    /// The selected bases in reading order, joined across the origin.
    pub fn bases(&self, seq: &[u8]) -> Vec<u8> {
        self.parts(seq.len()).iter().flat_map(|r| seq[r.clone()].iter().copied()).collect()
    }

    /// Exactly the extent `start..end` of a map item, whose `end` passes `len` when it
    /// wraps the origin.
    pub fn matches_extent(&self, start: usize, end: usize, len: usize) -> bool {
        !self.is_empty() && self.start == start && self.end == if end > len { end - len } else { end }
    }
}

/// Pixel geometry shared by every row of one frame.
#[derive(Clone, Copy)]
struct Metrics {
    char_w: f32,
    line_h: f32,
    font_size: f32,
    lane_h: f32,
    lanes_top: f32,
}

/// Per feature, the amino acids to draw over it: (middle base of the codon, amino acid),
/// sorted by position. Empty for features that aren't CDS or ORF.
type Translations = Rc<Vec<Vec<(usize, u8)>>>;

fn translation(seq: &[u8], f: &Feature) -> Vec<(usize, u8)> {
    if !genetic_code::is_coding(&f.kind) || f.strand == 0 {
        return Vec::new();
    }
    let (protein, codons) = structure::translate(seq, f);
    let mut aas: Vec<(usize, u8)> = codons.iter().zip(protein).map(|(c, aa)| (c[1], aa)).collect();
    aas.sort_unstable_by_key(|a| a.0);
    aas
}

/// Everything a row's paint closure needs, cloned cheaply (Rc) per frame.
#[derive(Clone)]
struct RowCtx {
    seq: Rc<Vec<u8>>,
    features: Rc<Vec<Feature>>,
    layout: Rc<Layout>,
    translations: Translations,
    m: Metrics,
    selection: Selection,
    cursor: usize,
    focused: bool,
    editor: WeakEntity<SeqEditor>,
    focus: FocusHandle,
}

impl SeqEditor {
    pub fn new(doc: Document, cx: &mut Context<Self>) -> Self {
        let message = (!doc.warnings.is_empty()).then(|| format!("Opened with problems: {}", doc.warnings.join("; ")).into());
        Self {
            doc,
            focus: cx.focus_handle(),
            rows: ListState::new(0, ListAlignment::Top, px(400.0)),
            anchor: 0,
            head: 0,
            wraps: false,
            dragging: false,
            font_size: 13.0,
            viewport_w: 0.0,
            cached: None,
            message,
            saving: false,
            keep_format: false,
            menu: None,
            dialog: None,
            translations: None,
            auto_links: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn selection(&self) -> Selection {
        let (lo, hi) = (self.anchor.min(self.head), self.anchor.max(self.head));
        if self.wraps && self.doc.circular && lo != hi {
            let len = self.doc.len();
            // Touching either end of the sequence, "through the origin" is one range.
            return match (lo == 0, hi == len) {
                (true, _) => Selection { start: hi, end: len },
                (_, true) => Selection { start: 0, end: lo },
                _ => Selection { start: hi, end: lo },
            };
        }
        Selection { start: lo, end: hi }
    }

    pub fn cursor(&self) -> usize {
        self.head
    }

    /// Select from `anchor` to `head` from another view (the map) and scroll it into
    /// sight. With `wraps`, the selection goes the other way round a circular sequence,
    /// through the origin.
    pub fn select(&mut self, anchor: usize, head: usize, wraps: bool, message: Option<SharedString>, cx: &mut Context<Self>) {
        let len = self.doc.len();
        self.anchor = anchor.min(len);
        self.head = head.min(len);
        self.wraps = wraps && self.doc.circular;
        self.message = message;
        // `ListState::scroll_to_reveal_item` counts unmeasured rows as zero height and
        // stops short on long jumps, so put an off-screen row at the top instead.
        let row = self.head.min(self.anchor) / self.bases_per_row();
        let view = self.rows.viewport_bounds();
        let visible = self.rows.bounds_for_item(row).is_some_and(|b| b.top() >= view.top() && b.bottom() <= view.bottom());
        if !visible {
            self.rows.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.0) });
        }
        cx.notify();
    }

    fn layout(&mut self, bases_per_row: usize) -> Rc<Layout> {
        if let Some((v, bpr, l)) = &self.cached {
            if *v == self.doc.version && *bpr == bases_per_row {
                return l.clone();
            }
        }
        let l = Rc::new(layout::compute(self.doc.len(), &self.doc.features, bases_per_row));
        // Row heights changed: remeasure, but keep the same bases at the top of the view.
        let top = self.rows.logical_scroll_top();
        let old_bpr = self.cached.as_ref().map(|c| c.1).unwrap_or(bases_per_row);
        self.rows.reset(l.rows);
        self.rows.scroll_to(ListOffset { item_ix: top.item_ix * old_bpr / bases_per_row, offset_in_item: px(0.0) });
        self.cached = Some((self.doc.version, bases_per_row, l.clone()));
        l
    }

    /// The translations of CDS and ORF features, drawn in place of their bars.
    fn translations(&mut self) -> Translations {
        if let Some((v, t)) = &self.translations {
            if *v == self.doc.version {
                return t.clone();
            }
        }
        let t: Translations = Rc::new(self.doc.features.iter().map(|f| translation(&self.doc.seq, f)).collect());
        self.translations = Some((self.doc.version, t.clone()));
        t
    }

    /// The structures whose chains are linked to feature `fi`: (id, name).
    fn linked_structures(&mut self, fi: usize) -> Vec<(String, String)> {
        let Some(f) = self.doc.features.get(fi) else { return Vec::new() };
        let refs = f.structure_refs();
        if self.auto_links.as_ref().is_none_or(|(v, _)| *v != self.doc.version) {
            let auto = self
                .doc
                .structures
                .iter()
                .filter(|s| s.linking == ChainLinking::Auto)
                .filter_map(|s| {
                    let mol = Molecule::parse(&s.data, s.format).ok()?;
                    let linked = structure::link_chains(&mol, &self.doc.seq, &self.doc.features).into_iter().map(|l| l.feature).collect();
                    Some((s.id.clone(), linked))
                })
                .collect();
            self.auto_links = Some((self.doc.version, auto));
        }
        let auto = &self.auto_links.as_ref().unwrap().1;
        self.doc
            .structures
            .iter()
            .filter(|s| match s.linking {
                ChainLinking::Features => refs.iter().any(|(id, _)| *id == s.id),
                ChainLinking::Auto => auto.iter().any(|(id, fs)| *id == s.id && fs.contains(&fi)),
                ChainLinking::None => false,
            })
            .map(|s| (s.id.clone(), s.source.clone().unwrap_or_else(|| s.name.clone())))
            .collect()
    }

    fn bases_per_row(&self) -> usize {
        self.cached.as_ref().map(|c| c.1).unwrap_or(100)
    }

    fn set_cursor(&mut self, pos: usize, extend: bool) {
        let before = self.head.cmp(&self.anchor);
        self.head = pos.min(self.doc.len());
        if !extend {
            self.anchor = self.head;
        }
        // A wrapped selection stays wrapped while it's extended, until the head crosses
        // the anchor.
        if !extend || self.head.cmp(&self.anchor) != before {
            self.wraps = false;
        }
        self.rows.scroll_to_reveal_item(self.head / self.bases_per_row());
    }

    /// Delete the selection and return where it was, as a cursor position.
    fn delete_selection(&mut self) -> usize {
        let sel = self.selection();
        if sel.wraps() {
            self.doc.delete_across_origin(sel.start, sel.end);
            // What's left starts just after the selection; the gap is now the end.
            self.doc.len()
        } else {
            self.doc.delete(sel.start, sel.end);
            sel.start
        }
    }

    fn replace_selection(&mut self, bases: &[u8]) {
        let at = self.delete_selection();
        self.doc.insert(at, bases);
        self.set_cursor(at + bases.len(), false);
    }

    fn selected_bases(&self) -> Vec<u8> {
        self.selection().bases(&self.doc.seq)
    }

    /// Copy the selection, or its reverse complement. False if nothing was selected.
    fn copy(&mut self, reverse_complement: bool, cx: &mut Context<Self>) -> bool {
        let bases = self.selected_bases();
        if bases.is_empty() {
            return false;
        }
        let bases = if reverse_complement { model::reverse_complement(&bases) } else { bases };
        self.message =
            Some(format!("Copied {} bp{}", bases.len(), if reverse_complement { " (reverse complement)" } else { "" }).into());
        cx.write_to_clipboard(ClipboardItem::new_string(String::from_utf8_lossy(&bases).into_owned()));
        true
    }

    /// Replace the selection (or insert at the cursor) with the clipboard's bases.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else { return };
        let bases = model::sanitize_bases(&text);
        if bases.is_empty() {
            self.message = Some("Nothing to paste: the clipboard has no bases".into());
            return;
        }
        self.replace_selection(&bases);
    }

    /// Open the Create Feature dialog over the selection.
    fn create_feature(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sel = self.selection();
        let len = self.doc.len();
        let mut segments: Vec<(usize, usize)> =
            sel.parts(len).into_iter().filter(|r| !r.is_empty()).map(|r| (r.start, r.end)).collect();
        segments.sort_unstable();
        let doc = &self.doc;
        let dialog = cx.new(|cx| FeatureDialog::create(&segments, doc, window, cx));
        self.show_dialog(dialog, window, cx);
    }

    /// Open the Edit Feature dialog for feature `ix`.
    fn edit_feature(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(feature) = self.doc.features.get(ix).cloned() else { return };
        let doc = &self.doc;
        let dialog = cx.new(|cx| FeatureDialog::edit(ix, &feature, doc, window, cx));
        self.show_dialog(dialog, window, cx);
    }

    fn show_dialog(&mut self, dialog: Entity<FeatureDialog>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(&dialog, window, |this, _, ev: &DialogEvent, window, cx| {
            this.on_dialog(ev, window, cx)
        });
        self.dialog = Some((dialog, self.doc.version, sub));
        cx.notify();
    }

    fn on_dialog(&mut self, ev: &DialogEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, version, _)) = self.dialog.take() else { return };
        window.focus(&self.focus);
        cx.notify();
        let DialogEvent::Confirm { feature, editing, structures } = ev else { return };
        if self.doc.version != version {
            self.message = Some("The sequence changed while the dialog was open; nothing was saved".into());
            return;
        }
        for s in structures {
            self.doc.add_structure(s.clone());
        }
        let feature = feature.clone();
        let bp: usize = feature.segments.iter().map(|(s, e)| e - s).sum();
        let verb = match *editing {
            Some(ix) => {
                self.doc.replace_feature(ix, feature.clone());
                "Updated"
            }
            None => {
                self.doc.add_feature(feature.clone());
                "Added"
            }
        };
        // Select what was just made, then say so.
        if let Some(&(s, e)) = feature.segments.first() {
            self.click(Some((&feature, s, e)), s, false);
        }
        self.message = Some(format!("{verb} {} · {} · {} bp", feature.label, feature.kind, thousands(bp)).into());
    }

    fn delete_feature(&mut self, ix: usize) {
        let Some(f) = self.doc.features.get(ix) else { return };
        self.message = Some(format!("Deleted {} · {}", f.label, f.kind).into());
        self.doc.remove_feature(ix);
        // The bases stay; leave a cursor where the feature started.
        let at = self.selection().start.min(self.doc.len());
        self.set_cursor(at, false);
    }

    /// Make the cursor position the origin (base 1) of a circular sequence.
    fn reindex(&mut self) {
        let at = self.head;
        if !self.doc.circular || at == 0 || at >= self.doc.len() {
            return;
        }
        self.doc.set_origin(at);
        self.message = Some(format!("Base {} is now base 1", thousands(at + 1)).into());
        self.set_cursor(0, false);
    }

    /// Place the cursor, or select a feature, for a click. `feature` is the clicked
    /// feature and the extent of the row piece that was hit.
    fn click(&mut self, feature: Option<(&Feature, usize, usize)>, pos: usize, extend: bool) {
        self.message = None;
        let Some((f, piece_start, piece_end)) = feature else {
            self.head = pos;
            if !extend {
                self.anchor = pos;
            }
            self.wraps = false;
            return;
        };
        let len = self.doc.len();
        let (start, end) = map_layout::feature_extent(&f.segments, len, self.doc.circular);
        let bp;
        if end > len {
            // Wraps the origin: select all of it, through the origin.
            self.anchor = start;
            self.head = end - len;
            self.wraps = true;
            bp = end - start;
        } else {
            // Select the contiguous segment that was clicked.
            let (s, e) = f
                .segments
                .iter()
                .copied()
                .find(|&(s, e)| s <= piece_start && piece_end <= e)
                .unwrap_or((piece_start, piece_end));
            self.anchor = s;
            self.head = e;
            self.wraps = false;
            bp = e - s;
        }
        let mut message = format!("{} · {} · {} bp", f.label, f.kind, bp);
        if genetic_code::is_coding(&f.kind) && f.strand != 0 {
            let table = genetic_code::feature_table(f);
            let name = genetic_code::Code::get(table).map_or("unknown code", |c| c.name);
            let aas = genetic_code::translate(&self.doc.seq, f).0.iter().take_while(|&&a| a != b'*').count();
            message.push_str(&format!(" · {} aa · table {table}, {name}", thousands(aas)));
        }
        self.message = Some(message.into());
    }

    fn open_menu(&mut self, position: Point<Pixels>, feature: Option<usize>) {
        self.dragging = false;
        let structures = feature.map(|fi| self.linked_structures(fi)).unwrap_or_default();
        self.menu = Some(ContextMenu { position, selection: !self.selection().is_empty(), feature, structures, plugins_open: false });
    }

    /// Close the menu, run one of its commands, and redraw.
    fn menu_command(&mut self, window: &mut Window, cx: &mut Context<Self>, f: MenuFn) {
        let Some(menu) = self.menu.take() else { return };
        f(self, &menu, window, cx);
        let len = self.doc.len();
        self.anchor = self.anchor.min(len);
        self.head = self.head.min(len);
        cx.notify();
    }

    fn set_plugins_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(m) = self.menu.as_mut().filter(|m| m.plugins_open != open) {
            m.plugins_open = open;
            cx.notify();
        }
    }

    fn context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let item = |id: &'static str, label: &'static str, hint: &'static str, enabled: bool| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap_6()
                .px_3()
                .py(px(3.0))
                .rounded_sm()
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
                .when(!enabled, |d| d.text_color(theme::muted_text()))
                .child(div().flex_1().child(label))
                .child(div().text_xs().text_color(theme::muted_text()).child(hint))
        };
        let item_owned = |id: (&'static str, usize), label: String| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .py(px(3.0))
                .rounded_sm()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(div().flex_1().child(label))
        };
        let separator = || div().h(px(1.0)).my_1().bg(theme::border());
        // Hovering any other item closes the Plugins submenu.
        let command = |id, label, hint, enabled: bool, f: MenuFn| {
            item(id, label, hint, enabled)
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.set_plugins_open(false, cx);
                    }
                }))
                .when(enabled, |d| d.on_click(cx.listener(move |this, _, window, cx| this.menu_command(window, cx, f))))
                .into_any_element()
        };

        let mut items: Vec<AnyElement> = Vec::new();
        if menu.feature.is_some() {
            items.push(command("menu-edit-feature", "Edit Feature…", "", true, |this, m, window, cx| {
                if let Some(ix) = m.feature {
                    this.edit_feature(ix, window, cx);
                }
            }));
            items.push(command("menu-delete-feature", "Delete Feature", "", true, |this, m, _, _| {
                if let Some(ix) = m.feature {
                    this.delete_feature(ix);
                }
            }));
            for (k, (id, name)) in menu.structures.iter().enumerate() {
                let id = id.clone();
                items.push(
                    item_owned(("menu-open-structure", k), format!("Open Structure · {name}"))
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.set_plugins_open(false, cx);
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            cx.emit(EditorEvent::OpenStructure(id.clone()));
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            items.push(separator().into_any_element());
        }
        items.extend(if menu.selection {
            vec![
                command("menu-copy", "Copy", "Ctrl+C", true, |this, _, _, cx| {
                    this.copy(false, cx);
                }),
                command("menu-copy-rc", "Copy Reverse Complement", "Ctrl+Shift+C", true, |this, _, _, cx| {
                    this.copy(true, cx);
                }),
                command("menu-paste", "Paste", "Ctrl+V", true, |this, _, _, cx| this.paste(cx)),
                command("menu-delete", "Delete", "Del", true, |this, _, _, _| {
                    let at = this.delete_selection();
                    this.set_cursor(at, false);
                }),
                separator().into_any_element(),
                command("menu-create-feature", "Create Feature…", "", true, |this, _, window, cx| {
                    this.create_feature(window, cx)
                }),
            ]
        } else {
            let circular = self.doc.circular;
            let movable = self.head > 0 && self.head < self.doc.len();
            vec![
                command("menu-paste", "Paste", "Ctrl+V", true, |this, _, _, cx| this.paste(cx)),
                command(
                    "menu-reindex",
                    "Reindex",
                    if circular { "Make this base 1" } else { "Circular only" },
                    circular && movable,
                    |this, _, _, _| this.reindex(),
                ),
            ]
        });

        let plugins = div()
            .relative()
            .child(
                item("menu-plugins", "Plugins", "▸", true)
                    .when(menu.plugins_open, |d| d.bg(theme::hover()))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.set_plugins_open(true, cx);
                        }
                    })),
            )
            .when(menu.plugins_open, |d| {
                d.child(
                    menu_panel()
                        .id("editor-plugins-menu")
                        .absolute()
                        .top(px(-5.0))
                        .left_full()
                        .ml_1()
                        .child(item("menu-plugins-none", "No plugins installed", "", false)),
                )
            });

        Some(
            deferred(
                anchored().position(menu.position).snap_to_window_with_margin(px(8.0)).child(
                    menu_panel()
                        .id("editor-context-menu")
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            cx.notify();
                        }))
                        .children(items)
                        .child(separator())
                        .child(plugins),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys the dialog's fields don't use mustn't edit the sequence behind it.
        if self.dialog.is_some() {
            return;
        }
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let cmd = m.control || m.platform;
        let len = self.doc.len();
        let bpr = self.bases_per_row();
        let sel = self.selection();
        self.message = None;
        // Any key closes the context menu; Escape does nothing else.
        let had_menu = self.menu.take().is_some();
        match (cmd, ks.key.as_str()) {
            (_, "escape") if had_menu => {}
            (true, "a") => {
                self.anchor = 0;
                self.head = len;
                self.wraps = false;
            }
            (true, "c") | (true, "x") => {
                if !self.copy(m.shift, cx) {
                    return;
                }
                if ks.key == "x" {
                    let at = self.delete_selection();
                    self.set_cursor(at, false);
                }
            }
            (true, "v") => self.paste(cx),
            (true, "z") if m.shift => {
                self.doc.redo();
                self.wraps = false;
            }
            (true, "z") => {
                self.doc.undo();
                self.wraps = false;
            }
            (true, "y") => {
                self.doc.redo();
                self.wraps = false;
            }
            (true, "s") if m.shift => self.save_as(cx),
            (true, "s") => self.save(window, cx),
            (true, "=") | (true, "+") => self.font_size = (self.font_size + 1.0).min(MAX_FONT),
            (true, "-") => self.font_size = (self.font_size - 1.0).max(MIN_FONT),
            (_, "left") => {
                let to = if !m.shift && !sel.is_empty() { sel.start } else { self.head.saturating_sub(1) };
                self.set_cursor(to, m.shift);
            }
            (_, "right") => {
                let to = if !m.shift && !sel.is_empty() { sel.end } else { self.head + 1 };
                self.set_cursor(to, m.shift);
            }
            (_, "up") => self.set_cursor(self.head.saturating_sub(bpr), m.shift),
            (_, "down") => self.set_cursor(self.head + bpr, m.shift),
            (_, "home") if cmd => self.set_cursor(0, m.shift),
            (_, "end") if cmd => self.set_cursor(len, m.shift),
            (_, "home") => self.set_cursor(self.head / bpr * bpr, m.shift),
            (_, "end") => self.set_cursor(((self.head / bpr + 1) * bpr).min(len), m.shift),
            (_, "pageup") => self.set_cursor(self.head.saturating_sub(bpr * 10), m.shift),
            (_, "pagedown") => self.set_cursor(self.head + bpr * 10, m.shift),
            (_, "backspace") => {
                if !sel.is_empty() {
                    let at = self.delete_selection();
                    self.set_cursor(at, false);
                } else if self.head > 0 {
                    self.doc.delete(self.head - 1, self.head);
                    self.set_cursor(self.head - 1, false);
                }
            }
            (_, "delete") => {
                if !sel.is_empty() {
                    let at = self.delete_selection();
                    self.set_cursor(at, false);
                } else if self.head < len {
                    self.doc.delete(self.head, self.head + 1);
                }
            }
            (false, _) => {
                let typed = ks.key_char.as_deref().unwrap_or("");
                let bases = model::sanitize_bases(typed);
                if bases.is_empty() || typed.chars().count() != 1 {
                    return;
                }
                self.replace_selection(&bases);
            }
            _ => return,
        }
        let len = self.doc.len();
        self.anchor = self.anchor.min(len);
        self.head = self.head.min(len);
        cx.stop_propagation();
        cx.notify();
    }

    /// Ctrl+S. Files in another format ask first whether to convert to `.pgx` (the only
    /// format that keeps edit history) or keep their format. "Keep" lasts for the tab.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc.untitled {
            self.save_as(cx);
            return;
        }
        if self.saving {
            self.message = Some("Still saving…".into());
            return;
        }
        if self.doc.format != Format::Native && !self.keep_format {
            self.ask_convert(window, cx);
            return;
        }
        self.write(cx);
    }

    fn ask_convert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = file_name(&self.doc.path);
        let target = file_name(&self.doc.path.with_extension(pgx::EXTENSION));
        let convert = format!("Convert saves your changes to {target} beside it and leaves {name} untouched.");
        let (detail, answers): (String, &[&str]) = match self.doc.format {
            Format::SnapGene => (
                format!("{name} is a SnapGene file. SnapGene files can be opened but not written. {convert}"),
                &["Convert to .pgx", "Cancel"],
            ),
            Format::Fasta => (
                format!(
                    "FASTA holds only the bases. {convert}\n\nKeep overwrites {name} with the sequence alone: its topology{} and edit history are not saved.",
                    if self.doc.features.is_empty() { "" } else { ", features" }
                ),
                &["Convert to .pgx", "Keep FASTA", "Cancel"],
            ),
            _ => (
                if self.doc.structures.is_empty() {
                    format!(
                        "GenBank keeps the sequence, features and annotations, but not edit history. {convert}\n\nKeep overwrites {name}; this session's edit history is not saved."
                    )
                } else {
                    format!(
                        "GenBank keeps the sequence, features and annotations, but not edit history or attached structures. {convert}\n\nKeep overwrites {name}; this session's edit history and the attached structures are not saved."
                    )
                },
                &["Convert to .pgx", "Keep GenBank", "Cancel"],
            ),
        };
        let can_keep = answers.len() == 3;
        let answer = window.prompt(PromptLevel::Warning, &format!("Save {name} as a .{} file?", pgx::EXTENSION), Some(&detail), answers, cx);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |ed, cx| {
                match answer {
                    Ok(0) => ed.convert(cx),
                    Ok(1) if can_keep => {
                        ed.keep_format = true;
                        ed.write(cx);
                    }
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switch to `name.pgx` beside the current file. If that name is taken, let the
    /// save dialog pick (it asks before overwriting).
    fn convert(&mut self, cx: &mut Context<Self>) {
        let target = self.doc.path.with_extension(pgx::EXTENSION);
        if target.exists() {
            self.save_as_suggesting(Some(file_name(&target)), cx);
            return;
        }
        match self.doc.set_path(&target) {
            Ok(()) => self.write(cx),
            Err(e) => self.message = Some(format!("Save failed: {e:#}").into()),
        }
    }

    /// Serialize on the UI thread (a consistent snapshot, ~1 ms even for lambda), then
    /// write on a background thread: disk latency on a busy drive can be seconds.
    fn write(&mut self, cx: &mut Context<Self>) {
        let bytes = match self.doc.serialize() {
            Ok(b) => b,
            Err(e) => {
                self.message = Some(format!("Save failed: {e:#}").into());
                return;
            }
        };
        let version = self.doc.version;
        self.message = Some("Saving…".into());
        self.write_in_background(self.doc.path.clone(), bytes, cx, move |ed, path, result| match result {
            Ok(()) => {
                ed.doc.mark_saved(version);
                format!("Saved {}", path.display())
            }
            Err(e) => format!("Save failed: {e:#}"),
        });
    }

    fn write_in_background(
        &mut self,
        path: PathBuf,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, &Path, anyhow::Result<()>) -> String + 'static,
    ) {
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let result = cx.background_executor().spawn(async move { model::write_atomic(&target, &bytes) }).await;
            this.update(cx, |ed, cx| {
                ed.saving = false;
                ed.message = Some(done(ed, &path, result).into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ask where to save, then switch the document to that file and save it there.
    /// The extension picks the format; with none, it becomes a `.pgx`.
    pub fn save_as(&mut self, cx: &mut Context<Self>) {
        let suggested = if self.doc.format.writable() {
            file_name(&self.doc.path)
        } else {
            file_name(&self.doc.path.with_extension(pgx::EXTENSION))
        };
        self.save_as_suggesting(Some(suggested), cx);
    }

    fn save_as_suggesting(&mut self, suggested: Option<String>, cx: &mut Context<Self>) {
        if self.saving {
            self.message = Some("Still saving…".into());
            return;
        }
        let dir = self.doc.path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| ".".into());
        let picked = cx.prompt_for_new_path(&dir, suggested.as_deref());
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |ed, cx| {
                match result {
                    Ok(Ok(Some(path))) => {
                        let path = with_default_extension(path);
                        match ed.doc.set_path(&path) {
                            // Picking a format by name is an explicit choice: don't ask again.
                            Ok(()) => {
                                ed.keep_format = true;
                                ed.write(cx);
                            }
                            Err(e) => ed.message = Some(format!("Save failed: {e:#}").into()),
                        }
                    }
                    Ok(Ok(None)) | Err(_) => {}
                    Ok(Err(e)) => ed.message = Some(format!("Could not open the save dialog: {e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Write a copy in another format. The tab keeps editing the original file.
    pub fn export_as(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            self.message = Some("Still saving…".into());
            return;
        }
        let dir = self.doc.path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| ".".into());
        let suggested = format!("{}.gb", self.doc.name);
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let result = picked.await;
            this.update(cx, |ed, cx| {
                match result {
                    Ok(Ok(Some(path))) => ed.export_to(with_default_extension(path), cx),
                    Ok(Ok(None)) | Err(_) => {}
                    Ok(Err(e)) => ed.message = Some(format!("Could not open the save dialog: {e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn export_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let format = Format::from_path(&path).unwrap_or(Format::Native);
        let bytes = match self.doc.serialize_as(format) {
            Ok(b) => b,
            Err(e) => {
                self.message = Some(format!("Export failed: {e:#}").into());
                return;
            }
        };
        self.message = Some("Exporting…".into());
        self.write_in_background(path, bytes, cx, move |_, path, result| match result {
            Ok(()) => format!("Exported {} ({})", path.display(), format.name()),
            Err(e) => format!("Export failed: {e:#}"),
        });
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.font_size = (self.font_size + delta).clamp(MIN_FONT, MAX_FONT);
        cx.notify();
    }

    fn status_bar(&self) -> impl IntoElement {
        let sel = self.selection();
        let mut items: Vec<String> = vec![
            format!("BASES {}", thousands(self.doc.len())),
            if self.doc.circular { "CIRCULAR".into() } else { "LINEAR".into() },
        ];
        if sel.is_empty() {
            items.push(format!("CURSOR {}", thousands(self.head)));
        } else {
            items.push(format!(
                "SELECTION {}–{} ({} bp{})",
                thousands(sel.start + 1),
                thousands(sel.end),
                thousands(sel.bp(self.doc.len())),
                if sel.wraps() { ", across origin" } else { "" }
            ));
            items.push(format!("GC {:.1}%", model::gc_percent(&self.selected_bases())));
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .h(px(26.0))
            .px_3()
            .bg(theme::panel())
            .border_t_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::muted_text())
            .children(items)
            .child(div().flex_1())
            .children(self.message.clone())
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_2()
                .rounded_sm()
                .border_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(32.0))
            .px_3()
            .border_b_1()
            .border_color(theme::border())
            .text_sm()
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(self.doc.name.clone()))
            .child(div().text_color(theme::muted_text()).text_xs().child(format!(
                "{} · {} bp · {} features · {}{}",
                if self.doc.circular { "circular" } else { "linear" },
                thousands(self.doc.len()),
                self.doc.features.len(),
                self.doc.format.name(),
                if self.doc.format.writable() { "" } else { " (read-only)" }
            )))
            .child(div().flex_1())
            .child(div().text_xs().text_color(theme::muted_text()).child("Type A/C/G/T to edit · Ctrl+Shift+C copies reverse complement"))
            .child(button("zoom-out", "−").on_click(cx.listener(|this, _, _, cx| this.zoom(-1.0, cx))))
            .child(button("zoom-in", "+").on_click(cx.listener(|this, _, _, cx| this.zoom(1.0, cx))))
    }
}

impl Render for SeqEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font = editor_font();
        let font_size = self.font_size;
        let ts = window.text_system();
        let font_id = ts.resolve_font(&font);
        let char_w: f32 = ts.advance(font_id, px(font_size), 'A').map(|s| s.width.into()).unwrap_or(font_size * 0.6);
        let line_h = (font_size * 1.35).round();
        let bpr = layout::bases_per_row(self.viewport_w - 2.0 * PAD_X, char_w);
        let layout = self.layout(bpr);
        let lane_h = (font_size * 1.25).round();
        let lanes_top = PAD_TOP + 2.0 * line_h + RULER_GAP + RULER_H;
        let m = Metrics { char_w, line_h, font_size, lane_h, lanes_top };
        let translations = self.translations();

        let ctx = RowCtx {
            seq: self.doc.seq.clone(),
            features: self.doc.features.clone(),
            layout: layout.clone(),
            translations,
            m,
            selection: self.selection(),
            cursor: self.head,
            focused: self.focus.is_focused(window),
            editor: cx.entity().downgrade(),
            focus: self.focus.clone(),
        };

        let me: Entity<Self> = cx.entity();
        let measure = canvas(
            move |bounds, window, cx| {
                let w: f32 = bounds.size.width.into();
                let changed = me.update(cx, |this, _| {
                    let changed = (this.viewport_w - w).abs() > 0.5;
                    this.viewport_w = w;
                    changed
                });
                // A notify during the draw is dropped; re-render once this frame is done.
                if changed {
                    window.on_next_frame(move |_, cx| me.update(cx, |_, cx| cx.notify()));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let rows = list(self.rows.clone(), move |row, _window, _cx| {
            let ctx = ctx.clone();
            let lanes = ctx.layout.row_lanes[row] as f32 * lane_h;
            let row_h = lanes_top + lanes + ROW_GAP;
            div()
                .w_full()
                .h(px(row_h))
                .child(
                    canvas(
                        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                        move |bounds, hitbox, window, cx| paint_row(&ctx, row, bounds, hitbox, window, cx),
                    )
                    .size_full(),
                )
                .into_any_element()
        })
        .size_full();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::editor_bg())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.menu.take().is_some() {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .child(self.toolbar(cx))
            .child(div().relative().flex_1().min_h_0().child(measure).child(rows))
            .child(self.status_bar())
            .children(self.context_menu(cx))
            .children(self.dialog.as_ref().map(|d| d.0.clone()))
    }
}

pub(crate) fn editor_font() -> Font {
    let family = std::env::var("PLASMID_FONT").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") { "Menlo".into() } else { "DejaVu Sans Mono".into() }
    });
    gpui::font(family)
}

fn run(len: usize, color: Hsla) -> TextRun {
    TextRun { len, font: editor_font(), color, background_color: None, underline: None, strikethrough: None }
}

pub(crate) fn paint_text(text: String, x: f32, y: f32, size: f32, line_h: f32, color: Hsla, window: &mut Window, cx: &mut App) -> f32 {
    let len = text.len();
    let shaped = window.text_system().shape_line(text.into(), px(size), &[run(len, color)], None);
    let w: f32 = shaped.width.into();
    shaped.paint(point(px(x), px(y)), px(line_h), window, cx).ok();
    w
}

/// The floating box behind the context menu and its submenu. Occludes, so clicks on
/// it don't reach the sequence rows underneath.
fn menu_panel() -> Div {
    div()
        .occlude()
        .flex()
        .flex_col()
        .min_w(px(220.0))
        .p_1()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::panel())
        .shadow_md()
        .text_sm()
        .text_color(theme::text())
}

fn paint_row(ctx: &RowCtx, row: usize, bounds: Bounds<Pixels>, hitbox: Hitbox, window: &mut Window, cx: &mut App) {
    let m = ctx.m;
    let bpr = ctx.layout.bases_per_row;
    let len = ctx.seq.len();
    let row_start = row * bpr;
    let row_end = (row_start + bpr).min(len);
    let ox: f32 = f32::from(bounds.origin.x) + PAD_X;
    let oy: f32 = f32::from(bounds.origin.y);
    let col = |i: usize| ox + (i - row_start) as f32 * m.char_w;
    let y_top = oy + PAD_TOP;
    let y_bot = y_top + m.line_h;
    let rect = |x0: f32, y0: f32, w: f32, h: f32| Bounds::new(point(px(x0), px(y0)), size(px(w), px(h)));

    // Selection behind both strands.
    let sel = &ctx.selection;
    for part in sel.parts(len) {
        let (sa, sb) = (part.start.max(row_start), part.end.min(row_end));
        if sa < sb {
            window.paint_quad(fill(rect(col(sa), y_top, (sb - sa) as f32 * m.char_w, 2.0 * m.line_h), theme::selection()));
        }
    }

    // Strands.
    if row_start < row_end {
        let top = String::from_utf8_lossy(&ctx.seq[row_start..row_end]).into_owned();
        let bottom: String = ctx.seq[row_start..row_end].iter().map(|&b| model::complement(b) as char).collect();
        paint_text(top, ox, y_top, m.font_size, m.line_h, theme::text(), window, cx);
        paint_text(bottom, ox, y_bot, m.font_size, m.line_h, theme::muted_text(), window, cx);
    }

    // Cursor: a boundary position belongs to the row it starts, except the very end.
    let c = ctx.cursor;
    let owns_cursor = (c >= row_start && c < row_start + bpr) || (c == len && row_end == len);
    if sel.is_empty() && owns_cursor {
        let color = if ctx.focused { theme::cursor() } else { theme::muted_text() };
        window.paint_quad(fill(rect(col(c) - 1.0, y_top - 2.0, 2.0, 2.0 * m.line_h + 4.0), color));
    }

    // Ruler: ticks every 10 bases, labels every 50 (1-based positions, like Benchling).
    let ruler_y = y_bot + m.line_h + RULER_GAP;
    let width = (row_end - row_start) as f32 * m.char_w;
    window.paint_quad(fill(rect(ox, ruler_y, width, 1.0), theme::ruler()));
    let label_size = (m.font_size * 0.8).max(8.0);
    for i in row_start..row_end {
        let pos = i + 1;
        if pos % 10 != 0 {
            continue;
        }
        let x = col(i) + m.char_w / 2.0;
        window.paint_quad(fill(rect(x - 0.5, ruler_y, 1.0, 5.0), theme::ruler()));
        if pos % 50 == 0 {
            let label = thousands(pos);
            let est = label.len() as f32 * label_size * 0.6;
            let lx = (x - est / 2.0).min(ox + width - est);
            paint_text(label, lx, ruler_y + 6.0, label_size, label_size * 1.3, theme::muted_text(), window, cx);
        }
    }

    // Feature lanes.
    let bar_h = m.lane_h - 4.0;
    for p in &ctx.layout.pieces[row] {
        let f = &ctx.features[p.feature];
        let x0 = col(p.start);
        let x1 = col(p.end);
        let y = oy + m.lanes_top + p.lane as f32 * m.lane_h;
        let color = theme::feature(&f.kind);
        let arrow = if p.head { (bar_h * 0.6).min((x1 - x0) * 0.5) } else { 0.0 };
        let mut b = PathBuilder::fill();
        if p.head && f.strand > 0 {
            b.move_to(point(px(x0), px(y)));
            b.line_to(point(px(x1 - arrow), px(y)));
            b.line_to(point(px(x1), px(y + bar_h / 2.0)));
            b.line_to(point(px(x1 - arrow), px(y + bar_h)));
            b.line_to(point(px(x0), px(y + bar_h)));
        } else if p.head && f.strand < 0 {
            b.move_to(point(px(x0 + arrow), px(y)));
            b.line_to(point(px(x1), px(y)));
            b.line_to(point(px(x1), px(y + bar_h)));
            b.line_to(point(px(x0 + arrow), px(y + bar_h)));
            b.line_to(point(px(x0), px(y + bar_h / 2.0)));
        } else {
            b.move_to(point(px(x0), px(y)));
            b.line_to(point(px(x1), px(y)));
            b.line_to(point(px(x1), px(y + bar_h)));
            b.line_to(point(px(x0), px(y + bar_h)));
        }
        b.close();
        if let Ok(path) = b.build() {
            window.paint_path(path, color);
        }
        let aas = &ctx.translations[p.feature];
        if !aas.is_empty() {
            // A coding feature shows its translation instead of its label: each amino
            // acid over the middle base of its codon, in the sequence font so they line up.
            let from = aas.partition_point(|&(pos, _)| pos < p.start);
            let mut text = vec![b' '; p.end - p.start];
            for &(pos, aa) in aas[from..].iter().take_while(|&&(pos, _)| pos < p.end) {
                text[pos - p.start] = aa;
            }
            let text = String::from_utf8(text).unwrap_or_default();
            let ink = theme::feature_text(color);
            let mask = ContentMask { bounds: rect(x0, y, x1 - x0, bar_h) };
            window.with_content_mask(Some(mask), |window| {
                paint_text(text, x0, y, m.font_size, bar_h, ink, window, cx);
            });
            // A chevron between codons, pointing the way the feature reads, so its
            // direction shows on every row and not only at the arrowhead.
            let (cw, ch) = ((m.char_w * 0.3).min(4.0), (bar_h * 0.22).min(4.0));
            let mid = y + bar_h / 2.0;
            let to = aas.partition_point(|&(pos, _)| pos < p.end);
            for w in aas[from.saturating_sub(1)..(to + 1).min(aas.len())].windows(2) {
                let x = (col(w[0].0) + col(w[1].0) + m.char_w) / 2.0;
                if x - cw < x0 + 1.0 || x + cw > x1 - arrow - 1.0 {
                    continue;
                }
                let (tip, tail) = if f.strand > 0 { (x + cw / 2.0, x - cw / 2.0) } else { (x - cw / 2.0, x + cw / 2.0) };
                let mut b = PathBuilder::stroke(px(1.0));
                b.move_to(point(px(tail), px(mid - ch)));
                b.line_to(point(px(tip), px(mid)));
                b.line_to(point(px(tail), px(mid + ch)));
                if let Ok(path) = b.build() {
                    window.paint_path(path, ink.opacity(0.5));
                }
            }
        } else if x1 - x0 > 24.0 {
            let mask = ContentMask { bounds: rect(x0 + 3.0, y, x1 - x0 - 3.0 - arrow, bar_h) };
            let label = f.label.clone();
            let size = (bar_h * 0.78).max(8.0);
            window.with_content_mask(Some(mask), |window| {
                paint_text(label, x0 + 4.0, y, size, bar_h, theme::feature_text(color), window, cx);
            });
        }
    }

    register_mouse(ctx, row, bounds, hitbox, window);
}

fn register_mouse(ctx: &RowCtx, row: usize, bounds: Bounds<Pixels>, hitbox: Hitbox, window: &mut Window) {
    let m = ctx.m;
    let bpr = ctx.layout.bases_per_row;
    let len = ctx.seq.len();
    let row_start = row * bpr;
    let row_end = (row_start + bpr).min(len);
    let ox = f32::from(bounds.origin.x) + PAD_X;
    let oy = f32::from(bounds.origin.y);
    let hit = move |x: Pixels| -> usize {
        let c = ((f32::from(x) - ox) / m.char_w).round().clamp(0.0, (row_end - row_start) as f32);
        row_start + c as usize
    };

    let pieces = ctx.layout.pieces[row].clone();
    let features = ctx.features.clone();
    let editor = ctx.editor.clone();
    let focus = ctx.focus.clone();
    window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
        // The hitbox rather than the bounds: an open context menu covers the rows under it.
        let button = ev.button;
        if !phase.bubble() || !matches!(button, MouseButton::Left | MouseButton::Right) || !hitbox.is_hovered(window) {
            return;
        }
        window.focus(&focus);
        // Clicking a feature bar selects the whole feature.
        let ly = f32::from(ev.position.y) - oy - m.lanes_top;
        let lane = if ly >= 0.0 { Some((ly / m.lane_h) as usize) } else { None };
        let base = ((f32::from(ev.position.x) - ox) / m.char_w).floor();
        let picked = lane.and_then(|lane| {
            pieces.iter().find(|p| p.lane == lane && base >= 0.0 && (base as usize + row_start) >= p.start && (base as usize + row_start) < p.end)
        });
        let feature = picked.map(|p| (&features[p.feature], p.start, p.end));
        let feature_ix = picked.map(|p| p.feature);
        let pos = hit(ev.position.x);
        editor
            .update(cx, |ed, cx| {
                if button == MouseButton::Right {
                    // Right-clicking in the selection keeps it, so the menu acts on it.
                    let sel = ed.selection();
                    let inside = feature.is_none()
                        && !sel.is_empty()
                        && sel.parts(ed.doc.len()).iter().any(|r| !r.is_empty() && r.start <= pos && pos <= r.end);
                    if !inside {
                        ed.click(feature, pos, false);
                    }
                    ed.open_menu(ev.position, feature_ix);
                } else {
                    ed.click(feature, pos, ev.modifiers.shift);
                    ed.dragging = feature.is_none();
                }
                cx.notify();
            })
            .ok();
    });

    let editor = ctx.editor.clone();
    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
        if !phase.bubble() || !bounds.contains(&ev.position) {
            return;
        }
        let pos = hit(ev.position.x);
        editor
            .update(cx, |ed, cx| {
                if ed.dragging && ev.pressed_button == Some(MouseButton::Left) && ed.head != pos {
                    ed.head = pos;
                    cx.notify();
                }
            })
            .ok();
    });

    let editor = ctx.editor.clone();
    window.on_mouse_event(move |_: &MouseUpEvent, _phase, _window, cx| {
        editor.update(cx, |ed, _| ed.dragging = false).ok();
    });
}

pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A name typed without a recognised extension saves as `.pgx`.
fn with_default_extension(path: PathBuf) -> PathBuf {
    if Format::from_path(&path).is_some() {
        path
    } else {
        let mut s = path.into_os_string();
        s.push(format!(".{}", pgx::EXTENSION));
        PathBuf::from(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_selection_reads_through_the_origin() {
        let seq = b"AACCGGTT";
        let sel = Selection { start: 6, end: 2 };
        assert!(sel.wraps());
        assert_eq!(sel.bp(seq.len()), 4);
        assert_eq!(sel.bases(seq), b"TTAA");
        assert_eq!(model::reverse_complement(&sel.bases(seq)), b"TTAA");
        let plain = Selection { start: 2, end: 6 };
        assert_eq!(plain.bases(seq), b"CCGG");
        // A map item for a feature spanning 6..10 on an 8 bp circle.
        assert!(sel.matches_extent(6, 10, 8));
        assert!(!plain.matches_extent(6, 10, 8));
    }
}

#[cfg(test)]
mod translation_tests {
    use super::*;

    #[test]
    fn cds_features_carry_their_translation() {
        let doc = Document::open(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/pEGFP-N1.gb"))).unwrap();
        let egfp = doc.features.iter().find(|f| f.kind == "CDS" && f.segments == vec![(678, 1398)]).expect("EGFP CDS");
        let aas = translation(&doc.seq, egfp);
        // M of MVSKG over the T of the ATG at 678.
        assert_eq!(aas[0], (679, b'M'));
        assert_eq!(&aas[..5].iter().map(|a| a.1).collect::<Vec<_>>(), b"MVSKG");
        assert!(aas.windows(2).all(|w| w[0].0 < w[1].0));
        // Reverse-strand CDS: sorted by position, so the stop comes first.
        if let Some(rev) = doc.features.iter().find(|f| f.kind == "CDS" && f.strand < 0) {
            let aas = translation(&doc.seq, rev);
            assert_eq!(aas.first().map(|a| a.1), Some(b'*'));
            assert_eq!(aas.last().map(|a| a.1), Some(b'M'));
        }
        let other = Feature { kind: "promoter".into(), label: String::new(), segments: vec![(0, 30)], strand: 1, source: None };
        assert!(translation(&doc.seq, &other).is_empty());
    }
}

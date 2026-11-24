//! Per-tab split layout: a binary tree of panes separated by draggable dividers.
//!
//! Each open file owns one `PaneGroup`, so its splits and divider positions travel
//! with the tab. The sequence editor lives in exactly one pane (it can't be closed);
//! every other pane shows an SBOL map of the same document, or a 3D structure attached
//! to it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    actions, canvas, div, point, prelude::*, px, relative, AnyElement, Bounds, Context, CursorStyle, Entity,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, SharedString, Subscription, Window, anchored, deferred,
};

use crate::editor::{EditorEvent, SeqEditor};
use crate::map::{DnaMap, MapMode};
use crate::structure_view::StructureView;
use crate::theme;

actions!(plasmid, [SplitRight, SplitDown, ClosePane]);

const DIVIDER: f32 = 5.0;
const MIN_PANE: f32 = 120.0;
/// How far the mouse must travel with a header held before the pane starts moving.
const DRAG_THRESHOLD: f32 = 5.0;
/// Edge zones take this fraction of a pane from each side; the rest is the center.
const EDGE_ZONE: f32 = 0.3;

/// Where a dragged pane lands relative to the pane under the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    /// Swap the two panes.
    Center,
}

impl Zone {
    /// The zone for `pos` inside `b`: the nearest edge when close to one, else the center.
    fn at(b: Bounds<Pixels>, pos: Point<Pixels>) -> Zone {
        let (w, h) = (f32::from(b.size.width).max(1.0), f32::from(b.size.height).max(1.0));
        let x = f32::from(pos.x - b.origin.x) / w;
        let y = f32::from(pos.y - b.origin.y) / h;
        let edges = [(x, Zone::Left), (1.0 - x, Zone::Right), (y, Zone::Top), (1.0 - y, Zone::Bottom)];
        let (d, zone) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).expect("four edges");
        if d < EDGE_ZONE { zone } else { Zone::Center }
    }
}

/// A pane being dragged by its header.
struct PaneDrag {
    leaf: usize,
    start: Point<Pixels>,
    pos: Point<Pixels>,
    /// False until the mouse passes `DRAG_THRESHOLD`, so header clicks stay clicks.
    live: bool,
    target: Option<(usize, Zone)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side, divided by a vertical bar.
    Row,
    /// Children stacked, divided by a horizontal bar.
    Column,
}

#[derive(Clone)]
enum PaneView {
    Sequence,
    Map(Entity<DnaMap>),
    Structure(Entity<StructureView>),
}

enum Node {
    Leaf { id: usize, view: PaneView },
    Split { id: usize, axis: Axis, ratio: f32, bounds: Rc<Cell<Bounds<Pixels>>>, a: Box<Node>, b: Box<Node> },
}

impl Node {
    fn contains_leaf(&self, leaf: usize) -> bool {
        match self {
            Node::Leaf { id, .. } => *id == leaf,
            Node::Split { a, b, .. } => a.contains_leaf(leaf) || b.contains_leaf(leaf),
        }
    }

    fn is_leaf(&self, leaf: usize) -> bool {
        matches!(self, Node::Leaf { id, .. } if *id == leaf)
    }

    fn leaf_mut(&mut self, leaf: usize) -> Option<&mut Node> {
        if self.is_leaf(leaf) {
            return Some(self);
        }
        match self {
            Node::Split { a, b, .. } => {
                if a.contains_leaf(leaf) {
                    a.leaf_mut(leaf)
                } else {
                    b.leaf_mut(leaf)
                }
            }
            Node::Leaf { .. } => None,
        }
    }

    fn view(&self, leaf: usize) -> Option<&PaneView> {
        match self {
            Node::Leaf { id, view } if *id == leaf => Some(view),
            Node::Leaf { .. } => None,
            Node::Split { a, b, .. } => a.view(leaf).or_else(|| b.view(leaf)),
        }
    }

    fn split_mut(&mut self, split: usize) -> Option<(&mut f32, Axis, Bounds<Pixels>)> {
        match self {
            Node::Leaf { .. } => None,
            Node::Split { id, axis, ratio, bounds, a, b } => {
                if *id == split {
                    Some((ratio, *axis, bounds.get()))
                } else {
                    a.split_mut(split).or_else(|| b.split_mut(split))
                }
            }
        }
    }

    /// Replace the split holding `leaf` with the leaf's sibling.
    fn remove_leaf(&mut self, leaf: usize) -> bool {
        let Node::Split { a, b, .. } = self else { return false };
        let placeholder = || Node::Leaf { id: usize::MAX, view: PaneView::Sequence };
        let keep = if a.is_leaf(leaf) {
            Some(std::mem::replace(&mut **b, placeholder()))
        } else if b.is_leaf(leaf) {
            Some(std::mem::replace(&mut **a, placeholder()))
        } else {
            None
        };
        if let Some(k) = keep {
            *self = k;
            return true;
        }
        let Node::Split { a, b, .. } = self else { return false };
        a.remove_leaf(leaf) || b.remove_leaf(leaf)
    }

    fn first_leaf(&self) -> usize {
        match self {
            Node::Leaf { id, .. } => *id,
            Node::Split { a, .. } => a.first_leaf(),
        }
    }

    /// The first pane showing a structure.
    fn structure_leaf(&self) -> Option<(usize, Entity<StructureView>)> {
        match self {
            Node::Leaf { id, view: PaneView::Structure(v) } => Some((*id, v.clone())),
            Node::Leaf { .. } => None,
            Node::Split { a, b, .. } => a.structure_leaf().or_else(|| b.structure_leaf()),
        }
    }

    /// The pane holding the sequence editor.
    fn sequence_leaf(&self) -> usize {
        match self {
            Node::Leaf { id, .. } => *id,
            Node::Split { a, b, .. } => {
                if a.has_sequence() { a.sequence_leaf() } else { b.sequence_leaf() }
            }
        }
    }

    fn has_sequence(&self) -> bool {
        match self {
            Node::Leaf { view, .. } => matches!(view, PaneView::Sequence),
            Node::Split { a, b, .. } => a.has_sequence() || b.has_sequence(),
        }
    }
}

/// What a new pane shows.
#[derive(Clone, Debug, PartialEq)]
enum NewPane {
    Map,
    /// A 3D structure pane, showing this structure if given.
    Structure(Option<String>),
}

/// The dropdown of the split button that was clicked.
struct SplitMenu {
    leaf: usize,
    axis: Axis,
    position: Point<Pixels>,
}

pub struct PaneGroup {
    editor: Entity<SeqEditor>,
    root: Node,
    next_id: usize,
    /// The pane that split/close actions apply to: the last one clicked.
    active: usize,
    /// The split whose divider is being dragged.
    resizing: Option<usize>,
    /// The pane being moved by its header.
    moving: Option<PaneDrag>,
    /// Window bounds of each leaf from the last paint, for finding the drop target.
    leaf_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    /// Window bounds of the whole group, for placing the drag chip.
    root_bounds: Rc<Cell<Bounds<Pixels>>>,
    split_menu: Option<SplitMenu>,
    _subscription: Subscription,
}

impl PaneGroup {
    pub fn new(editor: Entity<SeqEditor>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&editor, |this, _, ev: &EditorEvent, cx| match ev {
            EditorEvent::OpenStructure(id) => this.open_structure(id, cx),
        });
        Self {
            editor,
            root: Node::Leaf { id: 0, view: PaneView::Sequence },
            next_id: 1,
            active: 0,
            resizing: None,
            moving: None,
            leaf_bounds: Rc::new(RefCell::new(HashMap::new())),
            root_bounds: Rc::new(Cell::new(Bounds::default())),
            split_menu: None,
            _subscription: subscription,
        }
    }

    fn split(&mut self, leaf: usize, axis: Axis, kind: NewPane, cx: &mut Context<Self>) {
        self.split_menu = None;
        let editor = self.editor.clone();
        let view = match kind {
            NewPane::Map => {
                // Splitting a map keeps its mode; otherwise follow the topology.
                let mode = match self.root.view(leaf) {
                    Some(PaneView::Map(m)) => m.read(cx).mode,
                    _ if self.editor.read(cx).doc.circular => MapMode::Circular,
                    _ => MapMode::Linear,
                };
                PaneView::Map(cx.new(|cx| DnaMap::new(editor, mode, cx)))
            }
            NewPane::Structure(id) => PaneView::Structure(cx.new(|cx| {
                let mut v = StructureView::new(editor, cx);
                if let Some(id) = id {
                    v.show(&id, cx);
                }
                v
            })),
        };
        let (leaf_id, split_id) = (self.next_id, self.next_id + 1);
        self.next_id += 2;
        let Some(slot) = self.root.leaf_mut(leaf) else { return };
        let old = std::mem::replace(slot, Node::Leaf { id: usize::MAX, view: PaneView::Sequence });
        *slot = Node::Split {
            id: split_id,
            axis,
            ratio: 0.5,
            bounds: Rc::new(Cell::new(Bounds::default())),
            a: Box::new(old),
            b: Box::new(Node::Leaf { id: leaf_id, view }),
        };
        self.active = leaf_id;
        cx.notify();
    }

    /// Show structure `id`: in a structure pane if there is one, else in a new pane
    /// to the right of the sequence.
    fn open_structure(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some((leaf, view)) = self.root.structure_leaf() {
            view.update(cx, |v, cx| v.show(id, cx));
            self.active = leaf;
            cx.notify();
            return;
        }
        let sequence = self.root.sequence_leaf();
        self.split(sequence, Axis::Row, NewPane::Structure(Some(id.to_string())), cx);
    }

    fn toggle_split_menu(&mut self, leaf: usize, axis: Axis, position: Point<Pixels>, cx: &mut Context<Self>) {
        let same = self.split_menu.as_ref().is_some_and(|m| m.leaf == leaf && m.axis == axis);
        self.split_menu = (!same).then_some(SplitMenu { leaf, axis, position });
        self.active = leaf;
        cx.notify();
    }

    /// The split button's dropdown: what the new pane should show.
    fn render_split_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let menu = self.split_menu.as_ref()?;
        let (leaf, axis) = (menu.leaf, menu.axis);
        let item = |key: (&'static str, usize), label: SharedString, kind: NewPane| {
            div()
                .id(key)
                .px_3()
                .py(px(3.0))
                .rounded_sm()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.split(leaf, axis, kind.clone(), cx)))
        };
        let structures: Vec<(String, String)> =
            self.editor.read(cx).doc.structures.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
        let mut list = div()
            .id("split-menu")
            .min_w(px(180.0))
            .p_1()
            .rounded_md()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .text_sm()
            .text_color(theme::text())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.split_menu = None;
                cx.notify();
            }))
            .child(item(("split-map", 0), "Map".into(), NewPane::Map))
            .child(div().h(px(1.0)).my_1().bg(theme::border()));
        if structures.is_empty() {
            list = list.child(item(("split-3d", 0), "3D Structure".into(), NewPane::Structure(None)));
        } else {
            for (i, (id, name)) in structures.into_iter().enumerate() {
                list = list.child(item(("split-3d", i), format!("3D · {name}").into(), NewPane::Structure(Some(id))));
            }
        }
        Some(deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.0)).child(list)).with_priority(1))
    }

    fn close(&mut self, leaf: usize, cx: &mut Context<Self>) {
        if matches!(self.root.view(leaf), Some(PaneView::Sequence) | None) {
            return;
        }
        if self.root.remove_leaf(leaf) {
            if !self.root.contains_leaf(self.active) {
                self.active = self.root.first_leaf();
            }
            cx.notify();
        }
    }

    /// Move `leaf` beside `target`, splitting it on the `zone` side, or swap the two
    /// for `Zone::Center`.
    fn move_pane(&mut self, leaf: usize, target: usize, zone: Zone, cx: &mut Context<Self>) {
        if leaf == target {
            return;
        }
        let (Some(view), Some(target_view)) = (self.root.view(leaf).cloned(), self.root.view(target).cloned()) else {
            return;
        };
        if zone == Zone::Center {
            // Swap whole leaves, so each view keeps its id (and the moved one stays active).
            if let Some(slot) = self.root.leaf_mut(leaf) {
                *slot = Node::Leaf { id: usize::MAX - 1, view: target_view.clone() };
            }
            if let Some(slot) = self.root.leaf_mut(target) {
                *slot = Node::Leaf { id: leaf, view };
            }
            if let Some(slot) = self.root.leaf_mut(usize::MAX - 1) {
                *slot = Node::Leaf { id: target, view: target_view };
            }
        } else {
            if !self.root.remove_leaf(leaf) {
                return;
            }
            let (axis, before) = match zone {
                Zone::Left => (Axis::Row, true),
                Zone::Top => (Axis::Column, true),
                Zone::Right => (Axis::Row, false),
                Zone::Bottom | Zone::Center => (Axis::Column, false),
            };
            let split_id = self.next_id;
            self.next_id += 1;
            let Some(slot) = self.root.leaf_mut(target) else { return };
            let old = std::mem::replace(slot, Node::Leaf { id: usize::MAX, view: PaneView::Sequence });
            let moved = Node::Leaf { id: leaf, view };
            let (a, b) = if before { (moved, old) } else { (old, moved) };
            *slot = Node::Split {
                id: split_id,
                axis,
                ratio: 0.5,
                bounds: Rc::new(Cell::new(Bounds::default())),
                a: Box::new(a),
                b: Box::new(b),
            };
        }
        self.active = leaf;
        cx.notify();
    }

    fn drag_pane(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.moving.as_mut() else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.moving = None;
            cx.notify();
            return;
        }
        drag.pos = ev.position;
        if !drag.live {
            let d = drag.pos - drag.start;
            if f32::from(d.x).hypot(f32::from(d.y)) < DRAG_THRESHOLD {
                return;
            }
            drag.live = true;
        }
        drag.target = self
            .leaf_bounds
            .borrow()
            .iter()
            .find(|(_, b)| b.contains(&ev.position))
            .map(|(&id, &b)| (id, Zone::at(b, ev.position)))
            .filter(|&(id, _)| id != drag.leaf);
        cx.notify();
    }

    fn end_pane_drag(&mut self, drop: bool, cx: &mut Context<Self>) {
        let Some(drag) = self.moving.take() else { return };
        if let (true, true, Some((target, zone))) = (drop, drag.live, drag.target) {
            self.move_pane(drag.leaf, target, zone, cx);
        }
        cx.notify();
    }

    fn split_right(&mut self, _: &SplitRight, _: &mut Window, cx: &mut Context<Self>) {
        self.split(self.active, Axis::Row, NewPane::Map, cx);
    }

    fn split_down(&mut self, _: &SplitDown, _: &mut Window, cx: &mut Context<Self>) {
        self.split(self.active, Axis::Column, NewPane::Map, cx);
    }

    fn close_pane(&mut self, _: &ClosePane, _: &mut Window, cx: &mut Context<Self>) {
        self.close(self.active, cx);
    }

    fn drag_divider(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.moving.is_some() {
            self.drag_pane(ev, cx);
            return;
        }
        let Some(split) = self.resizing else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.resizing = None;
            return;
        }
        let Some((ratio, axis, b)) = self.root.split_mut(split) else { return };
        let (pos, origin, extent) = match axis {
            Axis::Row => (ev.position.x, b.origin.x, b.size.width),
            Axis::Column => (ev.position.y, b.origin.y, b.size.height),
        };
        let extent = f32::from(extent);
        if extent <= 0.0 {
            return;
        }
        let min = (MIN_PANE / extent).min(0.45);
        *ratio = ((f32::from(pos - origin) - DIVIDER / 2.0) / extent).clamp(min, 1.0 - min);
        cx.notify();
    }

    // ----- rendering ---------------------------------------------------------------

    fn render_node(&self, node: &Node, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf { id, view } => self.render_leaf(*id, view, cx),
            Node::Split { id, axis, ratio, bounds, a, b } => {
                let id = *id;
                let cell = bounds.clone();
                let measure = canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full();
                let first = div().relative().flex().flex_shrink_0().min_w_0().min_h_0().overflow_hidden();
                let first = match axis {
                    Axis::Row => first.h_full().w(relative(*ratio)),
                    Axis::Column => first.w_full().h(relative(*ratio)),
                };
                let second = div().relative().flex().flex_1().min_w_0().min_h_0().overflow_hidden();
                let dragging = self.resizing == Some(id);
                let divider = div()
                    .id(("divider", id))
                    .flex_shrink_0()
                    .bg(if dragging { theme::accent() } else { theme::border() })
                    .hover(|s| s.bg(theme::accent()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.resizing = Some(id);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    );
                let divider = match axis {
                    Axis::Row => divider.w(px(DIVIDER)).h_full().cursor_col_resize(),
                    Axis::Column => divider.h(px(DIVIDER)).w_full().cursor_row_resize(),
                };
                let container = div().relative().flex().size_full().min_w_0().min_h_0();
                let container = match axis {
                    Axis::Row => container.flex_row(),
                    Axis::Column => container.flex_col(),
                };
                container
                    .child(measure)
                    .child(first.child(self.render_node(a, cx)))
                    .child(divider)
                    .child(second.child(self.render_node(b, cx)))
                    .into_any_element()
            }
        }
    }

    fn render_leaf(&self, id: usize, view: &PaneView, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active == id && matches!(self.root, Node::Split { .. });
        let icon_button = |key: &'static str, tip: &'static str, icon: AnyElement| {
            div()
                .id((key, id))
                .flex()
                .items_center()
                .justify_center()
                .size(px(20.0))
                .rounded_sm()
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(icon)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.into())).into())
        };
        let icon_color = theme::muted_text();
        let split_icon = |axis: Axis| {
            let line = match axis {
                Axis::Row => div().absolute().top_0().bottom_0().left(px(5.0)).w(px(1.0)),
                Axis::Column => div().absolute().left_0().right_0().top(px(4.0)).h(px(1.0)),
            };
            div()
                .relative()
                .w(px(13.0))
                .h(px(11.0))
                .border_1()
                .border_color(icon_color)
                .rounded(px(1.5))
                .child(line.bg(icon_color))
                .into_any_element()
        };

        let title: SharedString = match view {
            PaneView::Sequence => "SEQUENCE".into(),
            PaneView::Map(_) => "MAP".into(),
            PaneView::Structure(_) => "STRUCTURE".into(),
        };
        let mut header = div()
            .id(("pane-header", id))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                    this.moving = Some(PaneDrag { leaf: id, start: ev.position, pos: ev.position, live: false, target: None });
                }),
            )
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .h(px(26.0))
            .pl_3()
            .pr_1()
            .bg(theme::panel())
            .border_b_1()
            .border_color(if active { theme::accent() } else { theme::border() })
            .text_xs()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(if active { theme::accent() } else { theme::muted_text() })
                    .child(title),
            );

        if let PaneView::Map(map) = view {
            let mode = map.read(cx).mode;
            let segment = |key: &'static str, label: &'static str, m: MapMode| {
                let selected = mode == m;
                let map = map.clone();
                div()
                    .id((key, id))
                    .px_2()
                    .py(px(1.0))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(theme::active()).text_color(theme::accent()))
                    .when(!selected, |d| d.text_color(theme::muted_text()).hover(|s| s.bg(theme::hover())))
                    .child(label)
                    .on_click(move |_, _, cx| map.update(cx, |m2, cx| m2.set_mode(m, cx)))
            };
            header = header.child(
                div()
                    .ml_2()
                    .flex()
                    .flex_row()
                    .gap(px(2.0))
                    .p(px(1.0))
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .child(segment("mode-circular", "Circular", MapMode::Circular))
                    .child(segment("mode-linear", "Linear", MapMode::Linear)),
            );
        }

        header = header
            .child(div().flex_1())
            .child(icon_button("split-right", "Split right… (Ctrl+\\ for a map)", split_icon(Axis::Row)).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_split_menu(id, Axis::Row, ev.position, cx);
                }),
            ))
            .child(icon_button("split-down", "Split down… (Ctrl+Shift+\\ for a map)", split_icon(Axis::Column)).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_split_menu(id, Axis::Column, ev.position, cx);
                }),
            ));
        if !matches!(view, PaneView::Sequence) {
            header = header.child(
                icon_button("close-pane", "Close pane (Ctrl+Shift+W)", div().text_sm().text_color(icon_color).child("×").into_any_element())
                    .on_click(cx.listener(move |this, _, _, cx| this.close(id, cx))),
            );
        }

        let body: AnyElement = match view {
            PaneView::Sequence => self.editor.clone().into_any_element(),
            PaneView::Map(map) => map.clone().into_any_element(),
            PaneView::Structure(s) => s.clone().into_any_element(),
        };

        let cell = self.leaf_bounds.clone();
        let measure = canvas(
            move |b, _, _| {
                cell.borrow_mut().insert(id, b);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        // While a pane is dragged: dim it, and outline where it would land.
        let live = self.moving.as_ref().filter(|d| d.live);
        let source = live.is_some_and(|d| d.leaf == id);
        let zone = live.and_then(|d| d.target).filter(|t| t.0 == id).map(|t| t.1);
        let overlay = zone.map(|zone| {
            let accent = theme::accent();
            let o = div().absolute().bg(accent.opacity(0.18)).border_2().border_color(accent.opacity(0.8)).rounded_sm();
            match zone {
                Zone::Left => o.left_0().top_0().bottom_0().w(relative(0.5)),
                Zone::Right => o.right_0().top_0().bottom_0().w(relative(0.5)),
                Zone::Top => o.left_0().right_0().top_0().h(relative(0.5)),
                Zone::Bottom => o.left_0().right_0().bottom_0().h(relative(0.5)),
                Zone::Center => o.inset_0(),
            }
        });

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(measure)
            .on_any_mouse_down(cx.listener(move |this, _, _, cx| {
                if this.active != id {
                    this.active = id;
                    cx.notify();
                }
            }))
            .child(header)
            .child(div().flex_1().min_h_0().min_w_0().child(body))
            .when(source, |d| d.child(div().absolute().inset_0().bg(theme::editor_bg().opacity(0.6))))
            .children(overlay)
            .into_any_element()
    }
}

impl Render for PaneGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Leaves re-register their bounds as they paint.
        self.leaf_bounds.borrow_mut().clear();
        let content = self.render_node(&self.root, cx);
        let cell = self.root_bounds.clone();
        let measure = canvas(move |b, _, _| cell.set(b), |_, _, _, _| {}).absolute().size_full();
        // While a pane moves, a transparent cover keeps the panes underneath from
        // reacting and shows the grab cursor, and a chip follows the mouse.
        let drag_layer = self.moving.as_ref().filter(|d| d.live).map(|d| {
            let origin = self.root_bounds.get().origin;
            let title = match self.root.view(d.leaf) {
                Some(PaneView::Sequence) => "SEQUENCE",
                Some(PaneView::Structure(_)) => "STRUCTURE",
                _ => "MAP",
            };
            let hint = match d.target.map(|t| t.1) {
                Some(Zone::Center) => " · swap",
                Some(Zone::Left) => " · left",
                Some(Zone::Right) => " · right",
                Some(Zone::Top) => " · top",
                Some(Zone::Bottom) => " · bottom",
                None => "",
            };
            let at = point(d.pos.x - origin.x + px(14.0), d.pos.y - origin.y + px(10.0));
            div().id("pane-drag-cover").absolute().inset_0().cursor(CursorStyle::ClosedHand).child(
                div()
                    .absolute()
                    .left(at.x)
                    .top(at.y)
                    .px_2()
                    .py(px(2.0))
                    .rounded_sm()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::accent())
                    .text_xs()
                    .text_color(theme::accent())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("{title}{hint}")),
            )
        });
        div()
            .relative()
            .size_full()
            .child(measure)
            .on_action(cx.listener(Self::split_right))
            .on_action(cx.listener(Self::split_down))
            .on_action(cx.listener(Self::close_pane))
            .on_mouse_move(cx.listener(Self::drag_divider))
            // Capture phase, so a pane can't swallow the release that drops a dragged pane.
            .capture_any_mouse_up(cx.listener(|this, _, _, cx| this.end_pane_drag(true, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.resizing.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_pane_drag(false, cx);
                    if this.resizing.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(content)
            .children(drag_layer)
            .children(self.render_split_menu(cx))
    }
}

struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme::panel())
            .border_1()
            .border_color(theme::border())
            .text_xs()
            .text_color(theme::text())
            .child(self.0.clone())
    }
}

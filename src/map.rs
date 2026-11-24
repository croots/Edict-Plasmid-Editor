//! An SBOL Visual map of the open document, drawn linear or circular.
//!
//! The map reads the document and selection straight from the tab's `SeqEditor`
//! and re-renders whenever it changes. Clicking a glyph selects that feature in the
//! editor; clicking or dragging along the backbone sets the cursor or a range.

use std::cell::Cell;
use std::f32::consts::TAU;
use std::rc::Rc;

use gpui::{
    canvas, div, point, prelude::*, px, App, Bounds, Context, Entity, FocusHandle, HitboxBehavior, Hsla, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, ScrollHandle, SharedString, Subscription, Window,
};

use crate::editor::{self, thousands, SeqEditor, Selection};
use crate::map_layout::{self, Placed, Placement};
use crate::locks::Lock;
use crate::model::Feature;
use crate::sbol::{self, Align, Glyph, Paint, Sizing};
use crate::theme;

const GLYPH_H: f32 = 16.0;
const LANE_GAP: f32 = 6.0;
const LABEL_SIZE: f32 = 11.0;
const LABEL_LINE: f32 = 14.0;
const PAD: f32 = 28.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMode {
    Circular,
    Linear,
}

pub struct DnaMap {
    editor: Entity<SeqEditor>,
    pub mode: MapMode,
    viewport: Rc<Cell<(f32, f32)>>,
    drag: Rc<Cell<Option<Drag>>>,
    /// Linear maps can be taller than the pane; they open centred on the backbone.
    scroll: ScrollHandle,
    /// The layout the scroll position was last centred for: (mode, content height).
    centred_for: Option<(MapMode, i32)>,
    _observe: Subscription,
}

impl DnaMap {
    pub fn new(editor: Entity<SeqEditor>, mode: MapMode, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            editor,
            mode,
            viewport: Rc::new(Cell::new((0.0, 0.0))),
            drag: Rc::new(Cell::new(None)),
            scroll: ScrollHandle::new(),
            centred_for: None,
            _observe: observe,
        }
    }

    pub fn set_mode(&mut self, mode: MapMode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
    }
}

// ----- geometry --------------------------------------------------------------------

/// Maps track coordinates (`u` along the backbone, `v` away from it) to the screen.
#[derive(Clone, Copy)]
enum Proj {
    Linear { x0: f32, y: f32 },
    Circular { cx: f32, cy: f32, r: f32 },
}

impl Proj {
    fn at(&self, u: f32, v: f32) -> Point<Pixels> {
        match *self {
            Proj::Linear { x0, y } => point(px(x0 + u), px(y - v)),
            Proj::Circular { cx, cy, r } => {
                // Angle 0 at twelve o'clock, increasing clockwise.
                let a = u / r;
                point(px(cx + (r + v) * a.sin()), px(cy - (r + v) * a.cos()))
            }
        }
    }

    fn inverse(&self, p: Point<Pixels>) -> (f32, f32) {
        let (x, y) = (f32::from(p.x), f32::from(p.y));
        match *self {
            Proj::Linear { x0, y: by } => (x - x0, by - y),
            Proj::Circular { cx, cy, r } => {
                let (dx, dy) = (x - cx, y - cy);
                let a = dx.atan2(-dy).rem_euclid(TAU);
                (a * r, (dx * dx + dy * dy).sqrt() - r)
            }
        }
    }

    fn curved(&self) -> bool {
        matches!(self, Proj::Circular { .. })
    }
}

struct CircLabel {
    item: usize,
    /// Label baseline center, relative to the circle's center.
    y: f32,
    right: bool,
}

struct Geo {
    mode: MapMode,
    len: usize,
    /// Pixels per base along the backbone.
    k: f32,
    lane_h: f32,
    /// Glyph height; circular maps shrink it when lanes run out of room.
    glyph_h: f32,
    /// Pane width, for keeping labels inside it.
    width: f32,
    placement: Placement,
    labels: Vec<String>,
    label_char_w: f32,
    /// Linear: total content height and the backbone's y within it.
    height: f32,
    y_base: f32,
    /// Circular: backbone radius, inner (ruler) and outer (glyph) radii, label ring.
    r: f32,
    r_in: f32,
    r_out: f32,
    r_label: f32,
    circ_labels: Vec<CircLabel>,
}

fn glyph_for(f: &Feature) -> Option<Glyph> {
    if Glyph::is_annotation(&f.kind) {
        return None;
    }
    let class = f.source.as_ref().and_then(|s| s.qualifier_values("regulatory_class").next());
    Some(Glyph::classify(&f.kind, class))
}

/// A one-line label for the map. Labels can come from free-text qualifiers such as
/// `/note`, which may wrap across lines and run long.
fn map_label(label: &str) -> String {
    const MAX: usize = 28;
    let flat = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    let mut short: String = flat.chars().take(MAX - 1).collect();
    short.push('…');
    short
}

impl Geo {
    fn compute(mode: MapMode, features: &[Feature], len: usize, w: f32, h: f32, label_char_w: f32) -> Geo {
        let glyphs: Vec<Option<Glyph>> = features.iter().map(glyph_for).collect();
        let labels: Vec<String> = features.iter().map(|f| map_label(&f.label)).collect();
        let lw = |i: usize| labels[i].chars().count() as f32 * label_char_w;
        match mode {
            MapMode::Linear => {
                let inner = (w - 2.0 * PAD).max(10.0);
                let k = inner / len as f32;
                let lane_h = GLYPH_H + LABEL_LINE + LANE_GAP;
                let mut shown = labels.clone();
                let mut placement = map_layout::place(features, &glyphs, len, k, None, &lw, 6.0);
                let tall = |p: &Placement| PAD + (p.fwd_lanes + p.rev_lanes) as f32 * lane_h + 40.0 > h;
                if tall(&placement) {
                    // Dense maps: keep only the labels that fit over their own feature
                    // (the rest show in the status bar on click) and pack again.
                    for it in &placement.items {
                        if lw(it.feature) > (it.span.1 - it.span.0).max(it.w) + 12.0 {
                            shown[it.feature].clear();
                        }
                    }
                    let lw = |i: usize| shown[i].chars().count() as f32 * label_char_w;
                    placement = map_layout::place(features, &glyphs, len, k, None, lw, 3.0);
                }
                let y_base = PAD * 0.5 + placement.fwd_lanes as f32 * lane_h;
                let height = y_base + placement.rev_lanes as f32 * lane_h + 40.0;
                Geo {
                    mode,
                    len,
                    k,
                    lane_h,
                    glyph_h: GLYPH_H,
                    width: w,
                    placement,
                    labels: shown,
                    label_char_w,
                    height,
                    y_base,
                    r: 0.0,
                    r_in: 0.0,
                    r_out: 0.0,
                    r_label: 0.0,
                    circ_labels: Vec::new(),
                }
            }
            MapMode::Circular => {
                // Room for a typical label, not the longest: long ones get shortened at
                // the pane edge.
                let mut widths: Vec<f32> = (0..labels.len()).filter(|&i| glyphs[i].is_some()).map(&lw).collect();
                widths.sort_by(f32::total_cmp);
                let typical = widths.get(widths.len() * 3 / 4).copied().unwrap_or(0.0);
                let margin_x = (typical.min(16.0 * label_char_w) + 30.0).clamp(50.0, w * 0.3);
                let outer_max = (w / 2.0 - margin_x).min(h / 2.0 - 24.0).max(40.0);
                // The center keeps a hole for the name and ruler.
                let hole = outer_max * 0.3;
                let full_lane = GLYPH_H + LANE_GAP;
                // Place once at a default radius, then move the backbone so forward
                // lanes (outside) and reverse lanes (inside) get equal room per lane.
                let place_at = |r: f32| {
                    let c = TAU * r;
                    map_layout::place(features, &glyphs, len, c / len as f32, Some(c), |_| 0.0, 4.0)
                };
                let r0 = outer_max * 0.66;
                let p0 = place_at(r0);
                let (f, b) = (p0.fwd_lanes as f32, p0.rev_lanes as f32);
                let balanced = (outer_max * b + hole * f) / (f + b);
                let r = balanced.clamp(outer_max * 0.5, outer_max * 0.8);
                let placement = if (r - r0).abs() > 1.0 { place_at(r) } else { p0 };
                // Too many lanes for the room: shrink the glyphs rather than the circle.
                let out_room = outer_max - r;
                let in_room = r - hole;
                let need_out = placement.fwd_lanes as f32 * full_lane;
                let need_in = placement.rev_lanes as f32 * full_lane;
                let scale = (out_room / need_out).min(in_room / need_in).clamp(0.4, 1.0);
                let lane_h = full_lane * scale;
                let k = TAU * r / len as f32;
                let r_out = r + placement.fwd_lanes as f32 * lane_h;
                let r_in = r - placement.rev_lanes as f32 * lane_h;
                let r_label = r_out + 10.0;
                let mut geo = Geo {
                    mode,
                    len,
                    k,
                    lane_h,
                    glyph_h: GLYPH_H * scale,
                    width: w,
                    placement,
                    labels,
                    label_char_w,
                    height: h,
                    y_base: 0.0,
                    r,
                    r_in,
                    r_out,
                    r_label,
                    circ_labels: Vec::new(),
                };
                geo.circ_labels = geo.layout_circular_labels(h);
                geo
            }
        }
    }

    /// Track `v` of a placed glyph's local `y = 0`.
    fn base_v(&self, it: &Placed) -> f32 {
        if it.lane == 0 {
            0.0
        } else {
            let lift = if it.glyph.align() == Align::Centered { self.glyph_h / 2.0 } else { 0.0 };
            it.lane as f32 * self.lane_h + lift
        }
    }

    /// The glyph's local point in track coordinates, rotated 180° on the reverse strand.
    fn to_track(&self, it: &Placed, x: f32, y: f32) -> (f32, f32) {
        let v = self.base_v(it) + y;
        if it.reverse { (it.u0 + it.w - x, -v) } else { (it.u0 + x, v) }
    }

    /// Lowest and highest track `v` the glyph covers.
    fn v_range(&self, it: &Placed) -> (f32, f32) {
        let (lo, hi) = it.glyph.y_range(self.glyph_h);
        let b = self.base_v(it);
        if it.reverse { (-(b + hi), -(b + lo)) } else { (b + lo, b + hi) }
    }

    fn circumference(&self) -> Option<f32> {
        (self.mode == MapMode::Circular).then(|| TAU * self.r)
    }

    fn base_at(&self, u: f32) -> usize {
        let u = match self.circumference() {
            Some(c) => u.rem_euclid(c),
            None => u,
        };
        ((u / self.k).round().max(0.0) as usize).min(self.len)
    }

    fn hit(&self, u: f32, v: f32) -> Option<&Placed> {
        let c = self.circumference();
        let along = |it: &Placed| {
            let inside = |u: f32| u >= it.u0 - 2.0 && u <= it.u0 + it.w + 2.0;
            match c {
                Some(c) => inside(u) || inside(u + c) || inside(u - c),
                None => inside(u),
            }
        };
        // Smallest first so a site on top of a gene wins.
        let mut hits: Vec<&Placed> = self
            .placement
            .items
            .iter()
            .filter(|it| along(it))
            .filter(|it| {
                let (lo, hi) = self.v_range(it);
                v >= lo - 2.0 && v <= hi + 2.0
            })
            .collect();
        hits.sort_by(|a, b| a.w.total_cmp(&b.w));
        hits.first().copied()
    }

    /// Labels sit outside the circle in two columns, nudged apart vertically and
    /// joined to their glyphs by leader lines. When a side runs out of room the
    /// smallest features lose their labels first.
    fn layout_circular_labels(&self, h: f32) -> Vec<CircLabel> {
        let half = h / 2.0 - 12.0;
        let max_per_side = ((2.0 * half) / LABEL_LINE).floor().max(0.0) as usize;
        let mut sides: [Vec<(usize, f32)>; 2] = [Vec::new(), Vec::new()];
        for (i, it) in self.placement.items.iter().enumerate() {
            if self.labels[it.feature].is_empty() {
                continue;
            }
            let a = it.center() / self.r;
            let right = a.rem_euclid(TAU) < std::f32::consts::PI;
            sides[if right { 0 } else { 1 }].push((i, -self.r_label * a.cos()));
        }
        let mut out = Vec::new();
        for (s, side) in sides.iter_mut().enumerate() {
            if side.len() > max_per_side {
                side.sort_by(|a, b| self.placement.items[b.0].w.total_cmp(&self.placement.items[a.0].w));
                side.truncate(max_per_side);
            }
            side.sort_by(|a, b| a.1.total_cmp(&b.1));
            let n = side.len();
            for i in 0..n {
                let min = if i == 0 { -half } else { side[i - 1].1 + LABEL_LINE };
                side[i].1 = side[i].1.max(min);
            }
            for i in (0..n).rev() {
                let max = if i == n - 1 { half } else { side[i + 1].1 - LABEL_LINE };
                side[i].1 = side[i].1.min(max);
            }
            out.extend(side.iter().map(|&(item, y)| CircLabel { item, y, right: s == 0 }));
        }
        out
    }
}

/// Half the width of the locked band, in track units (pixels).
const LOCK_BAND: f32 = 5.0;

// ----- painting --------------------------------------------------------------------

/// Points along a polyline in track space, subdivided so straight segments bend
/// with the circle.
fn project(proj: &Proj, pts: &[(f32, f32)], closed: bool) -> Vec<Point<Pixels>> {
    let mut out = Vec::with_capacity(pts.len() * 2);
    let n = pts.len();
    let segs = if closed { n } else { n.saturating_sub(1) };
    if n == 0 {
        return out;
    }
    out.push(proj.at(pts[0].0, pts[0].1));
    for i in 0..segs {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let steps = if proj.curved() { ((b.0 - a.0).abs() / 3.0).ceil().max(1.0) as usize } else { 1 };
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            out.push(proj.at(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    if closed {
        out.pop(); // the last point repeats the first
    }
    out
}

fn paint_poly(window: &mut Window, pts: &[Point<Pixels>], closed: bool, style: Option<f32>, dashed: bool, color: Hsla) {
    if pts.len() < 2 {
        return;
    }
    let mut b = match style {
        None => PathBuilder::fill(),
        Some(w) if dashed => PathBuilder::stroke(px(w)).dash_array(&[px(3.0), px(2.0)]),
        Some(w) => PathBuilder::stroke(px(w)),
    };
    b.move_to(pts[0]);
    for p in &pts[1..] {
        b.line_to(*p);
    }
    if closed {
        b.close();
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, color);
    }
}

/// Track-space polygon of an annular band `[u0, u1] × [v0, v1]`.
fn band(u0: f32, u1: f32, v0: f32, v1: f32) -> Vec<(f32, f32)> {
    vec![(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
}

struct PaintCtx {
    geo: Rc<Geo>,
    name: String,
    selection: Selection,
    cursor: usize,
    focused: bool,
    locks: Rc<Vec<Lock>>,
}

fn paint_map(ctx: &PaintCtx, proj: &Proj, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let geo = &ctx.geo;
    let k = geo.k;
    let total_u = geo.len as f32 * k;
    let (v_top, v_bot) = match geo.mode {
        MapMode::Linear => (geo.placement.fwd_lanes as f32 * geo.lane_h, -(geo.placement.rev_lanes as f32 * geo.lane_h)),
        MapMode::Circular => (geo.r_out - geo.r, geo.r_in - geo.r),
    };

    // Selection band and cursor.
    let sel = &ctx.selection;
    let spans: Vec<(usize, usize)> = if sel.wraps() && proj.curved() {
        // One unbroken band through the origin.
        vec![(sel.start, sel.end + geo.len)]
    } else {
        sel.parts(geo.len).iter().filter(|r| !r.is_empty()).map(|r| (r.start, r.end)).collect()
    };
    for (s, e) in spans {
        let pts = project(proj, &band(s as f32 * k, e as f32 * k, v_bot, v_top), true);
        paint_poly(window, &pts, true, None, false, theme::selection());
    }

    // Locked bases: a band along the backbone.
    for l in ctx.locks.iter() {
        let spans: Vec<(usize, usize)> =
            if l.wraps() && proj.curved() { vec![(l.start, l.end + geo.len)] } else { l.parts(geo.len) };
        for (s, e) in spans {
            let pts = project(proj, &band(s as f32 * k, e as f32 * k, -LOCK_BAND, LOCK_BAND), true);
            paint_poly(window, &pts, true, None, false, theme::accent().opacity(0.45));
        }
    }

    // Backbone.
    let backbone: Vec<(f32, f32)> = vec![(0.0, 0.0), (total_u, 0.0)];
    let closed = geo.mode == MapMode::Circular;
    let bb = project(proj, &backbone, false);
    paint_poly(window, &bb, closed, Some(2.0), false, theme::text());

    // Ruler.
    paint_ruler(geo, proj, window, cx);

    // Glyphs.
    let outline = theme::text();
    let highlight = theme::cursor();
    for it in &geo.placement.items {
        let color = theme::feature(it.glyph.color_kind());
        let selected = sel.matches_extent(it.start, it.end, geo.len);
        // Point glyphs are narrower than long features; show the true extent on the backbone.
        if matches!(it.glyph.sizing(), Sizing::Point { .. }) && it.span.1 - it.span.0 > it.w + 2.0 {
            let b = geo.base_v(it);
            let (v0, v1) = if it.reverse { (-b - 3.0, -b) } else { (b, b + 3.0) };
            let pts = project(proj, &band(it.span.0, it.span.1, v0, v1), true);
            paint_poly(window, &pts, true, None, false, color.opacity(0.7));
        }
        for s in sbol::shapes(it.glyph, it.w, geo.glyph_h) {
            let track: Vec<(f32, f32)> = s.points.iter().map(|&(x, y)| geo.to_track(it, x, y)).collect();
            let pts = project(proj, &track, s.closed);
            let edge = if selected { highlight } else { outline };
            match s.paint {
                Paint::Outline => {
                    paint_poly(window, &pts, true, None, false, color);
                    paint_poly(window, &pts, true, Some(if selected { 2.0 } else { 1.0 }), false, edge);
                }
                Paint::Fill => paint_poly(window, &pts, true, None, false, if selected { highlight } else { color }),
                Paint::Stroke => paint_poly(window, &pts, s.closed, Some(2.0), false, if selected { highlight } else { color }),
                Paint::Dashed => paint_poly(window, &pts, true, Some(1.0), true, edge),
            }
        }
        if geo.mode == MapMode::Linear {
            paint_linear_label(geo, it, &geo.labels[it.feature], proj, selected, window, cx);
        }
    }

    if geo.mode == MapMode::Circular {
        paint_circular_labels(ctx, proj, window, cx);
        if let Proj::Circular { cx: ccx, cy, .. } = *proj {
            if geo.r_in > 60.0 {
                let name_size = 14.0;
                let name_w = ctx.name.chars().count() as f32 * geo.label_char_w * name_size / LABEL_SIZE;
                editor::paint_text(ctx.name.clone(), ccx - name_w / 2.0, cy - 18.0, name_size, 18.0, theme::text(), window, cx);
                let bp = format!("{} bp", thousands(geo.len));
                let bp_w = bp.chars().count() as f32 * geo.label_char_w;
                editor::paint_text(bp, ccx - bp_w / 2.0, cy + 2.0, LABEL_SIZE, LABEL_LINE, theme::muted_text(), window, cx);
            }
        }
    }

    // Cursor.
    if sel.is_empty() {
        let u = ctx.cursor as f32 * k;
        let pts = project(proj, &[(u, v_bot), (u, v_top)], false);
        paint_poly(window, &pts, false, Some(2.0), false, if ctx.focused { theme::cursor() } else { theme::muted_text() });
    }
    let _ = bounds;
}

fn paint_linear_label(geo: &Geo, it: &Placed, label: &str, proj: &Proj, selected: bool, window: &mut Window, cx: &mut App) {
    if label.is_empty() {
        return;
    }
    let (lo, hi) = geo.v_range(it);
    let w = label.chars().count() as f32 * geo.label_char_w;
    let (v_text_top, _) = if it.reverse { (lo - 1.0, ()) } else { (hi + 1.0 + LABEL_LINE, ()) };
    let p = proj.at(it.center(), v_text_top);
    let color = if selected { theme::cursor() } else { theme::text() };
    // Keep labels near the ends inside the pane.
    let Proj::Linear { x0, .. } = *proj else { return };
    let (left, right) = (x0 - PAD + 2.0, x0 - PAD + geo.width - 2.0);
    let x = (f32::from(p.x) - w / 2.0).min(right - w).max(left);
    editor::paint_text(label.to_string(), x, f32::from(p.y), LABEL_SIZE, LABEL_LINE, color, window, cx);
}

fn paint_circular_labels(ctx: &PaintCtx, proj: &Proj, window: &mut Window, cx: &mut App) {
    let geo = &ctx.geo;
    let Proj::Circular { cx: ccx, cy, r } = *proj else { return };
    let sel = &ctx.selection;
    for l in &geo.circ_labels {
        let it = &geo.placement.items[l.item];
        let label = &geo.labels[it.feature];
        let a = it.center() / r;
        let (_, hi) = geo.v_range(it);
        let anchor_v = if it.reverse { hi.max(0.0) } else { hi };
        let anchor = proj.at(it.center(), anchor_v + 1.0);
        let elbow = proj.at(it.center(), geo.r_label - r - 2.0);
        let y = cy + l.y;
        let dy = l.y;
        let ring_x = if dy.abs() < geo.r_label { (geo.r_label * geo.r_label - dy * dy).sqrt() } else { 0.0 };
        let reach = ring_x.max((geo.r_label) * a.sin().abs()) + 8.0;
        // Shorten labels that would run past the pane edge.
        let room = geo.width / 2.0 - reach - 3.0 - 6.0;
        let fit = (room / geo.label_char_w).floor().max(0.0) as usize;
        let label = &if label.chars().count() > fit {
            let mut s: String = label.chars().take(fit.saturating_sub(1)).collect();
            if !s.is_empty() {
                s.push('…');
            }
            s
        } else {
            label.clone()
        };
        let text_w = label.chars().count() as f32 * geo.label_char_w;
        let (end_x, text_x) = if l.right { (ccx + reach, ccx + reach + 3.0) } else { (ccx - reach, ccx - reach - 3.0 - text_w) };
        let leader = [anchor, elbow, point(px(end_x), px(y))];
        paint_poly(window, &leader, false, Some(1.0), false, theme::ruler());
        let selected = sel.matches_extent(it.start, it.end, geo.len);
        let color = if selected { theme::cursor() } else { theme::text() };
        editor::paint_text(label.clone(), text_x, y - LABEL_LINE / 2.0, LABEL_SIZE, LABEL_LINE, color, window, cx);
    }
}

fn paint_ruler(geo: &Geo, proj: &Proj, window: &mut Window, cx: &mut App) {
    let color = theme::ruler();
    let text = theme::muted_text();
    match *proj {
        Proj::Linear { x0, y } => {
            let total = geo.len as f32 * geo.k;
            let ry = y + geo.placement.rev_lanes as f32 * geo.lane_h + 6.0;
            paint_poly(window, &[point(px(x0), px(ry)), point(px(x0 + total), px(ry))], false, Some(1.0), false, color);
            let step = map_layout::tick_step(geo.len, ((total / 90.0) as usize).max(2));
            let mut pos = 0;
            while pos <= geo.len {
                let x = x0 + pos as f32 * geo.k;
                paint_poly(window, &[point(px(x), px(ry)), point(px(x), px(ry + 4.0))], false, Some(1.0), false, color);
                let label = thousands(pos.max(1));
                let w = label.chars().count() as f32 * geo.label_char_w;
                let lx = (x - w / 2.0).clamp(x0 - w / 2.0, x0 + total - w / 2.0);
                editor::paint_text(label, lx, ry + 5.0, LABEL_SIZE, LABEL_LINE, text, window, cx);
                pos += step;
            }
        }
        Proj::Circular { cx: ccx, cy, r } => {
            let ring = geo.r_in - 3.0;
            if ring < 24.0 {
                return;
            }
            let step = map_layout::tick_step(geo.len, ((TAU * ring / 70.0) as usize).clamp(4, 16));
            let mut pos = 0;
            while pos < geo.len {
                let u = pos as f32 * geo.k;
                let a = u / r;
                let tick = [proj.at(u, ring - r), proj.at(u, ring - r - 4.0)];
                paint_poly(window, &tick, false, Some(1.0), false, color);
                if ring > 50.0 {
                    let label = thousands(pos.max(1));
                    let w = label.chars().count() as f32 * geo.label_char_w;
                    let lr = ring - 4.0 - 10.0 - w * 0.35 * a.sin().abs();
                    let (lx, ly) = (ccx + lr * a.sin(), cy - lr * a.cos());
                    editor::paint_text(label, lx - w / 2.0, ly - LABEL_LINE / 2.0, LABEL_SIZE, LABEL_LINE, text, window, cx);
                }
                pos += step;
            }
        }
    }
}

// ----- mouse -----------------------------------------------------------------------

#[derive(Clone)]
struct MouseCtx {
    geo: Rc<Geo>,
    features: Rc<Vec<Feature>>,
    editor: Entity<SeqEditor>,
    editor_focus: FocusHandle,
    /// The document is circular, so drags on the circular map may cross the origin.
    circular: bool,
    drag: Rc<Cell<Option<Drag>>>,
    locks: Rc<Vec<Lock>>,
}

/// A drag along the backbone. Around a circle it follows the pointer's path, so the
/// selection goes whichever way the pointer went, through the origin if need be.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    anchor: usize,
    /// Where the pointer was last, and how far it has travelled from the anchor
    /// (signed: positive is clockwise), at most one turn less a base either way.
    last: usize,
    travel: i64,
    /// The last selection sent to the editor: (head, wraps).
    sent: (usize, bool),
}

impl Drag {
    fn new(anchor: usize, len: usize, circular: bool) -> Self {
        let anchor = if circular && len > 0 { anchor % len } else { anchor };
        Drag { anchor, last: anchor, travel: 0, sent: (anchor, false) }
    }

    /// Move the pointer to `base` and return the selection's (head, wraps).
    fn to(&mut self, base: usize, len: usize, circular: bool) -> (usize, bool) {
        if !circular || len == 0 {
            return (base, false);
        }
        let n = len as i64;
        let base = base % len;
        // Assume the shorter way round between pointer events.
        let mut step = (base as i64 - self.last as i64).rem_euclid(n);
        if step > n / 2 {
            step -= n;
        }
        self.last = base;
        self.travel = (self.travel + step).clamp(-(n - 1), n - 1);
        let head = self.anchor as i64 + self.travel;
        if head > n {
            ((head - n) as usize, true)
        } else if head < 0 {
            ((head + n) as usize, true)
        } else {
            (head as usize, false)
        }
    }
}

fn register_mouse(m: MouseCtx, proj: Proj, hitbox: gpui::Hitbox, window: &mut Window) {
    let circular = m.circular && proj.curved();
    let mc = m.clone();
    let hb = hitbox.clone();
    window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
        if !phase.bubble() || ev.button != MouseButton::Left || !hb.is_hovered(window) {
            return;
        }
        window.focus(&mc.editor_focus);
        let (u, v) = proj.inverse(ev.position);
        let base = mc.geo.base_at(u);
        if let Some(it) = mc.geo.hit(u, v) {
            let f = &mc.features[it.feature];
            let len = mc.geo.len;
            let wraps = it.end > len && mc.circular;
            let (s, e) = if it.end <= len {
                (it.start, it.end)
            } else if wraps {
                // Wraps the origin: select all of it, through the origin.
                (it.start, it.end - len)
            } else {
                f.segments.iter().copied().find(|&(s, e)| s <= base && base <= e).unwrap_or(f.segments[0])
            };
            let msg: SharedString = format!("{} · {} · {} bp", map_label(&f.label), it.glyph.name(), it.end - it.start).into();
            mc.drag.set(None);
            mc.editor.update(cx, |ed, cx| ed.select(s, e, wraps, Some(msg), cx));
        } else if let Some(ids) = (v.abs() <= LOCK_BAND + 3.0 && !ev.modifiers.shift)
            .then(|| mc.locks.iter().filter(|l| l.contains(base % mc.geo.len.max(1))).map(|l| l.id).collect::<Vec<_>>())
            .filter(|ids| !ids.is_empty())
        {
            // On the locked band: bring its task forward.
            mc.drag.set(None);
            let at = ev.position;
            mc.editor.update(cx, |ed, cx| ed.locked_click(at, ids, cx));
        } else {
            let drag = Drag::new(base, mc.geo.len, circular);
            mc.drag.set(Some(drag));
            mc.editor.update(cx, |ed, cx| ed.select(drag.anchor, drag.anchor, false, None, cx));
        }
        cx.stop_propagation();
    });

    let mc = m.clone();
    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
        let Some(mut drag) = mc.drag.get() else { return };
        if !phase.bubble() || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let base = mc.geo.base_at(proj.inverse(ev.position).0);
        let (head, wraps) = drag.to(base, mc.geo.len, circular);
        let changed = drag.sent != (head, wraps);
        drag.sent = (head, wraps);
        mc.drag.set(Some(drag));
        if changed {
            mc.editor.update(cx, |ed, cx| ed.select(drag.anchor, head, wraps, None, cx));
        }
    });

    let drag = m.drag.clone();
    window.on_mouse_event(move |_: &MouseUpEvent, _, _, _| drag.set(None));
}

// ----- view ------------------------------------------------------------------------

impl Render for DnaMap {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ed = self.editor.read(cx);
        let len = ed.doc.len();
        let features = ed.doc.features.clone();
        let name = ed.doc.name.clone();
        let selection = ed.selection();
        let cursor = ed.cursor();
        let ed_circular = ed.doc.circular;
        let editor_focus = ed.focus_handle().clone();
        let focused = editor_focus.is_focused(window);

        let ts = window.text_system();
        let font_id = ts.resolve_font(&editor::editor_font());
        let label_char_w: f32 = ts.advance(font_id, px(LABEL_SIZE), 'A').map(|s| s.width.into()).unwrap_or(LABEL_SIZE * 0.6);

        let me = cx.entity();
        let viewport = self.viewport.clone();
        let measure = canvas(
            move |bounds, window, _| {
                let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let old = viewport.get();
                if (old.0 - size.0).abs() > 0.5 || (old.1 - size.1).abs() > 0.5 {
                    viewport.set(size);
                    // A notify during the draw is dropped; re-render once this frame is done.
                    window.on_next_frame(move |_, cx| me.update(cx, |_, cx| cx.notify()));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let root = div().relative().size_full().bg(theme::editor_bg()).child(measure);
        let (w, h) = self.viewport.get();
        if len == 0 {
            return root.child(div().p_4().text_sm().text_color(theme::muted_text()).child("Empty sequence"));
        }
        if w < 80.0 || h < 60.0 {
            return root;
        }

        let geo = Rc::new(Geo::compute(self.mode, &features, len, w, h, label_char_w));
        let mode = self.mode;
        let content_h = if mode == MapMode::Linear { geo.height.max(h) } else { h };
        let key = (mode, geo.height.round() as i32);
        if mode == MapMode::Linear && self.centred_for != Some(key) {
            self.centred_for = Some(key);
            let top = (geo.y_base - h / 2.0).clamp(0.0, (content_h - h).max(0.0));
            self.scroll.set_offset(point(px(0.0), px(-top)));
        }
        let locks = Rc::new(ed.doc.locks().to_vec());
        let paint = PaintCtx { geo: geo.clone(), name, selection, cursor, focused, locks: locks.clone() };
        let mouse = MouseCtx { geo: geo.clone(), features, editor: self.editor.clone(), editor_focus, circular: len > 0 && ed_circular, drag: self.drag.clone(), locks };

        let map = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, cx| {
                let ox = f32::from(bounds.origin.x);
                let oy = f32::from(bounds.origin.y);
                let bw = f32::from(bounds.size.width);
                let bh = f32::from(bounds.size.height);
                let proj = match mode {
                    MapMode::Linear => {
                        let slack = ((bh - paint.geo.height) / 2.0).max(0.0);
                        Proj::Linear { x0: ox + PAD, y: oy + slack + paint.geo.y_base }
                    }
                    MapMode::Circular => Proj::Circular { cx: ox + bw / 2.0, cy: oy + bh / 2.0, r: paint.geo.r },
                };
                paint_map(&paint, &proj, bounds, window, cx);
                register_mouse(mouse.clone(), proj, hitbox, window);
            },
        )
        .w_full()
        .h(px(content_h));

        match mode {
            MapMode::Linear => root.child(div().id("map-scroll").absolute().inset_0().overflow_y_scroll().track_scroll(&self.scroll).child(map)),
            MapMode::Circular => root.child(div().absolute().inset_0().child(map)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circular_drag_crosses_the_origin_either_way() {
        // Clockwise from 90 past the origin to 10.
        let mut d = Drag::new(90, 100, true);
        for b in [95, 99, 0, 5, 10] {
            d.to(b, 100, true);
        }
        assert_eq!(d.to(10, 100, true), (10, true));
        // Back the way it came, it unwraps.
        assert_eq!(d.to(95, 100, true), (95, false));

        // Anticlockwise from 10 past the origin to 90.
        let mut d = Drag::new(10, 100, true);
        for b in [5, 0, 95] {
            d.to(b, 100, true);
        }
        assert_eq!(d.to(90, 100, true), (90, true));

        // The origin itself is position 0, never a wrap.
        let mut d = Drag::new(100, 100, true);
        assert_eq!(d.anchor, 0);
        assert_eq!(d.to(20, 100, true), (20, false));
    }

    #[test]
    fn linear_drag_never_wraps() {
        let mut d = Drag::new(90, 100, false);
        assert_eq!(d.to(10, 100, false), (10, false));
    }
}

//! A pane showing a 3D structure attached to the document.
//!
//! The molecule is drawn by the software renderer in `structure_render` into a bitmap
//! that GPUI paints as an image; it is re-rendered only when the camera, size, colours
//! or selection change. Drag to rotate, right-drag (or Shift-drag) to pan, scroll to
//! zoom, double-click to reset. Clicking a residue selects its codons in the sequence,
//! for chains that line up with a CDS; selecting bases highlights their residues.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, prelude::*, px, App, Bounds, Context, Corners, Entity, HitboxBehavior, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point, PromptLevel, Rgba, RenderImage, ScrollWheelEvent,
    SharedString, Subscription, Window,
};

use crate::attach_dialog::{AttachDialog, AttachEvent};
use crate::editor::SeqEditor;
use crate::pgx::{ChainLinking, Structure};
use crate::structure::{self, ChainLink, Molecule, StructureFile};
use crate::structure_render::{self as render, Camera, Coloring, Frame, Sphere, Style, M3, V3};
use crate::theme;

/// Supersample the idle image while it has fewer device pixels than this.
const SUPERSAMPLE_BELOW: usize = 1_600_000;

#[derive(Clone, Copy, PartialEq)]
struct View {
    rot: M3,
    /// Multiplies the scale that fits the whole molecule.
    zoom: f32,
    /// Logical pixels.
    pan: (f32, f32),
}

#[derive(Clone, Copy)]
struct Drag {
    start: Point<Pixels>,
    last: Point<Pixels>,
    pan: bool,
    moved: bool,
    shift: bool,
}

/// A parsed structure and what's derived from it that doesn't depend on the camera.
struct Loaded {
    mol: Rc<Molecule>,
    center: V3,
    radius: f32,
    home: M3,
    alphafold: bool,
}

/// Links to the document's CDS features, for one document version.
struct Links {
    version: u64,
    links: Vec<ChainLink>,
    /// Per residue: the codons it was translated from and the feature it's in.
    spans: Vec<Option<(usize, usize, usize)>>,
}

#[derive(Clone, PartialEq)]
struct SceneKey {
    id: String,
    style: Style,
    coloring: Coloring,
    version: u64,
    selection: (usize, usize),
    theme: [u32; 2],
}

#[derive(Clone, PartialEq)]
struct ImageKey {
    scene: SceneKey,
    view: View,
    size: (usize, usize),
    ss: usize,
}

struct Image {
    key: ImageKey,
    image: Arc<RenderImage>,
    frame: Rc<Frame>,
}

pub struct StructureView {
    editor: Entity<SeqEditor>,
    current: Option<String>,
    style: Style,
    coloring: Coloring,
    view: Option<View>,
    loaded: HashMap<String, Result<Rc<Loaded>, SharedString>>,
    links: Option<(String, Links)>,
    scene: Option<(SceneKey, Rc<Vec<Sphere>>)>,
    image: Option<Image>,
    viewport: Rc<Cell<(f32, f32)>>,
    drag: Rc<Cell<Option<Drag>>>,
    hover: Option<usize>,
    notice: Option<SharedString>,
    loading: bool,
    /// The open Attach Structure / Link Chains dialog.
    dialog: Option<(Entity<AttachDialog>, Subscription)>,
    _observe: Subscription,
    _release: Subscription,
}

impl StructureView {
    pub fn new(editor: Entity<SeqEditor>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&editor, |_, _, cx| cx.notify());
        // The bitmap lives in the window's sprite atlas until it's dropped.
        let release = cx.on_release(|this: &mut Self, cx: &mut App| {
            if let Some(img) = this.image.take() {
                cx.drop_image(img.image, None);
            }
        });
        Self {
            editor,
            current: None,
            style: Style::Tube,
            coloring: Coloring::Rainbow,
            view: None,
            loaded: HashMap::new(),
            links: None,
            scene: None,
            image: None,
            viewport: Rc::new(Cell::new((0.0, 0.0))),
            drag: Rc::new(Cell::new(None)),
            hover: None,
            notice: None,
            loading: false,
            dialog: None,
            _observe: observe,
            _release: release,
        }
    }

    fn structures(&self, cx: &App) -> Vec<Structure> {
        self.editor.read(cx).doc.structures.clone()
    }

    /// The structure on show: the one picked, or else the first attached.
    fn shown(&mut self, cx: &App) -> Option<Structure> {
        let all = self.structures(cx);
        let s = self.current.as_ref().and_then(|id| all.iter().find(|s| &s.id == id)).or(all.first()).cloned();
        if self.current.as_ref() != s.as_ref().map(|s| &s.id) {
            self.current = s.as_ref().map(|s| s.id.clone());
            self.view = None;
        }
        // Forget structures that were removed.
        self.loaded.retain(|id, _| all.iter().any(|s| &s.id == id));
        s
    }

    fn load(&mut self, s: &Structure) -> Result<Rc<Loaded>, SharedString> {
        self.loaded
            .entry(s.id.clone())
            .or_insert_with(|| {
                let mol = Molecule::parse(&s.data, s.format).map_err(|e| SharedString::from(format!("Could not read {}: {e:#}", s.name)))?;
                let trace: Vec<V3> = mol.residues.iter().filter_map(|r| r.trace).map(|t| mol.atoms[t].pos).collect();
                let pts: Vec<V3> = if trace.len() >= 3 { trace } else { mol.atoms.iter().map(|a| a.pos).collect() };
                let n = pts.len().max(1) as f32;
                let mut center = [0.0f32; 3];
                for p in &pts {
                    for d in 0..3 {
                        center[d] += p[d] / n;
                    }
                }
                let radius = mol
                    .atoms
                    .iter()
                    .map(|a| {
                        let d = [a.pos[0] - center[0], a.pos[1] - center[1], a.pos[2] - center[2]];
                        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                    })
                    .fold(0.0f32, f32::max)
                    + 2.0;
                let home = render::principal_axes(&pts, center);
                let alphafold = mol.title.to_ascii_lowercase().contains("alphafold");
                Ok(Rc::new(Loaded { mol: Rc::new(mol), center, radius, home, alphafold }))
            })
            .clone()
    }

    fn links(&mut self, id: &str, mol: &Molecule, cx: &App) -> &Links {
        let doc = &self.editor.read(cx).doc;
        let fresh = self.links.as_ref().is_some_and(|(lid, l)| lid == id && l.version == doc.version);
        if !fresh {
            let linking = doc.structures.iter().find(|s| s.id == id).map_or(ChainLinking::None, |s| s.linking);
            let links = match linking {
                ChainLinking::Auto => structure::link_chains(mol, &doc.seq, &doc.features),
                ChainLinking::Features => structure::bound_links(mol, &doc.seq, &doc.features, id),
                ChainLinking::None => Vec::new(),
            };
            let mut spans = vec![None; mol.residues.len()];
            for l in &links {
                for &(r, span) in &l.spans {
                    if let Some((s, e)) = span {
                        spans[r] = Some((s, e, l.feature));
                    }
                }
            }
            self.links = Some((id.to_string(), Links { version: doc.version, links, spans }));
        }
        &self.links.as_ref().unwrap().1
    }

    fn set_style(&mut self, style: Style, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    fn set_coloring(&mut self, coloring: Coloring, cx: &mut Context<Self>) {
        self.coloring = coloring;
        cx.notify();
    }

    fn reset_view(&mut self, cx: &mut Context<Self>) {
        self.view = None;
        cx.notify();
    }

    fn pick(&mut self, id: String, cx: &mut Context<Self>) {
        self.current = Some(id);
        self.view = None;
        cx.notify();
    }

    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach Structure".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |v, cx| {
                v.loading = true;
                v.notice = None;
                cx.notify();
            })
            .ok();
            let read = cx.background_executor().spawn(async move { StructureFile::read(&path) }).await;
            this.update_in(cx, |v, window, cx| {
                v.loading = false;
                match read {
                    Ok(file) => {
                        let editor = v.editor.clone();
                        let dialog = cx.new(|cx| AttachDialog::attach(editor, file, window, cx));
                        v.show_dialog(dialog, window, cx);
                    }
                    Err(e) => v.notice = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Choose again which features the shown structure's chains are linked to.
    fn relink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.shown(cx) else { return };
        let Ok(loaded) = self.load(&s) else { return };
        let editor = self.editor.clone();
        let mol = (*loaded.mol).clone();
        let dialog = cx.new(|cx| AttachDialog::relink(editor, s.id.clone(), mol, window, cx));
        self.show_dialog(dialog, window, cx);
    }

    fn show_dialog(&mut self, dialog: Entity<AttachDialog>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(&dialog, window, |v, _, ev: &AttachEvent, window, cx| {
            v.dialog = None;
            if let AttachEvent::Done(id) = ev {
                v.pick(id.clone(), cx);
            }
            // Hand the keyboard back to the sequence.
            v.editor.read(cx).focus_handle().clone().focus(window);
            cx.notify();
        });
        self.dialog = Some((dialog, sub));
        cx.notify();
    }

    /// Show structure `id`.
    pub fn show(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.current.as_deref() != Some(id) {
            self.pick(id.to_string(), cx);
        }
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current.clone() else { return };
        let Some(name) = self.structures(cx).into_iter().find(|s| s.id == id).map(|s| s.name) else { return };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove {name} from this document?"),
            Some("The structure file is deleted from the document when it is next saved. This can't be undone."),
            &["Remove", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            this.update(cx, |v, cx| {
                v.editor.update(cx, |ed, cx| {
                    if let Some(ix) = ed.doc.structures.iter().position(|s| s.id == id) {
                        ed.doc.remove_structure(ix);
                        cx.notify();
                    }
                });
                v.current = None;
                v.view = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----- mouse ---------------------------------------------------------------------

    fn mouse_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        if ev.click_count >= 2 && ev.button == MouseButton::Left {
            self.reset_view(cx);
            return;
        }
        let pan = ev.button == MouseButton::Right || ev.button == MouseButton::Middle || ev.modifiers.shift && ev.button == MouseButton::Left;
        self.drag.set(Some(Drag { start: ev.position, last: ev.position, pan, moved: false, shift: ev.modifiers.shift }));
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, origin: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(mut d) = self.drag.get() else {
            let hover = self.residue_at(ev.position, origin);
            if hover != self.hover {
                self.hover = hover;
                cx.notify();
            }
            return;
        };
        if ev.pressed_button.is_none() {
            self.drag.set(None);
            return;
        }
        let (dx, dy) = (f32::from(ev.position.x - d.last.x), f32::from(ev.position.y - d.last.y));
        let total = ev.position - d.start;
        if !d.moved && f32::from(total.x).abs() + f32::from(total.y).abs() < 3.0 {
            return;
        }
        d.moved = true;
        d.last = ev.position;
        self.drag.set(Some(d));
        let Some(view) = self.view.as_mut() else { return };
        if d.pan {
            view.pan.0 += dx;
            view.pan.1 += dy;
        } else {
            view.rot = render::rotate(&view.rot, dx * 0.01, dy * 0.01);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, origin: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.take() else { return };
        if d.moved || d.pan {
            // The idle image is supersampled; the dragging one wasn't.
            cx.notify();
            return;
        }
        let Some(r) = self.residue_at(ev.position, origin) else { return };
        let Some((_, links)) = self.links.as_ref() else { return };
        let mol = match self.current.as_ref().and_then(|id| self.loaded.get(id)) {
            Some(Ok(l)) => l.mol.clone(),
            _ => return,
        };
        let label = mol.residue_label(r);
        let Some((s, e, fi)) = links.spans.get(r).copied().flatten() else {
            self.notice = Some(format!("{label} isn't encoded by a CDS in this sequence").into());
            cx.notify();
            return;
        };
        self.notice = None;
        let focus = self.editor.read(cx).focus_handle().clone();
        self.editor.update(cx, |ed, cx| {
            let feature = ed.doc.features.get(fi).map(|f| f.label.clone()).unwrap_or_default();
            let (mut a, mut b) = (s, e);
            if d.shift {
                // Extend the selection to take in this residue.
                let sel = ed.selection();
                if !sel.is_empty() && !sel.wraps() {
                    a = a.min(sel.start);
                    b = b.max(sel.end);
                }
            }
            let msg: SharedString = format!("{label} · {feature} {}–{}", a + 1, b).into();
            ed.select(a, b, false, Some(msg), cx);
        });
        window.focus(&focus);
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(view) = self.view.as_mut() else { return };
        let dy = f32::from(ev.delta.pixel_delta(px(20.0)).y);
        view.zoom = (view.zoom * (dy * 0.004).exp()).clamp(0.2, 40.0);
        cx.notify();
    }

    fn residue_at(&self, pos: Point<Pixels>, origin: Point<Pixels>) -> Option<usize> {
        let img = self.image.as_ref()?;
        let (w, _) = self.viewport.get();
        let k = img.frame.width as f32 / w.max(1.0);
        img.frame.residue_at(f32::from(pos.x - origin.x) * k, f32::from(pos.y - origin.y) * k)
    }

    // ----- drawing -------------------------------------------------------------------

    fn scene(&mut self, s: &Structure, loaded: &Loaded, cx: &App) -> (SceneKey, Rc<Vec<Sphere>>) {
        let ed = self.editor.read(cx);
        let sel = ed.selection();
        let len = ed.doc.len();
        let version = ed.doc.version;
        let key = SceneKey {
            id: s.id.clone(),
            style: self.style,
            coloring: self.coloring,
            version,
            selection: (sel.start, sel.end),
            theme: [rgb_key(theme::editor_bg()), rgb_key(theme::accent())],
        };
        if let Some((k, spheres)) = &self.scene {
            if *k == key {
                return (key, spheres.clone());
            }
        }
        let features = ed.doc.features.clone();
        let parts = sel.parts(len);
        let mol = &loaded.mol;
        let spans = self.links(&s.id, mol, cx).spans.clone();
        let highlight: Vec<bool> = spans
            .iter()
            .map(|sp| sp.is_some_and(|(a, b, _)| !sel.is_empty() && parts.iter().any(|p| p.start < b && a < p.end)))
            .collect();
        let feature_colors: Vec<Option<V3>> = spans.iter().map(|sp| sp.and_then(|(_, _, fi)| features.get(fi)).map(|f| v3(theme::feature(&f.kind)))).collect();
        let colors = render::residue_colors(mol, self.coloring, loaded.alphafold, &feature_colors);
        let spheres = Rc::new(render::build_spheres(mol, self.style, &colors, &highlight, v3(theme::accent())));
        self.scene = Some((key.clone(), spheres.clone()));
        (key, spheres)
    }

    fn image(&mut self, s: &Structure, loaded: &Loaded, window: &mut Window, cx: &mut Context<Self>) -> Option<(Arc<RenderImage>, Rc<Frame>)> {
        let scale_factor = window.scale_factor();
        let (w, h) = self.viewport.get();
        let (dw, dh) = ((w * scale_factor).round() as usize, (h * scale_factor).round() as usize);
        if dw < 8 || dh < 8 {
            return None;
        }
        let view = *self.view.get_or_insert(View { rot: loaded.home, zoom: 1.0, pan: (0.0, 0.0) });
        let dragging = self.drag.get().is_some_and(|d| d.moved);
        let ss = if dragging || dw * dh > SUPERSAMPLE_BELOW { 1 } else { 2 };
        let (scene, spheres) = self.scene(s, loaded, cx);
        let key = ImageKey { scene, view, size: (dw, dh), ss };
        if let Some(img) = &self.image {
            if img.key == key {
                return Some((img.image.clone(), img.frame.clone()));
            }
        }
        let fit = dw.min(dh) as f32 * 0.92 / (2.0 * loaded.radius);
        let cam = Camera { rot: view.rot, center: loaded.center, scale: fit * view.zoom, pan: (view.pan.0 * scale_factor, view.pan.1 * scale_factor) };
        let bg = v3(theme::editor_bg());
        let frame = render::render(&spheres, &cam, dw, dh, ss, bg, [bg[0] * 0.25, bg[1] * 0.25, bg[2] * 0.25]);
        let buf = image::RgbaImage::from_raw(dw as u32, dh as u32, frame.bgra.clone())?;
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        let frame = Rc::new(Frame { width: frame.width, height: frame.height, bgra: Vec::new(), residues: frame.residues });
        if let Some(old) = self.image.replace(Image { key, image: image.clone(), frame: frame.clone() }) {
            // While rendering, this window is leased out of the app's window list, so
            // `drop_image` only reaches its sprite atlas through the explicit argument.
            cx.drop_image(old.image, Some(window));
        }
        Some((image, frame))
    }

    fn render_toolbar(&self, all: &[Structure], cx: &mut Context<Self>) -> impl IntoElement {
        let segment = |key: SharedString, label: &'static str, selected: bool| {
            div()
                .id(key)
                .px_2()
                .py(px(1.0))
                .rounded_sm()
                .cursor_pointer()
                .when(selected, |d| d.bg(theme::active()).text_color(theme::accent()))
                .when(!selected, |d| d.text_color(theme::muted_text()).hover(|s| s.bg(theme::hover())))
                .child(label)
        };
        let group = || div().flex().flex_row().gap(px(2.0)).p(px(1.0)).rounded_md().border_1().border_color(theme::border());
        let button = |key: &'static str, label: &'static str| {
            div()
                .id(key)
                .px_2()
                .py(px(1.0))
                .rounded_sm()
                .cursor_pointer()
                .text_color(theme::muted_text())
                .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                .child(label)
        };

        let mut bar = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::border())
            .text_xs();

        if all.len() > 1 {
            let mut picker = group();
            for s in all {
                let id = s.id.clone();
                picker = picker.child(
                    segment(format!("pick-{id}").into(), "", self.current.as_ref() == Some(&s.id))
                        .child(s.name.clone())
                        .on_click(cx.listener(move |v, _, _, cx| v.pick(id.clone(), cx))),
                );
            }
            bar = bar.child(picker);
        } else if let Some(s) = all.first() {
            bar = bar.child(div().text_color(theme::text()).child(s.source.clone().unwrap_or_else(|| s.name.clone())));
        }

        let styles = [(Style::Tube, "Tube"), (Style::Atoms, "Atoms")];
        let mut style_group = group();
        for (st, label) in styles {
            style_group = style_group.child(
                segment(format!("style-{label}").into(), label, self.style == st).on_click(cx.listener(move |v, _, _, cx| v.set_style(st, cx))),
            );
        }
        let colorings = [
            (Coloring::Rainbow, "Rainbow"),
            (Coloring::Secondary, "Helix/Sheet"),
            (Coloring::Chain, "Chain"),
            (Coloring::BFactor, "B-factor"),
            (Coloring::Features, "Features"),
        ];
        let mut color_group = group();
        for (c, label) in colorings {
            color_group = color_group.child(
                segment(format!("color-{label}").into(), label, self.coloring == c).on_click(cx.listener(move |v, _, _, cx| v.set_coloring(c, cx))),
            );
        }
        bar.child(style_group)
            .child(color_group)
            .child(div().flex_1())
            .child(button("reset-view", "Reset").on_click(cx.listener(|v, _, _, cx| v.reset_view(cx))))
            .when(!all.is_empty(), |d| d.child(button("relink", "Link Chains…").on_click(cx.listener(|v, _, window, cx| v.relink(window, cx)))))
            .child(button("attach", "Attach…").on_click(cx.listener(|v, _, window, cx| v.attach(window, cx))))
            .child(button("remove", "Remove").on_click(cx.listener(|v, _, window, cx| v.remove(window, cx))))
    }
}

fn v3(c: Hsla) -> V3 {
    let c: Rgba = c.into();
    [c.r, c.g, c.b]
}

fn rgb_key(c: Hsla) -> u32 {
    let [r, g, b] = v3(c).map(|v| (v.clamp(0.0, 1.0) * 255.0) as u32);
    r << 16 | g << 8 | b
}

impl Render for StructureView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let all = self.structures(cx);
        let shown = self.shown(cx);

        let me = cx.entity();
        let viewport = self.viewport.clone();
        let measure = canvas(
            move |bounds, window, _| {
                let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let old = viewport.get();
                if (old.0 - size.0).abs() > 0.5 || (old.1 - size.1).abs() > 0.5 {
                    viewport.set(size);
                    window.on_next_frame(move |_, cx| me.update(cx, |_, cx| cx.notify()));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let toolbar = self.render_toolbar(&all, cx).into_any_element();
        let root = div().size_full().flex().flex_col().bg(theme::editor_bg()).text_color(theme::text())
            .children(self.dialog.as_ref().map(|d| d.0.clone()));

        let Some(s) = shown else {
            let message = if self.loading { "Reading structure…" } else { "No structure attached to this document." };
            return root.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_sm()
                    .child(div().text_color(theme::muted_text()).child(message))
                    .child(
                        div()
                            .id("attach-empty")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .cursor_pointer()
                            .hover(|d| d.bg(theme::hover()))
                            .child("Attach structure file…")
                            .on_click(cx.listener(|v, _, window, cx| v.attach(window, cx))),
                    )
                    .child(div().text_xs().text_color(theme::muted_text()).child("PDB or mmCIF, e.g. from rcsb.org or the AlphaFold DB"))
                    .when_some(self.notice.clone(), |d, n| d.child(div().text_xs().text_color(theme::error()).child(n))),
            );
        };

        let loaded = match self.load(&s) {
            Ok(l) => l,
            Err(e) => {
                return root.child(toolbar).child(div().p_4().text_sm().text_color(theme::error()).child(e));
            }
        };
        let mol = loaded.mol.clone();

        let image = self.image(&s, &loaded, window, cx);

        // Footer: which chains line up with which CDS, and what's under the mouse.
        let summary: SharedString = {
            let features = self.editor.read(cx).doc.features.clone();
            let links = &self.links(&s.id, &mol, cx).links;
            let protein_chains = mol.chains.iter().filter(|c| c.residues.clone().any(|r| mol.residues[r].trace.is_some() && mol.residues[r].polymer)).count();
            if links.is_empty() {
                format!("{} · {} residues · {}", s.name, mol.residues.iter().filter(|r| r.polymer).count(), if s.linking == ChainLinking::None { "not linked to the sequence" } else { "no chain linked to a feature" }).into()
            } else {
                let parts: Vec<String> = links
                    .iter()
                    .map(|l| {
                        let f = features.get(l.feature).map(|f| f.label.as_str()).unwrap_or("?");
                        format!("chain {} → {f} ({:.0}% identical)", mol.chains[l.chain].id, l.identity * 100.0)
                    })
                    .collect();
                let rest = protein_chains.saturating_sub(links.len());
                let tail = if rest > 0 { format!(" · {rest} other chain{}", if rest == 1 { "" } else { "s" }) } else { String::new() };
                format!("{}{tail}", parts.join(", ")).into()
            }
        };
        let hover: Option<SharedString> = self.hover.filter(|&r| r < mol.residues.len()).map(|r| {
            let span = self.links.as_ref().and_then(|(_, l)| l.spans.get(r).copied().flatten());
            match span {
                Some((a, b, _)) => format!("{} · bases {}–{}", mol.residue_label(r), a + 1, b).into(),
                None => mol.residue_label(r).into(),
            }
        });

        let me = cx.entity();
        let picture = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds: Bounds<Pixels>, hitbox, window, _| {
                if let Some((image, _)) = image {
                    window.paint_image(bounds, Corners::default(), image, 0, false).ok();
                }
                let origin = bounds.origin;
                let (m, hb) = (me.clone(), hitbox.clone());
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                    if phase.bubble() && hb.is_hovered(window) {
                        m.update(cx, |v, cx| v.mouse_down(ev, cx));
                        cx.stop_propagation();
                    }
                });
                let (m, hb) = (me.clone(), hitbox.clone());
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                    let dragging = m.read(cx).drag.get().is_some();
                    if phase.bubble() && (dragging || hb.is_hovered(window) || m.read(cx).hover.is_some()) {
                        m.update(cx, |v, cx| {
                            if !dragging && !hb.is_hovered(window) {
                                v.hover = None;
                                cx.notify();
                            } else {
                                v.mouse_move(ev, origin, cx);
                            }
                        });
                    }
                });
                let m = me.clone();
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                    if phase.bubble() && m.read(cx).drag.get().is_some() {
                        m.update(cx, |v, cx| v.mouse_up(ev, origin, window, cx));
                    }
                });
                let (m, hb) = (me.clone(), hitbox);
                window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                    if phase.bubble() && hb.is_hovered(window) {
                        m.update(cx, |v, cx| v.scroll(ev, cx));
                        cx.stop_propagation();
                    }
                });
            },
        )
        .size_full();

        let footer = div()
            .flex()
            .flex_row()
            .gap_3()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .text_xs()
            .text_color(theme::muted_text())
            .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(summary))
            .when_some(self.notice.clone(), |d, n| d.child(div().text_color(theme::error()).child(n)))
            .when_some(hover, |d, h| d.child(div().text_color(theme::text()).child(h)));

        root.child(toolbar).child(div().relative().flex_1().overflow_hidden().child(measure).child(picture)).child(footer)
    }
}

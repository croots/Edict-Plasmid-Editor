//! The Restriction pane: every NEB enzyme with how often it cuts the document, a
//! toggle to show its sites on the map and sequence, and a virtual digest of the
//! shown enzymes as an agarose gel, whose fragments can be opened as new files.

use std::rc::Rc;

use gpui::{
    canvas, div, fill, point, prelude::*, px, size, uniform_list, App, Bounds, Context, Entity, FontWeight, SharedString,
    Subscription, Task, Window,
};

use crate::editor::{editor_font, paint_text, thousands, SeqEditor};
use crate::enzymes::{self, Fragment};
use crate::settings;
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::workspace::{queue_new_document, NewDocument};

const ROW_H: f32 = 26.0;
const FRAGMENT_ROW_H: f32 = 40.0;
const GEL_W: f32 = 170.0;

/// Favorites until the user picks their own: the usual cloning enzymes.
const DEFAULT_FAVORITES: &[&str] = &[
    "BamHI", "BglII", "BsaI", "BsmBI", "EcoRI", "EcoRV", "HindIII", "KpnI", "NcoI", "NdeI", "NheI", "NotI", "PstI", "SacI", "SalI",
    "SpeI", "XbaI", "XhoI",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    TypeIis,
    Favorites,
    Shown,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::TypeIis, Filter::Favorites, Filter::Shown];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::TypeIis => "Type IIS",
            Filter::Favorites => "Favorites",
            Filter::Shown => "Shown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Enzymes,
    Digest,
}

pub struct RestrictionView {
    editor: Entity<SeqEditor>,
    search: Entity<TextInput>,
    filter: Filter,
    /// Only enzymes that cut at least once.
    cutters_only: bool,
    tab: Tab,
    favorites: Vec<String>,
    /// Cuts per enzyme (by index into [`enzymes::all`]) for a document version.
    counts: Option<(u64, Rc<Vec<usize>>)>,
    counting: Option<(u64, Task<()>)>,
    _subscriptions: Vec<Subscription>,
}

impl RestrictionView {
    pub fn new(editor: Entity<SeqEditor>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("", "Search enzymes or sites", cx));
        let subscriptions = vec![
            cx.observe(&editor, |this, _, cx| {
                this.count(cx);
                cx.notify();
            }),
            cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Changed) {
                    cx.notify();
                }
            }),
        ];
        let favorites = settings::shared()
            .favorite_enzymes
            .unwrap_or_else(|| DEFAULT_FAVORITES.iter().map(|s| s.to_string()).collect());
        let mut view = Self {
            editor,
            search,
            filter: Filter::All,
            cutters_only: false,
            tab: Tab::Enzymes,
            favorites,
            counts: None,
            counting: None,
            _subscriptions: subscriptions,
        };
        view.count(cx);
        view
    }

    /// Count every enzyme's cuts off the main thread, once per document version.
    fn count(&mut self, cx: &mut Context<Self>) {
        let doc = &self.editor.read(cx).doc;
        let version = doc.version;
        if self.counts.as_ref().is_some_and(|c| c.0 == version) || self.counting.as_ref().is_some_and(|c| c.0 == version) {
            return;
        }
        let seq: Vec<u8> = (*doc.seq).clone();
        let circular = doc.circular;
        let task = cx.spawn(async move |this, cx| {
            let counts = cx
                .background_executor()
                .spawn(async move { (0..enzymes::all().len()).map(|i| enzymes::count_cuts(&seq, circular, i)).collect::<Vec<_>>() })
                .await;
            this.update(cx, |this, cx| {
                this.counts = Some((version, Rc::new(counts)));
                if this.counting.as_ref().is_some_and(|c| c.0 == version) {
                    this.counting = None;
                }
                cx.notify();
            })
            .ok();
        });
        // Replacing the task drops (cancels) a count for an older version.
        self.counting = Some((version, task));
    }

    fn is_favorite(&self, name: &str) -> bool {
        self.favorites.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    fn toggle_favorite(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.is_favorite(name) {
            self.favorites.retain(|f| !f.eq_ignore_ascii_case(name));
        } else {
            self.favorites.push(name.to_string());
            self.favorites.sort_by_key(|f| f.to_ascii_lowercase());
        }
        let favorites = self.favorites.clone();
        if let Err(e) = settings::update_shared(|s| s.favorite_enzymes = Some(favorites)) {
            eprintln!("favorites not saved: {e:#}");
        }
        cx.notify();
    }

    /// The enzymes the search and filter leave, in alphabetical order.
    fn visible(&self, shown: &[usize], counts: Option<&[usize]>, cx: &App) -> Vec<usize> {
        let query = self.search.read(cx).text().trim().to_ascii_uppercase();
        enzymes::all()
            .iter()
            .enumerate()
            .filter(|(i, e)| match self.filter {
                Filter::All => true,
                Filter::TypeIis => e.type_iis(),
                Filter::Favorites => self.is_favorite(e.name),
                Filter::Shown => shown.contains(i),
            })
            .filter(|(i, _)| !self.cutters_only || counts.is_none_or(|c| c[*i] > 0))
            .filter(|(_, e)| query.is_empty() || e.name.to_ascii_uppercase().contains(&query) || e.site.contains(&query))
            .map(|(i, _)| i)
            .collect()
    }

    fn render_header(&self, shown: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |id: &'static str, label: SharedString, t: Tab, cx: &mut Context<Self>| {
            let active = self.tab == t;
            div()
                .id(id)
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .cursor_pointer()
                .when(active, |d| d.bg(theme::active()).text_color(theme::accent()))
                .when(!active, |d| d.text_color(theme::muted_text()).hover(|s| s.bg(theme::hover())))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = t;
                    cx.notify();
                }))
        };
        let clear = (shown > 0).then(|| {
            div()
                .id("enzymes-clear")
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .cursor_pointer()
                .text_color(theme::muted_text())
                .hover(|s| s.bg(theme::hover()))
                .child("Hide all")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.editor.update(cx, |ed, cx| ed.set_shown_enzymes(Vec::new(), cx));
                }))
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme::border())
            .text_sm()
            .child(tab("enzymes-tab", "Enzymes".into(), Tab::Enzymes, cx))
            .child(tab(
                "digest-tab",
                if shown > 0 { format!("Digest ({shown})").into() } else { "Digest".into() },
                Tab::Digest,
                cx,
            ))
            .child(div().flex_1())
            .children(clear)
    }

    fn render_enzymes(&self, shown: Vec<usize>, counts: Option<Rc<Vec<usize>>>, cx: &mut Context<Self>) -> impl IntoElement {
        let visible = self.visible(&shown, counts.as_deref().map(|c| c.as_slice()), cx);
        let chip = |f: Filter, cx: &mut Context<Self>| {
            let active = self.filter == f;
            div()
                .id(("enzyme-filter", f as usize))
                .px_2()
                .py(px(1.0))
                .rounded_full()
                .border_1()
                .cursor_pointer()
                .when(active, |d| d.bg(theme::accent()).border_color(theme::accent()).text_color(theme::editor_bg()))
                .when(!active, |d| d.border_color(theme::border()).hover(|s| s.bg(theme::hover())))
                .child(f.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter = f;
                    cx.notify();
                }))
        };
        let cutters_only = self.cutters_only;
        let chips = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_1()
            .text_xs()
            .children(Filter::ALL.map(|f| chip(f, cx)))
            .child(div().flex_1())
            .child(
                div()
                    .id("enzyme-cutters")
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_color(theme::muted_text())
                    .child(check(cutters_only))
                    .child("Cutters only")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cutters_only = !this.cutters_only;
                        cx.notify();
                    })),
            );
        let search = div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::app_bg())
            .text_sm()
            .child(self.search.clone());

        let favorites = self.favorites.clone();
        let empty = visible.is_empty().then(|| {
            let text = match self.filter {
                Filter::Shown => "No enzymes shown. Tick one in another list to draw its sites.",
                Filter::Favorites if favorites.is_empty() => "No favorites yet. Star an enzyme to add it here.",
                _ => "No enzymes match.",
            };
            div().p_3().text_xs().text_color(theme::muted_text()).child(text)
        });
        let n = visible.len();
        let list = uniform_list(
            "enzyme-rows",
            n,
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let ix = visible[row];
                        let e = &enzymes::all()[ix];
                        let on = shown.contains(&ix);
                        let fav = favorites.iter().any(|f| f.eq_ignore_ascii_case(e.name));
                        let count = counts.as_ref().map(|c| c[ix]);
                        let count_colour = match count {
                            Some(0) | None => theme::muted_text(),
                            Some(1) => theme::accent(),
                            _ => theme::text(),
                        };
                        div()
                            .id(("enzyme-row", ix))
                            .h(px(ROW_H))
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .when(on, |d| d.bg(theme::active()))
                            .child(check(on))
                            .child(
                                div()
                                    .w(px(78.0))
                                    .flex_none()
                                    .overflow_hidden()
                                    .when(on, |d| d.text_color(theme::accent()).font_weight(FontWeight::SEMIBOLD))
                                    .child(e.name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_xs()
                                    .font(editor_font())
                                    .text_color(theme::muted_text())
                                    .child(format!("{}  {}", e.specificity(), e.end_description())),
                            )
                            .child(
                                div()
                                    .id(("enzyme-star", ix))
                                    .w(px(16.0))
                                    .flex_none()
                                    .text_color(if fav { theme::accent() } else { theme::muted_text().opacity(0.5) })
                                    .hover(|s| s.text_color(theme::accent()))
                                    .child(if fav { "★" } else { "☆" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_favorite(enzymes::all()[ix].name, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .w(px(34.0))
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .text_xs()
                                    .text_color(count_colour)
                                    .child(count.map_or("…".to_string(), |c| thousands(c))),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.editor.update(cx, |ed, cx| ed.set_enzyme_shown(ix, !on, cx));
                            }))
                    })
                    .collect()
            }),
        )
        .flex_1();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(div().flex().flex_col().gap_1().p_2().border_b_1().border_color(theme::border()).child(search).child(chips))
            .child(
                div()
                    .px_2()
                    .py(px(3.0))
                    .flex()
                    .flex_row()
                    .text_xs()
                    .text_color(theme::muted_text())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().flex_1().child(format!("{n} enzyme{}", if n == 1 { "" } else { "s" })))
                    .child("cuts"),
            )
            .children(empty)
            .child(list)
    }

    fn render_digest(&self, shown: &[usize], cx: &mut Context<Self>) -> gpui::AnyElement {
        let ed = self.editor.read(cx);
        let n = ed.doc.len();
        let circular = ed.doc.circular;
        if shown.is_empty() {
            return div()
                .p_3()
                .text_sm()
                .text_color(theme::muted_text())
                .child("Show one or more enzymes in the Enzymes list to digest the sequence with them.")
                .into_any_element();
        }
        if n == 0 {
            return div().p_3().text_sm().text_color(theme::muted_text()).child("The sequence is empty.").into_any_element();
        }
        let sites = ed.enzyme_sites();
        let fragments = Rc::new(enzymes::digest(n, circular, &sites));
        let names: Vec<&'static str> = shown.iter().map(|&i| enzymes::all()[i].name).collect();
        let lane_label = names.join(" + ");

        let gel_fragments = fragments.clone();
        let gel = canvas(|_, _, _| (), move |bounds, _, window, cx| paint_gel(bounds, &gel_fragments, window, cx))
            .w(px(GEL_W))
            .h_full()
            .flex_none();

        let summary = if fragments.len() == 1 && fragments[0].circular {
            format!("{lane_label} doesn't cut: the plasmid stays circular.")
        } else {
            format!("{lane_label}: {} fragment{}", fragments.len(), if fragments.len() == 1 { "" } else { "s" })
        };

        // Largest first, as they sit on the gel.
        let mut order: Vec<usize> = (0..fragments.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(fragments[i].len));
        let doc_name = ed.doc.name.clone();
        let frags = fragments.clone();
        let rows = uniform_list(
            "fragment-rows",
            order.len(),
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let i = order[row];
                        let f = frags[i].clone();
                        let end = (f.start + f.len) % n.max(1);
                        let ends = format!("{} … {}", end_name(f.left.as_ref()), end_name(f.right.as_ref()));
                        let place = if f.circular {
                            "uncut circle".to_string()
                        } else {
                            format!("{}–{}{}", thousands(f.start + 1), thousands(if end == 0 { n } else { end }), if f.start + f.len > n { " (through the origin)" } else { "" })
                        };
                        let stem = fragment_stem(&doc_name, &f, i);
                        div()
                            .id(("fragment-row", i))
                            .h(px(FRAGMENT_ROW_H))
                            .px_2()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .child(
                                div()
                                    .w(px(70.0))
                                    .flex_none()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{} bp", thousands(f.len))),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .child(div().text_xs().whitespace_nowrap().child(ends))
                                    .child(div().text_xs().whitespace_nowrap().text_color(theme::muted_text()).child(place)),
                            )
                            .child(
                                div()
                                    .id(("fragment-open", i))
                                    .flex_none()
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme::border())
                                    .text_xs()
                                    .hover(|s| s.bg(theme::active()))
                                    .child("Open")
                                    .on_click({
                                        let f = f.clone();
                                        let stem = stem.clone();
                                        cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.open_fragment(&f, &stem, window, cx);
                                        })
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.select_fragment(&f, cx)))
                    })
                    .collect()
            }),
        )
        .flex_1();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme::muted_text())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(summary),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(div().p_2().h_full().flex_none().child(gel))
                    .child(div().flex().flex_col().flex_1().min_w_0().border_l_1().border_color(theme::border()).child(rows)),
            )
            .into_any_element()
    }

    fn select_fragment(&mut self, f: &Fragment, cx: &mut Context<Self>) {
        self.editor.update(cx, |ed, cx| {
            let n = ed.doc.len();
            if f.circular || n == 0 {
                ed.select(0, n, false, None, cx);
            } else {
                let end = f.start + f.len;
                ed.select(f.start, end % n, end > n, Some(format!("{} bp fragment", thousands(f.len)).into()), cx);
            }
        });
    }

    fn open_fragment(&mut self, f: &Fragment, stem: &str, window: &mut Window, cx: &mut Context<Self>) {
        let doc = &self.editor.read(cx).doc;
        let seq = enzymes::fragment_bases(&doc.seq, f);
        let features = enzymes::fragment_features(&doc.features, doc.len(), f);
        queue_new_document(NewDocument { stem: stem.to_string(), seq, features, circular: f.circular }, window, cx);
    }
}

/// A small tick box.
fn check(on: bool) -> impl IntoElement {
    div()
        .w(px(12.0))
        .h(px(12.0))
        .flex_none()
        .rounded_sm()
        .border_1()
        .border_color(if on { theme::accent() } else { theme::muted_text() })
        .when(on, |d| d.bg(theme::accent()))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(9.0))
        .text_color(theme::editor_bg())
        .when(on, |d| d.child("✓"))
}

fn end_name(e: Option<&enzymes::End>) -> String {
    match e {
        None => "end".into(),
        Some(e) => {
            let name = enzymes::all()[e.enzyme].name;
            match e.overhang {
                0 => format!("{name} (blunt)"),
                o if o > 0 => format!("{name} (5′ {o})"),
                o => format!("{name} (3′ {})", -o),
            }
        }
    }
}

/// `pUC19_EcoRI-BamHI_2686bp`: the parent, the enzymes at its ends, its size.
fn fragment_stem(doc: &str, f: &Fragment, i: usize) -> String {
    let name = |e: Option<&enzymes::End>| e.map(|e| enzymes::all()[e.enzyme].name);
    let ends = match (name(f.left.as_ref()), name(f.right.as_ref())) {
        _ if f.circular => "uncut".to_string(),
        (Some(a), Some(b)) if a == b => a.to_string(),
        (Some(a), Some(b)) => format!("{a}-{b}"),
        (Some(a), None) | (None, Some(a)) => a.to_string(),
        (None, None) => format!("fragment{}", i + 1),
    };
    let clean: String = doc.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{clean}_{ends}_{}bp", f.len)
}

/// The gel: the 1 kb Plus ladder beside one lane of the digest.
fn paint_gel(bounds: Bounds<gpui::Pixels>, fragments: &[Fragment], window: &mut Window, cx: &mut App) {
    let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    // A gel reads as a gel in any theme: dark slab, light bands.
    let slab = gpui::hsla(0.6, 0.12, 0.10, 1.0);
    let band = gpui::hsla(0.15, 0.25, 0.92, 1.0);
    let text = gpui::hsla(0.0, 0.0, 0.70, 1.0);
    window.paint_quad(fill(bounds, slab).corner_radii(px(4.0)));

    let label_w = 44.0;
    let lane_w = (w - label_w - 18.0) / 2.0;
    let ladder_x = x0 + label_w;
    let digest_x = ladder_x + lane_w + 10.0;
    let top = y0 + 26.0;
    let span = (h - 34.0).max(40.0);
    let y_of = |bp: f32| top + enzymes::migration(bp) * span;

    // Lane headers and wells.
    paint_text("L".into(), ladder_x + lane_w / 2.0 - 3.0, y0 + 4.0, 10.0, 12.0, text, window, cx);
    paint_text("D".into(), digest_x + lane_w / 2.0 - 3.0, y0 + 4.0, 10.0, 12.0, text, window, cx);
    for x in [ladder_x, digest_x] {
        let well = Bounds::new(point(px(x), px(top - 8.0)), size(px(lane_w), px(4.0)));
        window.paint_quad(fill(well, gpui::hsla(0.0, 0.0, 0.0, 0.6)));
    }

    let paint_band = |window: &mut Window, x: f32, y: f32, alpha: f32, thick: f32| {
        let b = Bounds::new(point(px(x), px(y - thick / 2.0)), size(px(lane_w), px(thick)));
        window.paint_quad(fill(b, band.opacity(alpha)).corner_radii(px(1.0)));
    };

    for &bp in enzymes::LADDER {
        let y = y_of(bp as f32);
        let bright = enzymes::LADDER_BRIGHT.contains(&bp);
        paint_band(window, ladder_x, y, if bright { 0.95 } else { 0.55 }, if bright { 3.0 } else { 2.0 });
        let label = if bp >= 1000 { format!("{}k", bp as f32 / 1000.0) } else { bp.to_string() };
        paint_text(label.clone(), ladder_x - 6.0 - label.len() as f32 * 5.6, y - 6.0, 9.5, 12.0, text, window, cx);
    }

    // Equal moles of each fragment: a band's brightness follows its mass, and
    // fragments of one size stack into one brighter band.
    let max = fragments.iter().map(|f| f.len).max().unwrap_or(1).max(1) as f32;
    let mut bands: Vec<(f32, f32)> = Vec::new();
    for f in fragments {
        let size = if f.circular { f.len as f32 * enzymes::SUPERCOILED } else { f.len as f32 };
        let y = y_of(size);
        match bands.iter_mut().find(|b| (b.0 - y).abs() < 2.0) {
            Some(b) => b.1 += f.len as f32 / max,
            None => bands.push((y, f.len as f32 / max)),
        }
    }
    for (y, mass) in bands {
        let alpha = (0.35 + 0.65 * mass.sqrt()).min(1.0);
        paint_band(window, digest_x, y, alpha, if mass > 1.0 { 4.0 } else { 3.0 });
    }
}

impl Render for RestrictionView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown: Vec<usize> = self.editor.read(cx).shown_enzymes().to_vec();
        let counts = self.counts.as_ref().map(|c| c.1.clone());
        let header = self.render_header(shown.len(), cx);
        let body = match self.tab {
            Tab::Enzymes => self.render_enzymes(shown, counts, cx).into_any_element(),
            Tab::Digest => self.render_digest(&shown, cx),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::editor_bg())
            .text_color(theme::text())
            .text_sm()
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_names() {
        let ecori = enzymes::by_name("EcoRI").unwrap();
        let bamhi = enzymes::by_name("BamHI").unwrap();
        let end = |enzyme| Some(enzymes::End { enzyme, overhang: 4 });
        let f = Fragment { start: 10, len: 2686, left: end(ecori), right: end(bamhi), circular: false };
        assert_eq!(fragment_stem("pUC 19", &f, 0), "pUC_19_EcoRI-BamHI_2686bp");
        let f = Fragment { start: 10, len: 500, left: end(ecori), right: end(ecori), circular: false };
        assert_eq!(fragment_stem("p", &f, 0), "p_EcoRI_500bp");
        let f = Fragment { start: 0, len: 5000, left: None, right: None, circular: true };
        assert_eq!(fragment_stem("p", &f, 0), "p_uncut_5000bp");
    }

    #[test]
    fn default_favorites_exist() {
        for name in DEFAULT_FAVORITES {
            assert!(enzymes::by_name(name).is_some(), "{name} isn't in the catalogue");
        }
    }
}

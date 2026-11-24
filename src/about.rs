//! The About panel: app version and the licenses of everything that ships in the binary.
//!
//! `licenses/rust-crates.tsv` lists every Rust crate linked into the app. Regenerate it
//! with `scripts/licenses.sh` after changing dependencies.

use gpui::{div, prelude::*, px, uniform_list, Context, EventEmitter, FontWeight, MouseButton, SharedString, Window};

use crate::editor::editor_font;
use crate::theme;

/// One component and its license.
struct Notice {
    name: &'static str,
    what: &'static str,
    license: &'static str,
    url: &'static str,
    text: &'static str,
}

const APACHE: &str = include_str!("../licenses/Apache-2.0.txt");
const CRATES: &str = include_str!("../licenses/rust-crates.tsv");

const NOTICES: &[Notice] = &[
    Notice {
        name: "libghostty-vt 1.0.1",
        what: "Terminal emulation in the terminal pane, from the Ghostty terminal.",
        license: "MIT",
        url: "https://ghostty.org",
        text: include_str!("../licenses/ghostty.txt"),
    },
    Notice {
        name: "ghostty-vt, ghostty-vt-sys",
        what: "Rust bindings to libghostty-vt.",
        license: "Apache-2.0",
        url: "https://github.com/prabirshrestha/gpui-cn",
        text: APACHE,
    },
    Notice {
        name: "simdutf",
        what: "UTF-8 validation and decoding, compiled into libghostty-vt.",
        license: "MIT OR Apache-2.0",
        url: "https://github.com/simdutf/simdutf",
        text: include_str!("../licenses/simdutf.txt"),
    },
    Notice {
        name: "Highway",
        what: "Portable SIMD, compiled into libghostty-vt.",
        license: "Apache-2.0 OR BSD-3-Clause",
        url: "https://github.com/google/highway",
        text: include_str!("../licenses/highway.txt"),
    },
    Notice {
        name: "Wuffs",
        what: "Safe decoders, compiled into libghostty-vt.",
        license: "Apache-2.0 OR MIT",
        url: "https://github.com/google/wuffs",
        text: include_str!("../licenses/wuffs.txt"),
    },
    Notice {
        name: "uucode",
        what: "Unicode properties, compiled into libghostty-vt.",
        license: "MIT",
        url: "https://github.com/jacobsandlund/uucode",
        text: include_str!("../licenses/uucode.txt"),
    },
    Notice {
        name: "Unicode Character Database",
        what: "Character data used by libghostty-vt and several crates.",
        license: "Unicode-3.0",
        url: "https://www.unicode.org/license.txt",
        text: include_str!("../licenses/unicode.txt"),
    },
    Notice {
        name: "portable-pty",
        what: "Starts the shell of the terminal pane, from WezTerm.",
        license: "MIT",
        url: "https://github.com/wezterm/wezterm",
        text: include_str!("../licenses/portable-pty.txt"),
    },
    Notice {
        name: "GPUI",
        what: "The UI framework, from the Zed editor.",
        license: "Apache-2.0",
        url: "https://github.com/zed-industries/zed",
        text: APACHE,
    },
    Notice {
        name: "base16 color schemes",
        what: "The bundled themes, from Tinted Theming.",
        license: "MIT",
        url: "https://github.com/tinted-theming/schemes",
        text: include_str!("../licenses/base16-schemes.txt"),
    },
];

/// The list entry after the notices: the table of Rust crates.
const CRATES_ENTRY: usize = NOTICES.len();

#[derive(Clone)]
struct Crate {
    name: SharedString,
    version: SharedString,
    license: SharedString,
    url: SharedString,
}

pub struct AboutPanel {
    selected: usize,
    crates: Vec<Crate>,
}

/// The panel asks to be closed.
pub struct Close;

impl EventEmitter<Close> for AboutPanel {}

impl AboutPanel {
    pub fn new() -> Self {
        let crates = CRATES
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some(Crate {
                    name: f.next()?.to_string().into(),
                    version: f.next()?.to_string().into(),
                    license: f.next()?.to_string().into(),
                    url: f.next().unwrap_or_default().to_string().into(),
                })
            })
            .collect();
        Self { selected: 0, crates }
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        cx.notify();
    }

    fn entry(&self, ix: usize, name: SharedString, license: SharedString, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected == ix;
        div()
            .id(("about-entry", ix))
            .flex()
            .flex_col()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::active()))
            .hover(|s| s.bg(theme::hover()))
            .child(div().text_sm().when(selected, |d| d.text_color(theme::accent())).child(name))
            .child(div().text_xs().text_color(theme::muted_text()).child(license))
            .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
    }

    fn detail(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.selected == CRATES_ENTRY {
            let crates = self.crates.clone();
            let header = div()
                .text_sm()
                .text_color(theme::muted_text())
                .pb_2()
                .child(format!(
                    "{} Rust crates are compiled into this app, each under the license shown. The full list, with \
                     each crate's repository, is in licenses/rust-crates.tsv.",
                    crates.len()
                ));
            let list = uniform_list(
                "about-crates",
                crates.len(),
                cx.processor(move |_, range: std::ops::Range<usize>, _, _| {
                    range
                        .map(|ix| {
                            let c = &crates[ix];
                            let url = c.url.clone();
                            div()
                                .id(("crate", ix))
                                .flex()
                                .flex_row()
                                .gap_2()
                                .h(px(22.0))
                                .items_center()
                                .text_xs()
                                .when(!url.is_empty(), |d| {
                                    d.cursor_pointer()
                                        .hover(|s| s.bg(theme::hover()))
                                        .on_click(move |_, _, cx| cx.open_url(&url))
                                })
                                .child(div().w(px(190.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.name.clone()))
                                .child(div().w(px(70.0)).text_color(theme::muted_text()).child(c.version.clone()))
                                .child(div().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(c.license.clone()))
                        })
                        .collect()
                }),
            )
            .flex_1();
            return div().flex().flex_col().size_full().child(header).child(list).into_any_element();
        }
        let n = &NOTICES[self.selected];
        div()
            .id("about-text")
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .overflow_y_scroll()
            .child(div().text_sm().child(n.what))
            .child(
                div()
                    .id("about-url")
                    .text_xs()
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .child(format!("{} ↗", n.url))
                    .on_click({
                        let url = n.url;
                        move |_, _, cx| cx.open_url(url)
                    }),
            )
            .child(
                div()
                    .mt_1()
                    .p_3()
                    .rounded_md()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .font(editor_font())
                    .text_xs()
                    .child(n.text.trim().to_string()),
            )
            .into_any_element()
    }
}

impl Render for AboutPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().id("about-list").flex().flex_col().gap(px(2.0)).w(px(230.0)).h_full().overflow_y_scroll().pr_2();
        for (ix, n) in NOTICES.iter().enumerate() {
            list = list.child(self.entry(ix, n.name.into(), n.license.into(), cx));
        }
        list = list.child(div().my_1().h(px(1.0)).bg(theme::border())).child(self.entry(
            CRATES_ENTRY,
            "Rust crates".into(),
            format!("{} crates · MIT, Apache-2.0, …", self.crates.len()).into(),
            cx,
        ));

        div()
            .id("about")
            .flex()
            .flex_col()
            .w(px(760.0))
            .h(px(560.0))
            .max_h_full()
            .bg(theme::editor_bg())
            .text_color(theme::text())
            .border_1()
            .border_color(theme::border())
            .rounded_lg()
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Plasmid {}", env!("CARGO_PKG_VERSION"))))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::muted_text())
                                    .child("A plasmid sequence editor built on GPUI. Third-party software and licenses:"),
                            ),
                    )
                    .child(
                        div()
                            .id("about-close")
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme::muted_text())
                            .hover(|s| s.bg(theme::hover()))
                            .child("×")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
                    ),
            )
            .child(div().flex().flex_row().flex_1().min_h_0().p_4().gap_4().child(list).child(
                div().flex_1().min_w_0().h_full().child(self.detail(cx)),
            ))
    }
}

//! A single-line text field: typing, selection by mouse or Shift+arrows, clipboard.
//!
//! Text the field can't show scrolls sideways to keep the cursor in sight. The field
//! reports Enter, Tab and Up/Down to its owner as events instead of acting on them,
//! so a form can move between fields and drive suggestion lists.

use std::ops::Range;

use gpui::{
    canvas, div, fill, point, prelude::*, px, size, Bounds, ClipboardItem, ContentMask, Context, CursorStyle,
    EventEmitter, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, SharedString,
    ShapedLine, TextRun, Window,
};

use crate::theme;

pub enum InputEvent {
    Changed,
    /// Enter.
    Submit,
    /// Tab, or Shift+Tab when `back`.
    Tab { back: bool },
    Up,
    Down,
}

pub struct TextInput {
    focus: FocusHandle,
    text: String,
    placeholder: SharedString,
    /// Byte offsets; the selection runs between them.
    anchor: usize,
    head: usize,
    /// How far the text is scrolled left, in pixels.
    scroll: f32,
    /// The last painted line and the x it was painted at, for mouse hit-testing.
    painted: Option<(ShapedLine, f32)>,
    /// Where the text area was last painted, in window coordinates, so a form can hang
    /// a suggestion list under it.
    bounds: Option<Bounds<Pixels>>,
    selecting: bool,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(text: impl Into<String>, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            focus: cx.focus_handle(),
            text,
            placeholder: placeholder.into(),
            anchor: end,
            head: end,
            scroll: 0.0,
            painted: None,
            bounds: None,
            selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace the text and put the cursor at its end.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.anchor = self.text.len();
        self.head = self.text.len();
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.bounds
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Focus the field with all of its text selected, ready to be typed over.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.anchor = 0;
        self.head = self.text.len();
        cx.notify();
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    fn prev_boundary(&self, ix: usize) -> usize {
        self.text[..ix].chars().next_back().map_or(0, |c| ix - c.len_utf8())
    }

    fn next_boundary(&self, ix: usize) -> usize {
        self.text[ix..].chars().next().map_or(ix, |c| ix + c.len_utf8())
    }

    fn move_to(&mut self, ix: usize, extend: bool) {
        self.head = ix;
        if !extend {
            self.anchor = ix;
        }
    }

    /// Replace the selection with `s`. Line breaks become spaces: the field is one line.
    fn insert(&mut self, s: &str, cx: &mut Context<Self>) {
        let s: String = s.chars().map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c }).collect();
        let sel = self.selection();
        self.text.replace_range(sel.clone(), &s);
        self.move_to(sel.start + s.len(), false);
        cx.emit(InputEvent::Changed);
    }

    /// Delete the selection, or else `range`.
    fn delete(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let sel = self.selection();
        let range = if sel.is_empty() { range } else { sel };
        if range.is_empty() {
            return;
        }
        self.text.replace_range(range.clone(), "");
        self.move_to(range.start, false);
        cx.emit(InputEvent::Changed);
    }

    fn index_at(&self, x: Pixels) -> usize {
        match &self.painted {
            Some((line, ox)) => line.closest_index_for_x(px(f32::from(x) - ox)).min(self.text.len()),
            None => self.text.len(),
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let cmd = m.control || m.platform;
        let len = self.text.len();
        let sel = self.selection();
        match (cmd, ks.key.as_str()) {
            (true, "a") => {
                self.anchor = 0;
                self.head = len;
            }
            (true, "c") | (true, "x") => {
                if sel.is_empty() {
                    return;
                }
                cx.write_to_clipboard(ClipboardItem::new_string(self.text[sel.clone()].to_string()));
                if ks.key == "x" {
                    self.delete(sel, cx);
                }
            }
            (true, "v") => {
                if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    self.insert(&text, cx);
                }
            }
            (_, "left") => {
                let to = if cmd {
                    0
                } else if !m.shift && !sel.is_empty() {
                    sel.start
                } else {
                    self.prev_boundary(self.head)
                };
                self.move_to(to, m.shift);
            }
            (_, "right") => {
                let to = if cmd {
                    len
                } else if !m.shift && !sel.is_empty() {
                    sel.end
                } else {
                    self.next_boundary(self.head)
                };
                self.move_to(to, m.shift);
            }
            (_, "home") => self.move_to(0, m.shift),
            (_, "end") => self.move_to(len, m.shift),
            (_, "backspace") => {
                let from = if cmd { 0 } else { self.prev_boundary(self.head) };
                self.delete(from..self.head, cx);
            }
            (_, "delete") => {
                let to = if cmd { len } else { self.next_boundary(self.head) };
                self.delete(self.head..to, cx);
            }
            (_, "enter") => cx.emit(InputEvent::Submit),
            (_, "tab") => cx.emit(InputEvent::Tab { back: m.shift }),
            (_, "up") => cx.emit(InputEvent::Up),
            (_, "down") => cx.emit(InputEvent::Down),
            (false, _) => {
                let Some(typed) = ks.key_char.as_deref().filter(|s| !s.is_empty() && !s.chars().any(char::is_control)) else {
                    return;
                };
                self.insert(typed, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let me = cx.entity();
        let text = self.text.clone();
        let placeholder = self.placeholder.clone();
        let sel = self.selection();
        let head = self.head;
        let scroll = self.scroll;

        let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut gpui::App| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let empty = text.is_empty();
            let (shown, color) = if empty { (placeholder.to_string(), theme::muted_text()) } else { (text, theme::text()) };
            let run = TextRun {
                len: shown.len(),
                font: style.font(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window.text_system().shape_line(shown.into(), font_size, &[run], None);
            let (x0, w, h) = (f32::from(bounds.origin.x), f32::from(bounds.size.width), f32::from(bounds.size.height));
            let x_of = |ix: usize| if empty { 0.0 } else { f32::from(line.x_for_index(ix)) };

            // Scroll just enough to keep the cursor in view, and no further than the text.
            let hx = x_of(head);
            let mut scroll = scroll;
            if hx - scroll > w - 2.0 {
                scroll = hx - w + 2.0;
            }
            if hx < scroll {
                scroll = hx;
            }
            let max = if empty { 0.0 } else { (f32::from(line.width) - w + 2.0).max(0.0) };
            let scroll = scroll.clamp(0.0, max);
            let ox = x0 - scroll;
            let y = f32::from(bounds.origin.y);

            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if focused && !sel.is_empty() {
                    let (a, b) = (x_of(sel.start), x_of(sel.end));
                    window.paint_quad(fill(Bounds::new(point(px(ox + a), px(y + 2.0)), size(px(b - a), px(h - 4.0))), theme::selection()));
                }
                line.paint(point(px(ox), px(y)), px(h), window, cx).ok();
                if focused {
                    window.paint_quad(fill(Bounds::new(point(px(ox + hx), px(y + 3.0)), size(px(1.5), px(h - 6.0))), theme::cursor()));
                }
            });
            me.update(cx, |this, cx| {
                this.scroll = scroll;
                this.painted = (!empty).then_some((line, ox));
                if this.bounds != Some(bounds) {
                    this.bounds = Some(bounds);
                    cx.notify();
                }
            });
        };

        div()
            .id("text-input")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    let ix = this.index_at(ev.position.x);
                    this.move_to(ix, ev.modifiers.shift);
                    this.selecting = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.selecting && ev.pressed_button == Some(MouseButton::Left) {
                    let ix = this.index_at(ev.position.x);
                    if ix != this.head {
                        this.head = ix;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.selecting = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.selecting = false))
            .w_full()
            .h(px(26.0))
            .px_2()
            .rounded_sm()
            .border_1()
            .border_color(if focused { theme::accent() } else { theme::border() })
            .bg(theme::editor_bg())
            .cursor(CursorStyle::IBeam)
            .overflow_hidden()
            .child(canvas(|_, _, _| {}, paint).size_full())
    }
}

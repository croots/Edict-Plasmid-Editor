//! A terminal pane: the user's shell on a pty, emulated by libghostty-vt.
//!
//! libghostty-vt owns all terminal state (parsing, screen, scrollback, modes) and
//! encodes key presses. This view only moves bytes between it and the pty, and paints
//! the render state as a grid of cells in the editor's monospace font.
//!
//! A reader thread copies pty output into a channel; a foreground task feeds it to
//! the terminal, since libghostty handles are single-threaded.

use std::io::{Read, Write};
use std::path::PathBuf;

use ghostty_vt::key::{self, Key, Mods};
use ghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use ghostty_vt::screen::{CellWide, Screen};
use ghostty_vt::style::{Palette, RgbColor, Underline};
use ghostty_vt::terminal::{ColorScheme, ScrollViewport};
use ghostty_vt::{Event, RenderState, Terminal, TerminalOptions};
use gpui::{
    canvas, div, fill, point, prelude::*, px, size, App, Bounds, ClipboardItem, Context, FocusHandle, Focusable,
    FontStyle, FontWeight, Hsla, KeyDownEvent, MouseButton, Pixels, Rgba, ScrollWheelEvent, StrikethroughStyle, Task,
    TextRun, UnderlineStyle, Window,
};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::editor::editor_font;
use crate::theme;

/// Key context of the terminal, so app shortcuts that a shell needs (Ctrl+W, Ctrl+E,
/// Escape, ...) can be bound with `!Terminal`.
pub const KEY_CONTEXT: &str = "Terminal";

const PAD: f32 = 6.0;
const DEFAULT_FONT: f32 = 13.0;
const MIN_FONT: f32 = 8.0;
const MAX_FONT: f32 = 32.0;

/// The running shell.
struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the pane ends the shell; the reader thread then sees EOF.
        self.killer.kill().ok();
    }
}

pub struct TerminalView {
    term: Terminal,
    render: RenderState,
    rows: RowIterator,
    cells: CellIterator,
    keys: key::Encoder,
    key_event: key::Event,
    pty: Option<Pty>,
    cwd: PathBuf,
    focus: FocusHandle,
    /// Grid size in cells, as last given to the terminal and the pty.
    grid: (u16, u16),
    font_size: f32,
    /// Fractional wheel lines not yet scrolled.
    scroll_rem: f32,
    title: String,
    /// Why there is no shell: it exited, or could not start.
    status: Option<String>,
    /// The theme colors last handed to the terminal as its defaults.
    theme_colors: Option<ThemeColors>,
    /// libghostty's own 256 colors, kept for indexes 16–255 (the cube and greys).
    builtin_palette: Option<Palette>,
    /// Take keyboard focus on the first frame, so a new pane is ready to type in.
    focus_pending: bool,
    _reader: Option<Task<()>>,
}

impl TerminalView {
    pub fn new(cwd: PathBuf, cx: &mut Context<Self>) -> Self {
        let grid = (80, 24);
        let term = Terminal::new(TerminalOptions { cols: grid.0, rows: grid.1, ..Default::default() })
            .expect("libghostty-vt terminal");
        let mut this = Self {
            term,
            render: RenderState::new().expect("render state"),
            rows: RowIterator::new().expect("row iterator"),
            cells: CellIterator::new().expect("cell iterator"),
            keys: key::Encoder::new().expect("key encoder"),
            key_event: key::Event::new().expect("key event"),
            pty: None,
            cwd,
            focus: cx.focus_handle(),
            grid,
            font_size: DEFAULT_FONT,
            scroll_rem: 0.0,
            title: String::new(),
            status: None,
            theme_colors: None,
            builtin_palette: None,
            focus_pending: true,
            _reader: None,
        };
        this.start_shell(cx);
        this
    }

    /// The title the shell set (OSC 0/2), for the pane header.
    pub fn title(&self) -> &str {
        &self.title
    }

    fn start_shell(&mut self, cx: &mut Context<Self>) {
        match self.spawn_shell(cx) {
            Ok(()) => self.status = None,
            Err(e) => self.status = Some(format!("Could not start a shell: {e:#}")),
        }
        cx.notify();
    }

    fn spawn_shell(&mut self, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let (cols, rows) = self.grid;
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new_default_prog();
        if self.cwd.is_dir() {
            cmd.cwd(&self.cwd);
        }
        // Ghostty's own terminfo describes exactly what libghostty-vt understands.
        match ghostty_vt::terminfo::dir() {
            Ok(dir) => {
                cmd.env("TERMINFO", dir);
                cmd.env("TERM", "xterm-ghostty");
            }
            Err(_) => cmd.env("TERM", "xterm-256color"),
        }
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "plasmid-gpui");
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let (tx, rx) = async_channel::unbounded::<Vec<u8>>();
        std::thread::Builder::new().name("terminal-pty".into()).spawn(move || {
            let mut child = child;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send_blocking(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
            child.wait().ok();
        })?;

        self._reader = Some(cx.spawn(async move |this, cx| {
            while let Ok(mut bytes) = rx.recv().await {
                // Coalesce whatever else already arrived into one update.
                while let Ok(more) = rx.try_recv() {
                    bytes.extend_from_slice(&more);
                }
                if this.update(cx, |t, cx| t.feed(&bytes, cx)).is_err() {
                    return;
                }
            }
            this.update(cx, |t, cx| {
                t.pty = None;
                t.status = Some("Process exited · press Enter to restart".into());
                cx.notify();
            })
            .ok();
        }));
        self.pty = Some(Pty { master: pair.master, writer, killer });
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.term.vt_write(bytes);
        let events: Vec<Event> = self.term.events().collect();
        for event in events {
            match event {
                Event::PtyWrite(reply) => self.write(&reply),
                Event::TitleChanged(title) => self.title = title,
                Event::ClipboardWrite(w) => {
                    if let Some(text) = w.contents.iter().find(|c| c.mime.starts_with("text/plain")) {
                        cx.write_to_clipboard(ClipboardItem::new_string(String::from_utf8_lossy(&text.data).into_owned()));
                    }
                }
                _ => {}
            }
        }
        cx.notify();
    }

    fn write(&mut self, bytes: &[u8]) {
        if let Some(pty) = self.pty.as_mut() {
            if pty.writer.write_all(bytes).and_then(|_| pty.writer.flush()).is_err() {
                self.status = Some("Could not write to the shell".into());
            }
        }
    }

    /// Fit the grid to the pane.
    fn resize(&mut self, cols: u16, rows: u16, cell: (f32, f32)) {
        if (cols, rows) == self.grid {
            return;
        }
        self.grid = (cols, rows);
        self.term.resize(cols, rows, cell.0 as u32, cell.1 as u32).ok();
        if let Some(pty) = &self.pty {
            pty.master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: (cols as f32 * cell.0) as u16,
                    pixel_height: (rows as f32 * cell.1) as u16,
                })
                .ok();
        }
    }

    /// Hand the app theme to the terminal as its default colors, when it changed.
    fn sync_theme(&mut self) {
        let colors = ThemeColors {
            bg: theme::editor_bg(),
            fg: theme::text(),
            cursor: theme::cursor(),
            ansi: theme::terminal_palette(),
        };
        if self.theme_colors.as_ref() == Some(&colors) {
            return;
        }
        if self.builtin_palette.is_none() {
            self.builtin_palette = self.term.default_color_palette().ok();
        }
        if let Some(mut palette) = self.builtin_palette {
            for (slot, color) in palette.0.iter_mut().zip(colors.ansi) {
                *slot = to_rgb(color);
            }
            self.term.set_default_color_palette(Some(palette)).ok();
        }
        self.term.set_default_bg_color(Some(to_rgb(colors.bg))).ok();
        self.term.set_default_fg_color(Some(to_rgb(colors.fg))).ok();
        self.term.set_default_cursor_color(Some(to_rgb(colors.cursor))).ok();
        // Answers programs asking whether to draw for a light or dark background.
        self.term.set_color_scheme(Some(if colors.bg.l < 0.5 { ColorScheme::Dark } else { ColorScheme::Light }));
        self.theme_colors = Some(colors);
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return };
        let bracketed = self.term.mode(ghostty_vt::terminal::Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.into_bytes();
        let mut out = vec![0u8; data.len() + 16];
        let n = match ghostty_vt::paste::encode(&mut data, bracketed, &mut out) {
            Ok(n) => n,
            Err(ghostty_vt::Error::OutOfSpace { required }) => {
                out.resize(required, 0);
                ghostty_vt::paste::encode(&mut data, bracketed, &mut out).unwrap_or(0)
            }
            Err(_) => 0,
        };
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.write(&out[..n]);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        // Zoom: Ctrl+= or Ctrl++ larger, Ctrl+- smaller, Ctrl+0 back to the default size.
        if m.control && !m.alt && !m.platform {
            match ks.key.as_str() {
                "=" | "+" => return self.zoom(1.0, cx),
                "-" | "_" => return self.zoom(-1.0, cx),
                "0" if !m.shift => return self.zoom(DEFAULT_FONT - self.font_size, cx),
                _ => {}
            }
        }
        // Paste is on Ctrl+Shift so plain Ctrl+V reaches the shell.
        if m.control && m.shift && ks.key == "v" {
            return self.paste(cx);
        }
        if m.shift && !m.control && !m.alt {
            let page = self.grid.1.saturating_sub(1).max(1) as isize;
            match ks.key.as_str() {
                "pageup" => return self.scroll_lines(-page, cx),
                "pagedown" => return self.scroll_lines(page, cx),
                _ => {}
            }
        }
        if self.pty.is_none() {
            if ks.key == "enter" {
                self.term.reset();
                self.start_shell(cx);
            }
            return;
        }

        let key = map_key(&ks.key);
        let mut mods = Mods::empty();
        mods.set(Mods::SHIFT, m.shift);
        mods.set(Mods::CTRL, m.control);
        mods.set(Mods::ALT, m.alt);
        mods.set(Mods::SUPER, m.platform);
        // The text the key types, minus control characters (the encoder derives those).
        let text = ks.key_char.clone().filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
        let unshifted = ks.key.chars().next().filter(|_| ks.key.chars().count() == 1);
        let consumed = match (&text, unshifted) {
            (Some(t), Some(u)) if m.shift && !t.starts_with(u) => Mods::SHIFT,
            _ => Mods::empty(),
        };
        self.key_event
            .set_action(if ev.is_held { key::Action::Repeat } else { key::Action::Press })
            .set_key(key)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_utf8(text.clone());
        if let Some(u) = unshifted {
            self.key_event.set_unshifted_codepoint(u);
        }
        self.keys.set_options_from_terminal(&self.term);
        let mut out = Vec::new();
        self.keys.encode_to_vec(&self.key_event, &mut out).ok();
        if out.is_empty() && !m.control && !m.alt && !m.platform {
            // Keys the encoder has no code for still type their text.
            if let Some(t) = text {
                out = t.into_bytes();
            }
        }
        if out.is_empty() {
            return;
        }
        cx.stop_propagation();
        self.term.scroll_viewport(ScrollViewport::Bottom);
        self.write(&out);
        cx.notify();
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.font_size = (self.font_size + delta).clamp(MIN_FONT, MAX_FONT);
        cx.notify();
    }

    fn scroll_lines(&mut self, lines: isize, cx: &mut Context<Self>) {
        if lines == 0 {
            return;
        }
        if self.term.active_screen().ok() == Some(Screen::Alternate) {
            // Full-screen programs keep no scrollback: send arrow keys, as other terminals do.
            let seq: &[u8] = match (lines < 0, self.term.mode(ghostty_vt::terminal::Mode::DECCKM).unwrap_or(false)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            for _ in 0..lines.unsigned_abs().min(20) {
                self.write(seq);
            }
        } else {
            self.term.scroll_viewport(ScrollViewport::Delta(lines));
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, line_h: f32, cx: &mut Context<Self>) {
        let dy: f32 = ev.delta.pixel_delta(px(line_h)).y.into();
        self.scroll_rem -= dy / line_h;
        let whole = self.scroll_rem.trunc();
        self.scroll_rem -= whole;
        self.scroll_lines(whole as isize, cx);
    }

    fn frame(&mut self, focused: bool) -> Frame {
        capture(&self.term, &mut self.render, &mut self.rows, &mut self.cells, focused)
    }
}

/// Capture the visible screen as backgrounds, text runs and the cursor.
fn capture(term: &Terminal, render: &mut RenderState, rows: &mut RowIterator, cells: &mut CellIterator, focused: bool) -> Frame {
    let mut frame = Frame::default();
    let Ok(snapshot) = render.update(term) else { return frame };
    let colors = snapshot.colors().ok();
    let default_fg = colors.as_ref().map(|c| to_hsla(c.foreground)).unwrap_or_else(theme::text);
    let default_bg = colors.as_ref().map(|c| to_hsla(c.background)).unwrap_or_else(theme::editor_bg);
    frame.bg = default_bg;

    let Ok(mut row_iter) = rows.update(&snapshot) else { return frame };
    let mut y = 0u16;
    let mut text = String::new();
    while let Some(row) = row_iter.next() {
        let Ok(mut cell_iter) = cells.update(row) else { break };
        let mut runs: Vec<Run> = Vec::new();
        let mut x = 0u16;
        while let Some(cell) = cell_iter.next() {
            let col = x;
            x += 1;
            let wide = cell.raw_cell().and_then(|c| c.wide()).unwrap_or(CellWide::Narrow);
            if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            let style = cell.style().unwrap_or_default();
            let mut fg = cell.fg_color().ok().flatten().map(to_hsla).unwrap_or(default_fg);
            let mut bg = cell.bg_color().ok().flatten().map(to_hsla);
            if style.inverse {
                (fg, bg) = (bg.unwrap_or(default_bg), Some(fg));
            }
            if style.faint {
                fg = fg.opacity(0.6);
            }
            let span = if wide == CellWide::Wide { 2 } else { 1 };
            if let Some(bg) = bg {
                match frame.backgrounds.last_mut() {
                    Some(b) if b.row == y && b.end == col && b.color == bg => b.end += span,
                    _ => frame.backgrounds.push(Background { row: y, start: col, end: col + span, color: bg }),
                }
            }
            text.clear();
            cell.graphemes_utf8(&mut text).ok();
            if text.is_empty() || style.invisible {
                text.clear();
                text.push(' ');
            }
            let look = Look {
                fg,
                bold: style.bold,
                italic: style.italic,
                underline: style.underline != Underline::None,
                curly: style.underline == Underline::Curly,
                strike: style.strikethrough,
            };
            // Only plain ASCII shares a run, so every glyph outside it starts on its own
            // cell boundary whatever width the font gives it.
            let ascii = text.is_ascii() && span == 1;
            match runs.last_mut() {
                Some(r) if ascii && r.ascii && r.look == look && r.col + r.cells == col => {
                    r.text.push_str(&text);
                    r.cells += 1;
                }
                _ => runs.push(Run { row: y, col, cells: span, text: text.clone(), look, ascii }),
            }
        }
        // Trailing blanks paint nothing, unless underlined or struck through.
        for r in &mut runs {
            if r.ascii && !r.look.underline && !r.look.strike {
                let kept = r.text.trim_end_matches(' ').len();
                r.cells -= (r.text.len() - kept) as u16;
                r.text.truncate(kept);
            }
        }
        runs.retain(|r| !(r.text.trim().is_empty() && !r.look.underline && !r.look.strike));
        frame.runs.extend(runs);
        y += 1;
    }

    if let Ok(cursor) = snapshot.cursor() {
        if let (true, Some(vp)) = (cursor.visible, cursor.viewport) {
            let color = colors.as_ref().and_then(|c| c.cursor).map(to_hsla).unwrap_or_else(theme::cursor);
            let shape = match cursor.visual_style {
                _ if !focused => CursorShape::Hollow,
                CursorVisualStyle::Bar => CursorShape::Bar,
                CursorVisualStyle::Underline => CursorShape::Underline,
                CursorVisualStyle::BlockHollow => CursorShape::Hollow,
                _ => CursorShape::Block,
            };
            frame.cursor = Some((vp.x, vp.y, shape, color));
        }
    }
    snapshot.clean().ok();
    frame
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The app theme colors last handed to the terminal.
#[derive(PartialEq)]
struct ThemeColors {
    bg: Hsla,
    fg: Hsla,
    cursor: Hsla,
    ansi: [Hsla; 16],
}

/// How a run of cells is drawn.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    curly: bool,
    strike: bool,
}

struct Run {
    row: u16,
    col: u16,
    cells: u16,
    text: String,
    look: Look,
    ascii: bool,
}

struct Background {
    row: u16,
    start: u16,
    end: u16,
    color: Hsla,
}

#[derive(Clone, Copy)]
enum CursorShape {
    Block,
    Hollow,
    Bar,
    Underline,
}

#[derive(Default)]
struct Frame {
    bg: Hsla,
    backgrounds: Vec<Background>,
    runs: Vec<Run>,
    cursor: Option<(u16, u16, CursorShape, Hsla)>,
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_theme();
        if std::mem::take(&mut self.focus_pending) {
            window.focus(&self.focus);
        }
        let font = editor_font();
        let font_size = self.font_size;
        let ts = window.text_system();
        let font_id = ts.resolve_font(&font);
        let char_w: f32 = ts.advance(font_id, px(font_size), 'M').map(|s| s.width.into()).unwrap_or(font_size * 0.6);
        let line_h = (font_size * 1.3).round();
        let focused = self.focus.is_focused(window);
        let frame = self.frame(focused);
        let bg = frame.bg;
        let status = self.status.clone();
        let me = cx.entity();

        let grid = canvas(
            move |bounds, window, cx| {
                let cols = ((f32::from(bounds.size.width) - 2.0 * PAD) / char_w).floor().max(2.0) as u16;
                let rows = ((f32::from(bounds.size.height) - 2.0 * PAD) / line_h).floor().max(1.0) as u16;
                let changed = me.update(cx, |this, _| {
                    let changed = this.grid != (cols, rows);
                    this.resize(cols, rows, (char_w, line_h));
                    changed
                });
                if changed {
                    window.on_next_frame(move |_, cx| me.update(cx, |_, cx| cx.notify()));
                }
            },
            move |bounds, _, window, cx| paint_frame(&frame, bounds, &font, font_size, char_w, line_h, window, cx),
        )
        .size_full();

        div()
            .id("terminal")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(bg)
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus);
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| this.on_scroll(ev, line_h, cx)))
            .child(grid)
            .children(status.map(|s| {
                div()
                    .absolute()
                    .bottom(px(8.0))
                    .right(px(8.0))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme::panel())
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::muted_text())
                    .child(s)
            }))
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_frame(
    frame: &Frame,
    bounds: Bounds<Pixels>,
    font: &gpui::Font,
    font_size: f32,
    char_w: f32,
    line_h: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let ox = f32::from(bounds.origin.x) + PAD;
    let oy = f32::from(bounds.origin.y) + PAD;
    let cell = |col: u16, row: u16| point(px(ox + col as f32 * char_w), px(oy + row as f32 * line_h));
    for b in &frame.backgrounds {
        let w = (b.end - b.start) as f32 * char_w;
        window.paint_quad(fill(Bounds::new(cell(b.start, b.row), size(px(w), px(line_h))), b.color));
    }
    // A block cursor is filled under the text, which then takes the background color.
    let block = frame.cursor.filter(|c| matches!(c.2, CursorShape::Block));
    if let Some((x, y, _, color)) = block {
        window.paint_quad(fill(Bounds::new(cell(x, y), size(px(char_w), px(line_h))), color));
    }
    for r in &frame.runs {
        let under_cursor = block.is_some_and(|(x, y, _, _)| y == r.row && x >= r.col && x < r.col + r.cells);
        let mut f = font.clone();
        if r.look.bold {
            f.weight = FontWeight::BOLD;
        }
        if r.look.italic {
            f.style = FontStyle::Italic;
        }
        let run = |len: usize, color: Hsla| TextRun {
            len,
            font: f.clone(),
            color,
            background_color: None,
            underline: r.look.underline.then_some(UnderlineStyle { thickness: px(1.0), color: Some(color), wavy: r.look.curly }),
            strikethrough: r.look.strike.then_some(StrikethroughStyle { thickness: px(1.0), color: Some(color) }),
        };
        let runs = if let (true, Some((x, _, _, _))) = (under_cursor, block) {
            // Split the run so the character under the block cursor stays readable.
            let at = (x - r.col) as usize;
            let start = r.text.char_indices().nth(at).map(|(i, _)| i).unwrap_or(r.text.len());
            let end = r.text[start..].chars().next().map(|c| start + c.len_utf8()).unwrap_or(start);
            [run(start, r.look.fg), run(end - start, frame.bg), run(r.text.len() - end, r.look.fg)]
                .into_iter()
                .filter(|t| t.len > 0)
                .collect::<Vec<_>>()
        } else {
            vec![run(r.text.len(), r.look.fg)]
        };
        let shaped = window.text_system().shape_line(r.text.clone().into(), px(font_size), &runs, None);
        shaped.paint(cell(r.col, r.row), px(line_h), window, cx).ok();
    }
    if let Some((x, y, shape, color)) = frame.cursor {
        let at = cell(x, y);
        match shape {
            CursorShape::Block => {}
            CursorShape::Bar => window.paint_quad(fill(Bounds::new(at, size(px(2.0), px(line_h))), color)),
            CursorShape::Underline => {
                let at = point(at.x, at.y + px(line_h - 2.0));
                window.paint_quad(fill(Bounds::new(at, size(px(char_w), px(2.0))), color));
            }
            CursorShape::Hollow => window.paint_quad(
                gpui::outline(Bounds::new(at, size(px(char_w), px(line_h))), color, gpui::BorderStyle::Solid),
            ),
        }
    }
}

fn to_rgb(c: Hsla) -> RgbColor {
    let c = Rgba::from(c);
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    RgbColor { r: b(c.r), g: b(c.g), b: b(c.b) }
}

fn to_hsla(c: RgbColor) -> Hsla {
    gpui::rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}

/// GPUI's key name to the physical key libghostty encodes.
fn map_key(name: &str) -> Key {
    let named = match name {
        "enter" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "escape" => Some(Key::Escape),
        "space" | " " => Some(Key::Space),
        "delete" => Some(Key::Delete),
        "insert" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "-" => Some(Key::Minus),
        "=" => Some(Key::Equal),
        "[" => Some(Key::BracketLeft),
        "]" => Some(Key::BracketRight),
        "\\" => Some(Key::Backslash),
        ";" => Some(Key::Semicolon),
        "'" => Some(Key::Quote),
        "," => Some(Key::Comma),
        "." => Some(Key::Period),
        "/" => Some(Key::Slash),
        "`" => Some(Key::Backquote),
        _ => None,
    };
    if let Some(k) = named {
        return k;
    }
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        const LETTERS: [Key; 26] = [
            Key::A, Key::B, Key::C, Key::D, Key::E, Key::F, Key::G, Key::H, Key::I, Key::J, Key::K, Key::L, Key::M,
            Key::N, Key::O, Key::P, Key::Q, Key::R, Key::S, Key::T, Key::U, Key::V, Key::W, Key::X, Key::Y, Key::Z,
        ];
        const DIGITS: [Key; 10] = [
            Key::Digit0, Key::Digit1, Key::Digit2, Key::Digit3, Key::Digit4,
            Key::Digit5, Key::Digit6, Key::Digit7, Key::Digit8, Key::Digit9,
        ];
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() {
            return LETTERS[(c as u8 - b'a') as usize];
        }
        if c.is_ascii_digit() {
            return DIGITS[(c as u8 - b'0') as usize];
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<usize>().ok()) {
        const F: [Key; 12] =
            [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
        if (1..=12).contains(&n) {
            return F[n - 1];
        }
    }
    Key::Unidentified
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(term: &Terminal) -> (Vec<String>, Frame) {
        let (mut rs, mut rows, mut cells) = (RenderState::new().unwrap(), RowIterator::new().unwrap(), CellIterator::new().unwrap());
        let frame = capture(term, &mut rs, &mut rows, &mut cells, true);
        let mut lines = vec![String::new(); term.rows().unwrap_or(0) as usize];
        for r in &frame.runs {
            let line = &mut lines[r.row as usize];
            while line.chars().count() < r.col as usize {
                line.push(' ');
            }
            line.push_str(&r.text);
        }
        (lines, frame)
    }

    fn term(cols: u16, rows: u16) -> Terminal {
        Terminal::new(TerminalOptions { cols, rows, ..Default::default() }).unwrap()
    }

    #[test]
    fn captures_text_styles_and_cursor() {
        let mut t = term(20, 4);
        t.vt_write(b"plain \x1b[1;31mbold red\x1b[0m\r\n\x1b[44m  \x1b[0mx");
        let (lines, frame) = screen(&t);
        assert_eq!(lines[0], "plain bold red");
        assert_eq!(lines[1].trim_start(), "x");
        let red = frame.runs.iter().find(|r| r.text == "bold red").expect("styled run");
        assert!(red.look.bold);
        assert_eq!(red.col, 6);
        let bg = frame.backgrounds.iter().find(|b| b.row == 1).expect("background");
        assert_eq!((bg.start, bg.end), (0, 2));
        let (x, y, _, _) = frame.cursor.expect("cursor");
        assert_eq!((x, y), (3, 1));
    }

    #[test]
    fn ansi_colors_follow_the_palette() {
        let mut t = term(10, 2);
        t.vt_write(b"\x1b[31mr");
        let mut palette = t.default_color_palette().unwrap();
        palette.0[1] = RgbColor { r: 0x12, g: 0x34, b: 0x56 };
        // Set after the text was written, as when the user switches themes.
        t.set_default_color_palette(Some(palette)).unwrap();
        let (_, frame) = screen(&t);
        let r = frame.runs.iter().find(|r| r.text == "r").unwrap();
        assert_eq!(to_rgb(r.look.fg), RgbColor { r: 0x12, g: 0x34, b: 0x56 });
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut t = term(10, 2);
        t.vt_write("a漢b".as_bytes());
        let (_, frame) = screen(&t);
        let wide = frame.runs.iter().find(|r| r.text == "漢").expect("wide run");
        assert_eq!((wide.col, wide.cells), (1, 2));
        let b = frame.runs.iter().find(|r| r.text == "b").expect("after wide");
        assert_eq!(b.col, 3);
    }

    #[test]
    fn keys_encode_for_the_shell() {
        let t = term(10, 2);
        let mut enc = key::Encoder::new().unwrap();
        enc.set_options_from_terminal(&t);
        let mut ev = key::Event::new().unwrap();
        let mut out = Vec::new();
        ev.set_action(key::Action::Press).set_key(map_key("c")).set_mods(Mods::CTRL).set_utf8(None::<String>);
        ev.set_unshifted_codepoint('c');
        enc.encode_to_vec(&ev, &mut out).unwrap();
        assert_eq!(out, b"\x03");
        out.clear();
        ev.set_key(map_key("up")).set_mods(Mods::empty());
        enc.encode_to_vec(&ev, &mut out).unwrap();
        assert_eq!(out, b"\x1b[A");
    }

    #[test]
    fn shell_output_reaches_the_screen() {
        let pair = native_pty_system().openpty(PtySize { rows: 5, cols: 40, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", "printf 'hello \\033[32mpty\\033[0m'"]);
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
            if bytes.windows(3).any(|w| w == b"pty") {
                break;
            }
        }
        child.wait().unwrap();
        let mut t = term(40, 5);
        t.vt_write(&bytes);
        let (lines, _) = screen(&t);
        assert!(lines[0].starts_with("hello pty"), "{lines:?}");
    }
}

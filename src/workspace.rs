//! IDE shell: file tree on the left, tab strip on top, the active editor below it,
//! plus the hamburger menu and the settings panel drawn as overlays.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{
    actions, div, prelude::*, px, uniform_list, App, Context, Entity, FocusHandle, Hsla, KeyBinding, MouseButton,
    PathPromptOptions, PromptLevel, ScrollStrategy, SharedString, UniformListScrollHandle, Window,
};

use crate::about::{self, AboutPanel};
use crate::editor::SeqEditor;
use crate::hotkeys;
use crate::panes::{self, PaneGroup};
use crate::model::{self, Document};
use crate::settings::{self, Settings};
use crate::tasks::{self, TaskStatus};
use crate::text_input::{InputEvent, TextInput};
use crate::theme::{self, ThemeEntry, ThemeSource};

actions!(plasmid, [NewFile, OpenQueued, OpenFile, OpenFolder, Save, SaveAs, ExportAs, CloseTab, OpenSettings, About, Dismiss, Quit, ToggleFullScreen]);

/// A document a pane wants opened in a new tab.
pub struct NewDocument {
    /// A file-name stem; the tab is untitled until saved.
    pub stem: String,
    pub seq: Vec<u8>,
    pub features: Vec<crate::model::Feature>,
    pub circular: bool,
}

thread_local! {
    static QUEUED: std::cell::RefCell<Vec<NewDocument>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Open `doc` in a new tab, once the workspace sees the action.
pub fn queue_new_document(doc: NewDocument, window: &mut Window, cx: &mut App) {
    QUEUED.with(|q| q.borrow_mut().push(doc));
    window.dispatch_action(Box::new(OpenQueued), cx);
}

/// Bindings handled by the workspace. Ctrl+S / Ctrl+Shift+S stay with the editor's
/// own key handler; the menu shows them for discoverability.
///
/// Keys a shell needs (Ctrl+W, Ctrl+E, Ctrl+\, Escape, ...) go to a focused terminal
/// instead; the Ctrl+Shift ones stay global.
pub fn bind_keys(cx: &mut App) {
    // On macOS these are Cmd keys, which a shell never sees.
    let app = if cfg!(target_os = "macos") { None } else { Some("!Terminal") };
    cx.bind_keys([
        KeyBinding::new("secondary-n", NewFile, app),
        KeyBinding::new("secondary-o", OpenFile, app),
        KeyBinding::new("secondary-shift-o", OpenFolder, None),
        KeyBinding::new("secondary-e", ExportAs, app),
        KeyBinding::new("secondary-w", CloseTab, app),
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-q", Quit, app),
        KeyBinding::new("escape", Dismiss, Some("!Terminal")),
        KeyBinding::new("secondary-\\", panes::SplitRight, app),
        KeyBinding::new("secondary-shift-\\", panes::SplitDown, None),
        KeyBinding::new("secondary-|", panes::SplitDown, None),
        KeyBinding::new("secondary-shift-w", panes::ClosePane, None),
        KeyBinding::new("f11", ToggleFullScreen, None),
    ]);
    if cfg!(target_os = "macos") {
        cx.bind_keys([KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None)]);
    }
}

const SCHEMES_URL: &str = "https://github.com/tinted-theming/schemes";

enum TabContent {
    Editor(Entity<SeqEditor>, Entity<PaneGroup>),
    Failed(SharedString),
}

struct Tab {
    /// Where the tab was opened from; an editor tab follows its document instead (Save As).
    path: PathBuf,
    content: TabContent,
}

impl Tab {
    fn path(&self, cx: &App) -> PathBuf {
        match &self.content {
            TabContent::Editor(ed, _) => ed.read(cx).doc.path.clone(),
            TabContent::Failed(_) => self.path.clone(),
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        self.path(cx).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into()
    }
}

struct TreeEntry {
    path: PathBuf,
    depth: usize,
    is_dir: bool,
}

/// The pages of the Settings panel.
#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    Appearance,
    Hotkeys,
    Keys,
}

#[derive(Clone, Copy, PartialEq)]
enum ThemeFilter {
    All,
    Dark,
    Light,
    Custom,
}

pub struct Workspace {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    focus: FocusHandle,
    untitled_count: usize,
    menu_open: bool,
    settings_open: bool,
    settings_page: SettingsPage,
    /// The About panel, while it is open.
    about: Option<Entity<AboutPanel>>,
    settings: Settings,
    /// Built-in and bundled themes, then those loaded from `settings.custom_theme_paths`.
    themes: Vec<ThemeEntry>,
    custom_errors: Vec<String>,
    theme_filter: ThemeFilter,
    theme_scroll: UniformListScrollHandle,
    /// A workspace-level problem (file dialog failed, settings not saved).
    notice: Option<SharedString>,
    /// Settings → Keys: the address sent to NCBI with BLAST searches.
    email_input: Entity<TextInput>,
    /// The task last shown in front, to notice when another comes forward.
    shown_task: Option<tasks::TaskId>,
    _tasks: gpui::Subscription,
}

impl Workspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.clone());
        let focus = cx.focus_handle();
        window.focus(&focus);
        let settings = Settings::load();
        let email_input = cx.new(|cx| TextInput::new(settings.email.clone().unwrap_or_default(), "you@example.org", cx));
        cx.subscribe(&email_input, |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Changed) {
                let text = input.read(cx).text().trim().to_string();
                this.settings.email = (!text.is_empty()).then_some(text);
                this.persist(cx);
            }
        })
        .detach();
        // A task that finishes while the window is behind others gets a desktop notification.
        cx.subscribe_in(&tasks::manager(cx), window, |_this: &mut Self, m, ev: &tasks::TaskEvent, window, cx| {
            let tasks::TaskEvent::Finished(id) = *ev;
            if window.is_window_active() {
                return;
            }
            let Some(t) = m.read(cx).task(id) else { return };
            let (summary, body) = match &t.status {
                TaskStatus::Failed(e) => (format!("{} failed", t.kind), format!("{}: {e}", t.title)),
                _ => {
                    let found = t.output.clone().and_then(|o| o.downcast::<crate::blast::BlastResults>().ok()).map(|r| match r.hits.len() {
                        0 => "no matches".to_string(),
                        1 => "1 match".to_string(),
                        n => format!("{n} matches"),
                    });
                    let body = match found {
                        Some(f) => format!("{}: {f}", t.title),
                        None => t.title.clone(),
                    };
                    (format!("{} finished", t.kind), body)
                }
            };
            crate::notify::send(summary, body);
        })
        .detach();
        let _tasks = cx.observe_in(&tasks::manager(cx), window, |this: &mut Self, m, window, cx| {
            let front = m.read(cx).foreground;
            if front != this.shown_task {
                this.shown_task = front;
                match front.and_then(|id| m.read(cx).task(id).map(|t| t.editor.clone())) {
                    // A task came forward: show its file, and take focus off the editor behind it.
                    Some(editor) => {
                        if let Some(ix) = this.tabs.iter().position(|t| matches!(&t.content, TabContent::Editor(ed, _) if ed.downgrade() == editor)) {
                            this.active = Some(ix);
                        }
                        window.focus(&this.focus);
                    }
                    None => this.refocus_editor(window, cx),
                }
            }
            cx.notify();
        });
        let mut ws = Self {
            root,
            expanded,
            tabs: Vec::new(),
            active: None,
            focus,
            untitled_count: 0,
            menu_open: false,
            settings_open: false,
            settings_page: SettingsPage::Appearance,
            about: None,
            settings,
            themes: Vec::new(),
            custom_errors: Vec::new(),
            theme_filter: ThemeFilter::All,
            theme_scroll: UniformListScrollHandle::new(),
            notice: None,
            email_input,
            shown_task: None,
            _tasks,
        };
        crate::model::set_author(ws.settings.effective_author());
        settings::set_shared(ws.settings.clone());
        ws.reload_themes();
        if let Some(entry) = ws.themes.iter().find(|t| t.id == ws.settings.theme) {
            theme::set(entry.theme());
        }
        ws.update_title(window);
        ws
    }

    // ----- files -------------------------------------------------------------------

    /// The visible rows of the tree: directories (always) and sequence files, sorted
    /// directories-first, children listed only under expanded directories.
    fn visible_entries(&self) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        self.walk(&self.root, 0, &mut out);
        out
    }

    fn walk(&self, dir: &Path, depth: usize, out: &mut Vec<TreeEntry>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<(bool, PathBuf)> = read
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| !p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(true))
            .filter(|p| !matches!(p.file_name().and_then(|n| n.to_str()), Some("target" | "node_modules")))
            .map(|p| (p.is_dir(), p))
            .filter(|(is_dir, p)| *is_dir || model::is_sequence_file(p))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.file_name().cmp(&b.1.file_name())));
        for (is_dir, path) in entries {
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(TreeEntry { path: path.clone(), depth, is_dir });
            if expanded {
                self.walk(&path, depth + 1, out);
            }
        }
    }

    pub fn open(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| t.path(cx) == path) {
            self.activate(ix, window, cx);
            return;
        }
        let content = match Document::open(path) {
            Ok(doc) => self.new_editor(doc, cx),
            Err(e) => TabContent::Failed(format!("Could not open {}: {e:#}", path.display()).into()),
        };
        self.tabs.push(Tab { path: path.to_path_buf(), content });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn new_editor(&mut self, doc: Document, cx: &mut Context<Self>) -> TabContent {
        let editor = cx.new(|cx| SeqEditor::new(doc, cx));
        cx.observe(&editor, |_, _, cx| cx.notify()).detach(); // keep the tab's title and dirty dot current
        let panes = cx.new(|cx| PaneGroup::new(editor.clone(), cx));
        TabContent::Editor(editor, panes)
    }

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.untitled_path("untitled", cx);
        let content = self.new_editor(Document::new_untitled(&path), cx);
        self.tabs.push(Tab { path, content });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    /// A path in the folder that's neither on disk nor open: `<stem>.pgx`, or
    /// `<stem>-2.pgx` and so on.
    fn untitled_path(&mut self, stem: &str, cx: &App) -> PathBuf {
        let free = |p: &Path| !p.exists() && !self.tabs.iter().any(|t| t.path(cx) == p);
        if stem != "untitled" {
            for i in 1.. {
                let name = if i == 1 { format!("{stem}.{}", crate::pgx::EXTENSION) } else { format!("{stem}-{i}.{}", crate::pgx::EXTENSION) };
                let p = self.root.join(name);
                if free(&p) {
                    return p;
                }
            }
        }
        loop {
            self.untitled_count += 1;
            let p = self.root.join(format!("untitled-{}.{}", self.untitled_count, crate::pgx::EXTENSION));
            if free(&p) {
                return p;
            }
        }
    }

    /// Open the documents [`queue_new_document`] queued, each in a new untitled tab.
    fn open_queued(&mut self, _: &OpenQueued, window: &mut Window, cx: &mut Context<Self>) {
        for q in QUEUED.with(|q| std::mem::take(&mut *q.borrow_mut())) {
            let path = self.untitled_path(&q.stem, cx);
            let doc = Document::from_parts(&path, &q.seq, q.features, q.circular);
            let content = self.new_editor(doc, cx);
            self.tabs.push(Tab { path, content });
            self.activate(self.tabs.len() - 1, window, cx);
        }
    }

    fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = picked.await;
            this.update_in(cx, |ws, window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    for p in paths {
                        ws.open(&p, window, cx);
                    }
                }
                Ok(Err(e)) => ws.set_notice(format!("Could not open the file dialog: {e:#}"), cx),
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = picked.await;
            this.update_in(cx, |ws, window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(dir) = paths.into_iter().next() {
                        ws.set_root(dir, window, cx);
                    }
                }
                Ok(Err(e)) => ws.set_notice(format!("Could not open the folder dialog: {e:#}"), cx),
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn set_root(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let dir = dir.canonicalize().unwrap_or(dir);
        self.expanded.clear();
        self.expanded.insert(dir.clone());
        self.root = dir;
        self.update_title(window);
        cx.notify();
    }

    fn update_title(&self, window: &mut Window) {
        let name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.root.display().to_string());
        window.set_window_title(&format!("{name} — Plasmid Editor"));
    }

    fn active_editor(&self) -> Option<Entity<SeqEditor>> {
        match self.active.map(|a| &self.tabs[a].content) {
            Some(TabContent::Editor(ed, _)) => Some(ed.clone()),
            _ => None,
        }
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = self.active_editor() {
            ed.update(cx, |ed, cx| {
                ed.save(window, cx);
                cx.notify();
            });
        }
    }

    fn save_as(&mut self, _: &SaveAs, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = self.active_editor() {
            ed.update(cx, |ed, cx| {
                ed.save_as(cx);
                cx.notify();
            });
        }
    }

    fn export_as(&mut self, _: &ExportAs, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = self.active_editor() {
            ed.update(cx, |ed, cx| {
                ed.export_as(cx);
                cx.notify();
            });
        }
    }

    fn close_active(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.active {
            self.close(a, window, cx);
        }
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        let unsaved = self
            .tabs
            .iter()
            .filter(|t| matches!(&t.content, TabContent::Editor(ed, _) if ed.read(cx).doc.dirty))
            .count();
        let (running, finished) = {
            let m = tasks::manager(cx);
            let m = m.read(cx);
            let real = m.tasks().iter().filter(|t| t.status != TaskStatus::Draft);
            (real.clone().filter(|t| t.status.active()).count(), real.filter(|t| t.status.finished()).count())
        };
        if unsaved == 0 && running == 0 && finished == 0 {
            cx.quit();
            return;
        }
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = Vec::new();
        if unsaved > 0 {
            parts.push(plural(unsaved, "file has unsaved changes", "files have unsaved changes"));
        }
        if running > 0 {
            parts.push(plural(running, "task is still running", "tasks are still running"));
        }
        if finished > 0 {
            parts.push(plural(finished, "task has results that will be lost", "tasks have results that will be lost"));
        }
        let answer = window.prompt(PromptLevel::Warning, &parts.join("; "), Some("Quit anyway?"), &["Quit", "Cancel"], cx);
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|cx| cx.quit()).ok();
            }
        })
        .detach();
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(ix);
        if let TabContent::Editor(ed, _) = &self.tabs[ix].content {
            window.focus(ed.read(cx).focus_handle());
        }
        cx.notify();
    }

    /// Close a tab, asking first if tasks are running on or holding results for its file.
    fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(TabContent::Editor(ed, _)) = self.tabs.get(ix).map(|t| &t.content) else {
            return self.close_now(ix, window, cx);
        };
        let weak = ed.downgrade();
        let busy = tasks::manager(cx).read(cx).for_editor(&weak).len();
        if busy == 0 {
            return self.close_now(ix, window, cx);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("{busy} task{} use this file", if busy == 1 { "" } else { "s" }),
            Some("Closing it cancels them and throws away their results."),
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                this.update_in(cx, |this, window, cx| {
                    if let Some(ix) = this.tabs.iter().position(|t| matches!(&t.content, TabContent::Editor(e, _) if e.downgrade() == weak)) {
                        this.close_now(ix, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn close_now(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let TabContent::Editor(ed, _) = &self.tabs[ix].content {
            let weak = ed.downgrade();
            let manager = tasks::manager(cx);
            let ids: Vec<_> = manager.read(cx).tasks().iter().filter(|t| t.editor == weak).map(|t| t.id).collect();
            manager.update(cx, |m, cx| ids.into_iter().for_each(|id| m.dismiss(id, cx)));
        }
        self.tabs.remove(ix);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > ix => Some(a - 1),
            Some(a) => Some(a.min(self.tabs.len() - 1)),
            None => None,
        };
        match self.active {
            Some(a) => self.activate(a, window, cx),
            None => window.focus(&self.focus),
        }
        cx.notify();
    }

    fn set_notice(&mut self, msg: String, cx: &mut Context<Self>) {
        self.notice = Some(msg.into());
        cx.notify();
    }

    // ----- overlays ----------------------------------------------------------------

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = !self.menu_open;
        if self.menu_open {
            window.focus(&self.focus); // so Escape reaches us and typing doesn't edit behind the menu
        } else {
            self.refocus_editor(window, cx);
        }
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        self.settings_open = true;
        window.focus(&self.focus);
        self.scroll_to_current_theme();
        cx.notify();
    }

    fn toggle_fullscreen(&mut self, _: &ToggleFullScreen, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        window.toggle_fullscreen();
        cx.notify();
    }

    fn open_about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_open = false;
        self.settings_open = false;
        let panel = cx.new(|_| AboutPanel::new());
        cx.subscribe_in(&panel, window, |this, _, _: &about::Close, window, cx| this.dismiss(&Dismiss, window, cx)).detach();
        self.about = Some(panel);
        window.focus(&self.focus);
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        let manager = tasks::manager(cx);
        if let Some(id) = manager.read(cx).foreground {
            // Escape sends a task to the background, except a dialog not yet run,
            // whose options would be lost.
            if manager.read(cx).task(id).is_some_and(|t| t.status != TaskStatus::Draft) {
                manager.update(cx, |m, cx| m.background(cx));
            }
            return;
        }
        if self.menu_open || self.settings_open || self.about.is_some() {
            self.menu_open = false;
            self.settings_open = false;
            self.about = None;
            self.refocus_editor(window, cx);
        }
        self.notice = None;
        cx.notify();
    }

    fn refocus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(ed) => window.focus(ed.read(cx).focus_handle()),
            None => window.focus(&self.focus),
        }
    }

    // ----- themes ------------------------------------------------------------------

    fn reload_themes(&mut self) {
        self.themes = theme::bundled_themes();
        self.custom_errors.clear();
        for path in &self.settings.custom_theme_paths {
            let (entries, errors) = theme::load_custom(path);
            if entries.is_empty() && errors.is_empty() {
                self.custom_errors.push(format!("{}: no .yaml schemes found", path.display()));
            }
            self.themes.extend(entries);
            self.custom_errors.extend(errors);
        }
    }

    fn filtered_themes(&self) -> Vec<usize> {
        (0..self.themes.len())
            .filter(|&i| {
                let t = &self.themes[i];
                match self.theme_filter {
                    ThemeFilter::All => true,
                    ThemeFilter::Dark => t.dark,
                    ThemeFilter::Light => !t.dark,
                    ThemeFilter::Custom => matches!(t.source, ThemeSource::Custom(_)),
                }
            })
            .collect()
    }

    fn scroll_to_current_theme(&self) {
        if let Some(ix) = self.filtered_themes().iter().position(|&i| self.themes[i].id == self.settings.theme) {
            self.theme_scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    fn apply_theme(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let entry = &self.themes[ix];
        theme::set(entry.theme());
        self.settings.theme = entry.id.clone();
        self.persist(cx);
        window.refresh();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        // The BLAST dialog and the Restriction pane save through the shared copy; keep them.
        let shared = settings::shared();
        self.settings.blast = shared.blast;
        self.settings.favorite_enzymes = shared.favorite_enzymes;
        settings::set_shared(self.settings.clone());
        if let Err(e) = self.settings.save() {
            self.set_notice(format!("Settings not saved: {e:#}"), cx);
        }
        cx.notify();
    }

    fn add_custom_theme(&mut self, directories: bool, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: !directories,
            prompt: Some(if directories { "Add Theme Folder" } else { "Add Theme" }.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = picked.await;
            this.update_in(cx, |ws, window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    let before = ws.themes.len();
                    for p in paths {
                        if !ws.settings.custom_theme_paths.contains(&p) {
                            ws.settings.custom_theme_paths.push(p);
                        }
                    }
                    ws.reload_themes();
                    ws.theme_filter = ThemeFilter::Custom;
                    // Adding a single scheme most likely means "use this one".
                    if ws.themes.len() == before + 1 {
                        ws.apply_theme(ws.themes.len() - 1, window, cx);
                    }
                    ws.persist(cx);
                }
                Ok(Err(e)) => ws.set_notice(format!("Could not open the file dialog: {e:#}"), cx),
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn remove_custom_path(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.custom_theme_paths.remove(ix);
        self.reload_themes();
        // Fall back to the default if the active theme came from the removed path.
        if !self.themes.iter().any(|t| t.id == self.settings.theme) {
            self.apply_theme(0, window, cx);
        }
        self.persist(cx);
    }

    // ----- rendering ---------------------------------------------------------------

    fn hamburger(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = || div().w(px(14.0)).h(px(2.0)).rounded_sm().bg(theme::muted_text());
        div()
            .id("hamburger")
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap(px(3.0))
            .size(px(26.0))
            .rounded_md()
            .cursor_pointer()
            .when(self.menu_open, |d| d.bg(theme::active()))
            .hover(|s| s.bg(theme::hover()))
            .child(bar())
            .child(bar())
            .child(bar())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_menu(window, cx)))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.visible_entries();
        let active_path = self.active.map(|a| self.tabs[a].path(cx));
        let root_name = self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.root.display().to_string());
        let expanded = self.expanded.clone();
        let count = entries.len();

        div()
            .flex()
            .flex_col()
            .w(px(260.0))
            .h_full()
            .bg(theme::panel())
            .border_r_1()
            .border_color(theme::border())
            .child(
                div()
                    .h(px(36.0))
                    .pl_1()
                    .pr_3()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(self.hamburger(cx))
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::muted_text())
                            .child(format!("EXPLORER · {}", root_name.to_uppercase())),
                    ),
            )
            .child(
                uniform_list(
                    "file-tree",
                    count,
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|ix| {
                                let e = &entries[ix];
                                let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                let icon = if e.is_dir { if expanded.contains(&e.path) { "▾" } else { "▸" } } else { "≋" };
                                let is_active = active_path.as_deref() == Some(e.path.as_path());
                                let path = e.path.clone();
                                let is_dir = e.is_dir;
                                div()
                                    .id(ix)
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .h(px(24.0))
                                    .pl(px(10.0 + e.depth as f32 * 14.0))
                                    .pr_2()
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(is_active, |d| d.bg(theme::active()).text_color(theme::accent()))
                                    .hover(|s| s.bg(theme::hover()))
                                    .child(div().w(px(14.0)).text_color(theme::muted_text()).child(icon))
                                    .child(div().overflow_hidden().whitespace_nowrap().child(name))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if is_dir {
                                            if !this.expanded.remove(&path) {
                                                this.expanded.insert(path.clone());
                                            }
                                            cx.notify();
                                        } else {
                                            this.open(&path, window, cx);
                                        }
                                    }))
                            })
                            .collect()
                    }),
                )
                .flex_1(),
            )
            .children(self.task_list(cx))
    }

    /// The bottom of the sidebar: searches and other tasks, running or finished.
    fn task_list(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let manager = tasks::manager(cx);
        let m = manager.read(cx);
        let rows: Vec<_> = m
            .tasks()
            .iter()
            .filter(|t| t.status != TaskStatus::Draft)
            .map(|t| {
                let id = t.id;
                let active = t.status.active();
                let colour = match &t.status {
                    TaskStatus::Failed(_) => theme::error(),
                    TaskStatus::Running | TaskStatus::Queued | TaskStatus::Cancelling => theme::accent(),
                    _ => theme::muted_text(),
                };
                let front = m.foreground == Some(id);
                let unseen = t.unseen && !front;
                div()
                    .id(("task", id as usize))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .when(front, |d| d.bg(theme::active()))
                    .when(unseen, |d| d.bg(theme::accent().opacity(0.16)).border_l_2().border_color(theme::accent()).pl(px(10.0)))
                    .hover(|s| s.bg(theme::hover()))
                    .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(colour).flex_none())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .when(unseen, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(format!("{}: {}", t.kind, t.title)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if unseen { theme::accent() } else { theme::muted_text() })
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(match (&t.status, &t.progress) {
                                        (TaskStatus::Running, Some(p)) => p.clone(),
                                        (s, _) if unseen => format!("{} · new", s.label()),
                                        (s, _) => s.label().to_string(),
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id(("task-x", id as usize))
                            .px_1()
                            .rounded_sm()
                            .text_color(theme::muted_text())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                            .child("×")
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                // A first click stops a running task; a second clears it away.
                                tasks::manager(cx).update(cx, |m, cx| if active { m.cancel(id, cx) } else { m.dismiss(id, cx) })
                            }),
                    )
                    .on_click(move |_, _, cx| tasks::manager(cx).update(cx, |m, cx| m.bring_forward(id, cx)))
            })
            .collect();
        (!rows.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .max_h(px(260.0))
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::muted_text())
                        .child("TASKS"),
                )
                .child(div().id("task-rows").flex().flex_col().overflow_y_scroll().pb_1().children(rows))
        })
    }

    fn tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("tab-strip")
            .flex()
            .flex_row()
            .h(px(36.0))
            .bg(theme::panel())
            .border_b_1()
            .border_color(theme::border())
            .overflow_x_scroll()
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let active = self.active == Some(ix);
                let dirty = match &tab.content {
                    TabContent::Editor(ed, _) => ed.read(cx).doc.dirty,
                    TabContent::Failed(_) => false,
                };
                div()
                    .id(("tab", ix))
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h_full()
                    .pl_3()
                    .pr_2()
                    .text_sm()
                    .border_r_1()
                    .border_color(theme::border())
                    .cursor_pointer()
                    .when(active, |d| d.bg(theme::editor_bg()).text_color(theme::accent()).border_t_2().border_color(theme::accent()))
                    .when(!active, |d| d.text_color(theme::muted_text()).hover(|s| s.bg(theme::hover())))
                    .child(tab.title(cx))
                    .child(
                        div()
                            .id(("close", ix))
                            .w(px(16.0))
                            .flex()
                            .justify_center()
                            .rounded_sm()
                            .hover(|s| s.bg(theme::border()))
                            .child(if dirty { "●" } else { "×" })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close(ix, window, cx);
                            })),
                    )
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, window, cx| this.close(ix, window, cx)))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
            }))
    }

    fn menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_editor = self.active_editor().is_some();
        let has_tab = self.active.is_some();
        let item = |id: &'static str, label: &'static str, keys: &'static str, enabled: bool, action: Box<dyn gpui::Action>| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .h(px(28.0))
                .px_3()
                .mx_1()
                .rounded_md()
                .text_sm()
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(theme::hover())))
                .when(!enabled, |d| d.text_color(theme::ruler()))
                .child(div().flex_1().child(label))
                .child(div().text_xs().text_color(theme::muted_text()).child(keys))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !enabled {
                        return;
                    }
                    this.menu_open = false;
                    this.refocus_editor(window, cx);
                    window.dispatch_action(action.boxed_clone(), cx);
                    cx.notify();
                }))
        };
        let separator = || div().my_1().h(px(1.0)).bg(theme::border());

        let k = |mac: &'static str, other: &'static str| if cfg!(target_os = "macos") { mac } else { other };
        div()
            .id("menu")
            .absolute()
            .top(px(34.0))
            .left(px(4.0))
            .w(px(250.0))
            .py_1()
            .flex()
            .flex_col()
            .bg(theme::editor_bg())
            .border_1()
            .border_color(theme::border())
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .child(item("m-new", "New File", k("⌘N", "Ctrl+N"), true, Box::new(NewFile)))
            .child(item("m-open", "Open File…", k("⌘O", "Ctrl+O"), true, Box::new(OpenFile)))
            .child(item("m-folder", "Open Folder…", k("⇧⌘O", "Ctrl+Shift+O"), true, Box::new(OpenFolder)))
            .child(separator())
            .child(item("m-save", "Save", k("⌘S", "Ctrl+S"), has_editor, Box::new(Save)))
            .child(item("m-save-as", "Save As…", k("⇧⌘S", "Ctrl+Shift+S"), has_editor, Box::new(SaveAs)))
            .child(item("m-export", "Export As…", k("⌘E", "Ctrl+E"), has_editor, Box::new(ExportAs)))
            .child(item("m-close", "Close Tab", k("⌘W", "Ctrl+W"), has_tab, Box::new(CloseTab)))
            .child(separator())
            .child(item("m-fullscreen", "Toggle Full Screen", k("⌃⌘F", "F11"), true, Box::new(ToggleFullScreen)))
            .child(item("m-settings", "Settings…", k("⌘,", "Ctrl+,"), true, Box::new(OpenSettings)))
            .child(item("m-about", "About…", "", true, Box::new(About)))
            .child(separator())
            .child(item("m-quit", "Quit", k("⌘Q", "Ctrl+Q"), true, Box::new(Quit)))
    }

    fn settings_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filtered = self.filtered_themes();
        let no_matches = filtered.is_empty();
        let themes: Vec<ThemeEntry> = filtered.iter().map(|&i| self.themes[i].clone()).collect();
        let current = self.settings.theme.clone();
        let current_name = self.themes.iter().find(|t| t.id == current).map(|t| t.name.clone()).unwrap_or_default();
        let custom_count = self.themes.iter().filter(|t| matches!(t.source, ThemeSource::Custom(_))).count();

        let chip = |id: &'static str, label: String, f: ThemeFilter, selected: bool| {
            div()
                .id(id)
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .border_1()
                .border_color(if selected { theme::accent() } else { theme::border() })
                .when(selected, |d| d.bg(theme::active()).text_color(theme::accent()))
                .hover(|s| s.bg(theme::hover()))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.theme_filter = f;
                    this.scroll_to_current_theme();
                    cx.notify();
                }))
        };
        let page_tab = |id: &'static str, label: &'static str, page: SettingsPage, current: SettingsPage| {
            let selected = page == current;
            div()
                .id(id)
                .h_full()
                .flex()
                .items_center()
                .px_3()
                .text_sm()
                .cursor_pointer()
                .border_b_2()
                .border_color(if selected { theme::accent() } else { gpui::transparent_black() })
                .text_color(if selected { theme::text() } else { theme::muted_text() })
                .hover(|s| s.text_color(theme::text()))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings_page = page;
                    if page == SettingsPage::Appearance {
                        this.scroll_to_current_theme();
                    }
                    cx.notify();
                }))
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .border_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()))
                .child(label)
        };
        let heading = |text: &'static str| {
            div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::muted_text()).child(text)
        };

        let list = uniform_list(
            "theme-list",
            themes.len(),
            cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|ix| {
                        let t = &themes[ix];
                        let selected = t.id == current;
                        let global_ix = filtered[ix];
                        let sw = t.swatches();
                        div()
                            .id(("theme", ix))
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .h(px(30.0))
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .when(selected, |d| d.bg(theme::active()))
                            .hover(|s| s.bg(theme::hover()))
                            .child(swatch_strip(&sw))
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .when(selected, |d| d.text_color(theme::accent()))
                                    .child(t.name.clone()),
                            )
                            .when(matches!(t.source, ThemeSource::Custom(_)), |d| {
                                d.child(div().text_xs().text_color(theme::muted_text()).child("custom"))
                            })
                            .child(div().w(px(36.0)).text_xs().text_color(theme::muted_text()).child(if t.dark { "dark" } else { "light" }))
                            .child(div().w(px(14.0)).text_color(theme::accent()).child(if selected { "✓" } else { "" }))
                            .on_click(cx.listener(move |this, _, window, cx| this.apply_theme(global_ix, window, cx)))
                    })
                    .collect()
            }),
        )
        .track_scroll(self.theme_scroll.clone())
        .w_full()
        .h(px(270.0));

        let config_hint = settings::config_path().map(|p| p.display().to_string()).unwrap_or_else(|| "(no config directory)".into());

        div()
            .id("settings")
            .flex()
            .flex_col()
            .w(px(640.0))
            .max_h_full()
            .bg(theme::editor_bg())
            .border_1()
            .border_color(theme::border())
            .rounded_lg()
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // Header
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(44.0))
                    .px_4()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).mr_4().child("Settings"))
                    .child(page_tab("page-appearance", "Appearance", SettingsPage::Appearance, self.settings_page))
                    .child(page_tab("page-hotkeys", "Hotkeys", SettingsPage::Hotkeys, self.settings_page))
                    .child(page_tab("page-keys", "Keys", SettingsPage::Keys, self.settings_page))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("settings-close")
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_color(theme::muted_text())
                            .hover(|s| s.bg(theme::hover()))
                            .child("×")
                            .on_click(cx.listener(|this, _, window, cx| this.dismiss(&Dismiss, window, cx))),
                    ),
            )
            .when(self.settings_page == SettingsPage::Hotkeys, |d| d.child(hotkeys_page()))
            .when(self.settings_page == SettingsPage::Keys, |d| d.child(self.keys_page()))
            .when(self.settings_page == SettingsPage::Appearance, |d| {
                // Theme picker
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_4()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(heading("THEME"))
                                .child(div().text_xs().text_color(theme::muted_text()).child(format!("· {current_name}")))
                                .child(div().flex_1())
                                .child(chip("f-all", "All".into(), ThemeFilter::All, self.theme_filter == ThemeFilter::All))
                                .child(chip("f-dark", "Dark".into(), ThemeFilter::Dark, self.theme_filter == ThemeFilter::Dark))
                                .child(chip("f-light", "Light".into(), ThemeFilter::Light, self.theme_filter == ThemeFilter::Light))
                                .child(chip(
                                    "f-custom",
                                    format!("Custom ({custom_count})"),
                                    ThemeFilter::Custom,
                                    self.theme_filter == ThemeFilter::Custom,
                                )),
                        )
                        .child(
                            div()
                                .p_1()
                                .border_1()
                                .border_color(theme::border())
                                .rounded_md()
                                .child(list)
                                .when(no_matches, |d| {
                                    d.child(
                                        div()
                                            .p_2()
                                            .text_sm()
                                            .text_color(theme::muted_text())
                                            .child("No custom themes yet. Add a base16 .yaml file or a folder of them below."),
                                    )
                                }),
                        ),
                )
                // Custom base16 sources
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .px_4()
                        .pb_4()
                        .child(heading("CUSTOM BASE16 THEMES"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::muted_text())
                                .child("Point at a base16 scheme (.yaml, tinted-theming or original chriskempson layout) or a folder of them."),
                        )
                        .children(self.settings.custom_theme_paths.iter().enumerate().map(|(ix, p)| {
                            let n = self.themes.iter().filter(|t| t.source == ThemeSource::Custom(p.clone())).count();
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(p.display().to_string()))
                                .child(div().text_xs().text_color(theme::muted_text()).child(format!("{n} scheme{}", if n == 1 { "" } else { "s" })))
                                .child(
                                    div()
                                        .id(("remove-custom", ix))
                                        .px_2()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_color(theme::muted_text())
                                        .hover(|s| s.bg(theme::hover()))
                                        .child("×")
                                        .on_click(cx.listener(move |this, _, window, cx| this.remove_custom_path(ix, window, cx))),
                                )
                        }))
                        .children(self.custom_errors.iter().map(|e| div().text_xs().text_color(theme::error()).child(e.clone())))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(button("add-theme-file", "Add file…").on_click(cx.listener(|this, _, window, cx| this.add_custom_theme(false, window, cx))))
                                .child(button("add-theme-dir", "Add folder…").on_click(cx.listener(|this, _, window, cx| this.add_custom_theme(true, window, cx))))
                                .child(button("reload-themes", "Reload").on_click(cx.listener(|this, _, window, cx| {
                                    this.reload_themes();
                                    if let Some(t) = this.themes.iter().find(|t| t.id == this.settings.theme) {
                                        theme::set(t.theme());
                                    }
                                    window.refresh();
                                    cx.notify();
                                })))
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .id("browse-schemes")
                                        .text_xs()
                                        .text_color(theme::accent())
                                        .cursor_pointer()
                                        .child("Browse base16 schemes ↗")
                                        .on_click(|_, _, cx| cx.open_url(SCHEMES_URL)),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_color(theme::muted_text())
                    .child(format!("Saved to {config_hint}")),
            )
    }

    /// Settings → Keys: contact details and keys for online services.
    fn keys_page(&self) -> impl IntoElement {
        let heading = |text: &'static str| {
            div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::muted_text()).child(text)
        };
        let missing = self.settings.email.is_none();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .h(px(470.0))
            .child(heading("NCBI"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(60.0)).text_sm().child("Email"))
                    .child(
                        div()
                            .flex_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::app_bg())
                            .text_sm()
                            .child(self.email_input.clone()),
                    ),
            )
            .child(div().text_xs().text_color(theme::muted_text()).child(
                "BLAST searches send this address to NCBI with the tool name (plasmid-gpui), as NCBI's usage rules ask, \
                 so they can get in touch about a problem before blocking heavy use. It goes nowhere else.",
            ))
            .when(missing, |d| {
                d.child(div().text_xs().text_color(theme::error()).child("BLAST needs an email address before it can run."))
            })
    }
}

/// Settings → Hotkeys: every shortcut, grouped by where it applies.
fn hotkeys_page() -> impl IntoElement {
    let key_cap = |keys: &'static str| {
        div()
            .px_2()
            .py(px(1.0))
            .rounded_sm()
            .border_1()
            .border_color(theme::border())
            .bg(theme::panel())
            .font(crate::editor::editor_font())
            .text_xs()
            .whitespace_nowrap()
            .child(keys)
    };
    div()
        .id("hotkeys")
        .flex()
        .flex_col()
        .gap_4()
        .p_4()
        .h(px(470.0))
        .overflow_y_scroll()
        .children(hotkeys::GROUPS.iter().map(move |group| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .pb_1()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::muted_text())
                        .child(group.title),
                )
                .children(group.hotkeys.iter().map(move |h| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .min_h(px(26.0))
                        .px_2()
                        .rounded_md()
                        .hover(|s| s.bg(theme::hover()))
                        .child(div().flex_1().text_sm().child(h.description))
                        .child(key_cap(h.keys()))
                }))
        }))
}

/// A row of small color chips: the backgrounds, the foreground, then the accents.
fn swatch_strip(sw: &[Hsla; 16]) -> impl IntoElement {
    const ORDER: [usize; 10] = [0x0, 0x2, 0x5, 0x8, 0x9, 0xA, 0xB, 0xC, 0xD, 0xE];
    div()
        .flex()
        .flex_row()
        .flex_none()
        .rounded_sm()
        .overflow_hidden()
        .border_1()
        .border_color(theme::border())
        .children(ORDER.iter().map(|&i| div().w(px(9.0)).h(px(16.0)).bg(sw[i])))
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let task_view = {
            let m = tasks::manager(cx);
            let m = m.read(cx);
            m.foreground.and_then(|id| m.task(id)).map(|t| t.view.clone())
        };
        let body = match self.active.map(|a| &self.tabs[a].content) {
            Some(TabContent::Editor(_, panes)) => div().size_full().child(panes.clone()),
            Some(TabContent::Failed(msg)) => div().p_4().text_color(theme::error()).child(msg.clone()),
            None => {
                let k = |mac: &'static str, other: &'static str| if cfg!(target_os = "macos") { mac } else { other };
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .items_center()
                    .justify_center()
                    .text_color(theme::muted_text())
                    .child("Open a GenBank or FASTA file from the explorer")
                    .child(div().text_xs().child(format!(
                        "{} new file · {} open file · {} open folder · {} settings",
                        k("⌘N", "Ctrl+N"),
                        k("⌘O", "Ctrl+O"),
                        k("⇧⌘O", "Ctrl+Shift+O"),
                        k("⌘,", "Ctrl+,")
                    )))
            }
        };

        div()
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(theme::app_bg())
            .text_color(theme::text())
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::open_queued))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::export_as))
            .on_action(cx.listener(Self::close_active))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::open_about))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(Self::toggle_fullscreen))
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.tab_strip(cx))
                    .child(div().flex_1().min_h_0().child(body)),
            )
            .when_some(self.notice.clone(), |d, notice| {
                d.child(
                    div()
                        .id("notice")
                        .absolute()
                        .bottom(px(36.0))
                        .right(px(16.0))
                        .max_w(px(480.0))
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(theme::panel())
                        .border_1()
                        .border_color(theme::error())
                        .text_sm()
                        .cursor_pointer()
                        .child(notice)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.notice = None;
                            cx.notify();
                        })),
                )
            })
            .when(self.menu_open, |d| {
                d.child(
                    div()
                        .id("menu-backdrop")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.toggle_menu(window, cx))),
                )
                .child(self.menu(cx))
            })
            .when(self.settings_open, |d| {
                d.child(
                    div()
                        .id("settings-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_6()
                        .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.dismiss(&Dismiss, window, cx)))
                        .child(self.settings_panel(cx)),
                )
            })
            .when_some(task_view, |d, view| {
                d.child(
                    div()
                        .id("task-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_6()
                        .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                        .occlude()
                        .child(view),
                )
            })
            .when_some(self.about.clone(), |d, panel| {
                d.child(
                    div()
                        .id("about-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_6()
                        .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.dismiss(&Dismiss, window, cx)))
                        .child(panel),
                )
            })
    }
}

//! Long-running work that runs off the UI thread: BLAST searches now, plugin jobs
//! later. A task belongs to an editor, may lock a region of its sequence while it
//! runs and until its results are dismissed, and has a view (its dialog) that the
//! workspace shows while the task is in the foreground.
//!
//! Tasks for the same service queue up and run in order, as many at a time as the
//! service allows (NCBI: one).

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Result};
use gpui::{AnyView, App, AppContext as _, Context, Entity, Global, WeakEntity};

use crate::editor::SeqEditor;

pub type TaskId = u64;
pub type Output = Arc<dyn Any + Send + Sync>;

/// Work done on a background thread. `run` blocks; it should check `ctx` for
/// cancellation (and sleep through `ctx.sleep`) so Cancel takes effect quickly.
pub trait TaskRunner: Send + 'static {
    /// Tasks with the same service share a queue and its concurrency limit.
    fn service(&self) -> &'static str;
    fn run(self: Box<Self>, ctx: &TaskCtx) -> Result<Output>;
}

/// How many tasks of a service may run at once.
fn limit(service: &str) -> usize {
    match service {
        // NCBI asks for one search at a time from a client.
        "ncbi" => 1,
        _ => 4,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TaskStatus {
    /// Its dialog is open but it hasn't been started.
    Draft,
    Queued,
    Running,
    /// Cancel was pressed; the worker hasn't stopped yet.
    Cancelling,
    Done,
    Failed(String),
    Cancelled,
}

impl TaskStatus {
    pub fn finished(&self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Failed(_) | TaskStatus::Cancelled)
    }

    pub fn active(&self) -> bool {
        matches!(self, TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling)
    }

    pub fn label(&self) -> &'static str {
        match self {
            TaskStatus::Draft => "Not started",
            TaskStatus::Queued => "Queued",
            TaskStatus::Running => "Running",
            TaskStatus::Cancelling => "Cancelling",
            TaskStatus::Done => "Done",
            TaskStatus::Failed(_) => "Failed",
            TaskStatus::Cancelled => "Cancelled",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LogKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub text: String,
    pub kind: LogKind,
}

pub struct Task {
    pub id: TaskId,
    /// What kind of task, for the sidebar ("BLASTN", "Plugin").
    pub kind: String,
    pub title: String,
    pub status: TaskStatus,
    pub log: Vec<LogLine>,
    /// The worker's latest one-line status ("Waiting for results · checked 3 times").
    pub progress: Option<String>,
    pub editor: WeakEntity<SeqEditor>,
    pub view: AnyView,
    pub output: Option<Output>,
    /// When it finished, for notes on what it found.
    pub finished_at: Option<SystemTime>,
    /// A region of the editor's sequence is locked under this task's id.
    pub locks: bool,
    service: &'static str,
    runner: Option<Box<dyn TaskRunner>>,
    cancel: Arc<AtomicBool>,
}

/// What a worker thread can tell the UI.
pub(crate) enum Msg {
    Log(LogLine),
    Progress(String),
    Done(std::result::Result<Output, String>),
}

/// Handed to a running task: report progress, notice cancellation.
pub struct TaskCtx {
    tx: async_channel::Sender<Msg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
}

#[cfg(test)]
pub fn debug_msg(m: &Msg) -> String {
    match m {
        Msg::Log(l) => l.text.clone(),
        Msg::Progress(p) => format!("({p})"),
        Msg::Done(r) => format!("done: {:?}", r.as_ref().err()),
    }
}

impl TaskCtx {
    #[cfg(test)]
    pub fn for_test(tx: async_channel::Sender<Msg>) -> TaskCtx {
        TaskCtx { tx, cancel: Arc::new(AtomicBool::new(false)), started: Instant::now() }
    }

    fn line(&self, text: &str, kind: LogKind) {
        let t = self.started.elapsed().as_secs();
        let text = format!("[{:02}:{:02}] {text}", t / 60, t % 60);
        self.tx.send_blocking(Msg::Log(LogLine { text, kind })).ok();
    }

    pub fn log(&self, text: impl AsRef<str>) {
        self.line(text.as_ref(), LogKind::Info);
    }

    pub fn warn(&self, text: impl AsRef<str>) {
        self.line(text.as_ref(), LogKind::Error);
    }

    pub fn progress(&self, text: impl Into<String>) {
        self.tx.send_blocking(Msg::Progress(text.into())).ok();
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Fail with "Cancelled" if Cancel was pressed.
    pub fn check(&self) -> Result<()> {
        if self.cancelled() {
            bail!("Cancelled");
        }
        Ok(())
    }

    /// Sleep, waking early (with an error) if the task is cancelled.
    pub fn sleep(&self, d: Duration) -> Result<()> {
        let until = Instant::now() + d;
        loop {
            self.check()?;
            let now = Instant::now();
            if now >= until {
                return Ok(());
            }
            std::thread::sleep((until - now).min(Duration::from_millis(100)));
        }
    }
}

#[derive(Default)]
pub struct TaskManager {
    tasks: Vec<Task>,
    next_id: TaskId,
    /// The task whose view is showing.
    pub foreground: Option<TaskId>,
}

struct GlobalTasks(Entity<TaskManager>);
impl Global for GlobalTasks {}

pub fn init(cx: &mut App) {
    let m = cx.new(|_| TaskManager { next_id: 1, ..Default::default() });
    cx.set_global(GlobalTasks(m));
}

pub fn manager(cx: &App) -> Entity<TaskManager> {
    cx.global::<GlobalTasks>().0.clone()
}

impl TaskManager {
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    fn task_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// The tasks that belong to `editor` and aren't drafts.
    pub fn for_editor(&self, editor: &WeakEntity<SeqEditor>) -> Vec<TaskId> {
        self.tasks.iter().filter(|t| t.editor == *editor && t.status != TaskStatus::Draft).map(|t| t.id).collect()
    }

    /// Add a task in the Draft state with `view` as its dialog, and bring it forward.
    /// `view` is built knowing the id the task will get.
    pub fn create(
        &mut self,
        kind: impl Into<String>,
        title: impl Into<String>,
        editor: WeakEntity<SeqEditor>,
        view: impl FnOnce(TaskId, &mut Context<Self>) -> AnyView,
        cx: &mut Context<Self>,
    ) -> TaskId {
        let id = self.next_id;
        self.next_id += 1;
        let view = view(id, cx);
        self.tasks.push(Task {
            id,
            kind: kind.into(),
            title: title.into(),
            status: TaskStatus::Draft,
            log: Vec::new(),
            progress: None,
            editor,
            view,
            output: None,
            finished_at: None,
            locks: false,
            service: "",
            runner: None,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        self.foreground = Some(id);
        cx.notify();
        id
    }

    pub fn set_title(&mut self, id: TaskId, kind: impl Into<String>, title: impl Into<String>, cx: &mut Context<Self>) {
        if let Some(t) = self.task_mut(id) {
            t.kind = kind.into();
            t.title = title.into();
            cx.notify();
        }
    }

    /// How many tasks would run before one of `service` submitted now.
    pub fn ahead(&self, service: &str) -> usize {
        self.tasks.iter().filter(|t| t.service == service && t.status.active()).count().saturating_sub(limit(service) - 1)
    }

    /// Queue a draft task to run. If `lock` is given, that region of the editor's
    /// sequence (through the origin when start > end) is locked under the task's id
    /// until the task is dismissed, cancelled or fails. Errors if the region can't be
    /// locked.
    pub fn submit(&mut self, id: TaskId, runner: Box<dyn TaskRunner>, lock: Option<(usize, usize)>, cx: &mut Context<Self>) -> Result<()> {
        let Some(t) = self.task_mut(id) else { bail!("no such task") };
        if t.status != TaskStatus::Draft {
            bail!("already started");
        }
        if let Some((s, e)) = lock {
            let Some(ed) = t.editor.upgrade() else { bail!("the sequence was closed") };
            if !ed.update(cx, |ed, cx| {
                let ok = ed.doc.lock(id, s, e);
                cx.notify();
                ok
            }) {
                bail!("couldn't lock the region");
            }
            let t = self.task_mut(id).unwrap();
            t.locks = true;
        }
        let t = self.task_mut(id).unwrap();
        t.service = runner.service();
        t.runner = Some(runner);
        t.status = TaskStatus::Queued;
        t.log.push(LogLine { text: "Queued".into(), kind: LogKind::Info });
        self.pump(cx);
        cx.notify();
        Ok(())
    }

    /// Start whatever queued tasks the services' limits allow, oldest first.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let mut start = Vec::new();
        for t in &self.tasks {
            if t.status != TaskStatus::Queued {
                continue;
            }
            let running = self
                .tasks
                .iter()
                .filter(|o| o.service == t.service && matches!(o.status, TaskStatus::Running | TaskStatus::Cancelling))
                .count()
                + start.iter().filter(|(_, s)| *s == t.service).count();
            if running < limit(t.service) {
                start.push((t.id, t.service));
            }
        }
        for (id, _) in start {
            self.start(id, cx);
        }
    }

    fn start(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let Some(t) = self.task_mut(id) else { return };
        let Some(runner) = t.runner.take() else { return };
        t.status = TaskStatus::Running;
        let (tx, rx) = async_channel::unbounded::<Msg>();
        let ctx = TaskCtx { tx: tx.clone(), cancel: t.cancel.clone(), started: Instant::now() };
        let spawned = std::thread::Builder::new().name(format!("task-{id}")).spawn(move || {
            let result = runner.run(&ctx).map_err(|e| format!("{e:#}"));
            tx.send_blocking(Msg::Done(result)).ok();
        });
        if let Err(e) = spawned {
            self.finish(id, Err(format!("couldn't start a thread: {e}")), cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                if this.update(cx, |m, cx| m.on_msg(id, msg, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    fn on_msg(&mut self, id: TaskId, msg: Msg, cx: &mut Context<Self>) {
        match msg {
            Msg::Log(line) => {
                if let Some(t) = self.task_mut(id) {
                    t.log.push(line);
                }
            }
            Msg::Progress(p) => {
                if let Some(t) = self.task_mut(id) {
                    t.progress = Some(p);
                }
            }
            Msg::Done(result) => self.finish(id, result, cx),
        }
        cx.notify();
    }

    fn finish(&mut self, id: TaskId, result: std::result::Result<Output, String>, cx: &mut Context<Self>) {
        let Some(t) = self.task_mut(id) else { return };
        let cancelled = t.cancel.load(Ordering::Relaxed);
        t.finished_at = Some(SystemTime::now());
        t.progress = None;
        match result {
            _ if cancelled => {
                t.status = TaskStatus::Cancelled;
                t.log.push(LogLine { text: "Cancelled".into(), kind: LogKind::Error });
            }
            Ok(out) => {
                t.status = TaskStatus::Done;
                t.output = Some(out);
            }
            Err(e) => {
                t.log.push(LogLine { text: format!("Failed: {e}"), kind: LogKind::Error });
                t.status = TaskStatus::Failed(e);
            }
        }
        if !matches!(t.status, TaskStatus::Done) {
            self.release(id, cx);
        }
        self.pump(cx);
        cx.notify();
    }

    /// Stop a task: a queued one at once, a running one as soon as its worker notices.
    /// Its lock goes once it has stopped.
    pub fn cancel(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let Some(t) = self.task_mut(id) else { return };
        match t.status {
            TaskStatus::Queued => {
                t.runner = None;
                t.status = TaskStatus::Cancelled;
                t.finished_at = Some(SystemTime::now());
                t.log.push(LogLine { text: "Cancelled before it started".into(), kind: LogKind::Error });
                self.release(id, cx);
            }
            TaskStatus::Running => {
                t.cancel.store(true, Ordering::Relaxed);
                t.status = TaskStatus::Cancelling;
                t.log.push(LogLine { text: "Cancelling…".into(), kind: LogKind::Info });
            }
            _ => {}
        }
        cx.notify();
    }

    /// Remove a task and its results (cancelling it if it's still going), unlocking
    /// its region.
    pub fn dismiss(&mut self, id: TaskId, cx: &mut Context<Self>) {
        if let Some(t) = self.task_mut(id) {
            t.cancel.store(true, Ordering::Relaxed);
        }
        self.release(id, cx);
        self.tasks.retain(|t| t.id != id);
        if self.foreground == Some(id) {
            self.foreground = None;
        }
        cx.notify();
    }

    fn release(&mut self, id: TaskId, cx: &mut Context<Self>) {
        let Some(t) = self.task_mut(id) else { return };
        if !std::mem::take(&mut t.locks) {
            return;
        }
        if let Some(ed) = t.editor.upgrade() {
            ed.update(cx, |ed, cx| {
                ed.doc.unlock(id);
                cx.notify();
            });
        }
    }

    pub fn bring_forward(&mut self, id: TaskId, cx: &mut Context<Self>) {
        if self.task(id).is_some() {
            self.foreground = Some(id);
            cx.notify();
        }
    }

    /// Hide the foreground task's view; it keeps running and stays in the sidebar.
    /// A draft has nothing to keep, so it goes.
    pub fn background(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.foreground.take() {
            if self.task(id).is_some_and(|t| t.status == TaskStatus::Draft) {
                self.tasks.retain(|t| t.id != id);
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert!(TaskStatus::Failed("x".into()).finished());
        assert!(!TaskStatus::Running.finished());
        assert!(TaskStatus::Cancelling.active());
        assert!(!TaskStatus::Draft.active());
    }

    #[test]
    fn sleep_wakes_on_cancel() {
        let (tx, _rx) = async_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = TaskCtx { tx, cancel: cancel.clone(), started: Instant::now() };
        let c = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            c.store(true, Ordering::Relaxed);
        });
        let began = Instant::now();
        assert!(ctx.sleep(Duration::from_secs(30)).is_err());
        assert!(began.elapsed() < Duration::from_secs(2));
        t.join().unwrap();
    }
}

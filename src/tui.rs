//! The runtime around [`App`]: polls sources, reconciles and routes events, launches agents under
//! the concurrency cap, mirrors hook-driven state from the database, and performs UI effects.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::actions::{self, Renderer, RexSpawner, RunSpec, Spawner};
use crate::config::{Config, Paths};
use crate::exec::SystemRunner;
use crate::model::{BlockId, ItemEvent, ItemId, ProfileName, RunId, RunState, SourceId};
use crate::rules::Mode;
use crate::sources::{self, Source};
use crate::store::Store;
use crate::ui::draw::draw;
use crate::ui::state::{App, Effect, ProfileChoice, ToastKind};

const TICK: Duration = Duration::from_millis(250);
/// Refresh the selected agent's pane output this often (in ticks).
const CAPTURE_EVERY: usize = 8;

enum Msg {
    Key(event::KeyEvent),
    Mouse(MouseEvent),
    Redraw,
    Tick,
    Fetched {
        source: SourceId,
        result: Result<Vec<crate::model::Item>, String>,
    },
    Launched {
        id: RunId,
        result: Result<BlockId, String>,
    },
    Captured {
        block: BlockId,
        text: String,
    },
    Toast(String, ToastKind),
}

type Rex = RexSpawner<SystemRunner>;

struct Runtime {
    app: App,
    store: Store,
    config: Config,
    renderer: Renderer,
    rex: Arc<Rex>,
    tx: UnboundedSender<Msg>,
    known: HashSet<SourceId>,
    syncs: Vec<Arc<Notify>>,
    /// Runs reserved in the store but waiting for a free slot.
    pending: VecDeque<(RunId, RunSpec)>,
    inflight: usize,
    quit: bool,
}

pub async fn run(paths: &Paths) -> Result<()> {
    let config = Config::load(&paths.config)?;
    let store = Store::open(&paths.database)?;
    let now = Utc::now();
    store.interrupt_unlaunched_runs(now)?;

    let profiles = config
        .profiles
        .iter()
        .map(|(name, p)| ProfileChoice {
            name: name.clone(),
            description: describe(p),
        })
        .collect();
    let mut app = App::new(config.sections.clone(), config.rules.clone(), profiles);
    app.set_dismissed(store.dismissals()?);
    app.set_hidden(store.hidden_namespaces()?);
    app.set_ignored(store.ignored()?);
    app.set_runs(store.runs()?, now);

    let (tx, mut rx) = unbounded_channel();
    let rex = Arc::new(RexSpawner::new(
        SystemRunner::default(),
        config.rex.session.clone(),
        std::env::current_exe()?,
        paths.database.clone(),
    ));
    let mut runtime = Runtime {
        known: store.known_sources()?,
        renderer: Renderer::new(&paths.home),
        app,
        store,
        config,
        rex,
        tx: tx.clone(),
        syncs: Vec::new(),
        pending: VecDeque::new(),
        inflight: 0,
        quit: false,
    };

    runtime.start_pollers();
    spawn_input(tx.clone());
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        loop {
            interval.tick().await;
            if tx.send(Msg::Tick).is_err() {
                break;
            }
        }
    });

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = event_loop(&mut terminal, &mut runtime, &mut rx);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    runtime: &mut Runtime,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, &mut runtime.app, Utc::now()))?;
        let Some(first) = tokio::task::block_in_place(|| rx.blocking_recv()) else {
            return Ok(());
        };
        runtime.handle(first);
        // Batch whatever else is queued so a burst of input draws once.
        while let Ok(next) = rx.try_recv() {
            runtime.handle(next);
        }
        if runtime.quit {
            return Ok(());
        }
    }
}

fn spawn_input(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            let msg = match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => Msg::Key(key),
                Event::Mouse(mouse) => Msg::Mouse(mouse),
                Event::Resize(..) => Msg::Redraw,
                _ => continue,
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
}

/// First line of the prompt, prefixed with the skill, for the profile picker.
fn describe(profile: &actions::Profile) -> String {
    let first = profile
        .prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    match &profile.skill {
        Some(skill) => format!("/{skill} {first}"),
        None => first.to_owned(),
    }
}

impl Runtime {
    fn start_pollers(&mut self) {
        for source in sources::build(&self.config) {
            let source: Arc<dyn Source> = Arc::from(source);
            let notify = Arc::new(Notify::new());
            self.syncs.push(Arc::clone(&notify));
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let runner = SystemRunner::default();
                loop {
                    let result = source.fetch(&runner).await.map_err(|e| e.to_string());
                    if tx
                        .send(Msg::Fetched {
                            source: source.id(),
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    tokio::select! {
                        () = tokio::time::sleep(source.poll_every()) => {}
                        () = notify.notified() => {}
                    }
                }
            });
        }
    }

    fn handle(&mut self, msg: Msg) {
        let now = Utc::now();
        match msg {
            Msg::Key(key) => {
                let effects = self.app.handle_key(key, now);
                self.perform(effects);
            }
            Msg::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let effects = self.app.click(mouse.column, mouse.row, now);
                    self.perform(effects);
                }
                MouseEventKind::ScrollDown => self.app.scroll(true),
                MouseEventKind::ScrollUp => self.app.scroll(false),
                _ => {}
            },
            Msg::Redraw => {}
            Msg::Tick => self.tick(),
            Msg::Fetched { source, result } => self.fetched(source, result),
            Msg::Launched { id, result } => self.launched(id, result),
            Msg::Captured { block, text } => self.app.set_capture(block, text),
            Msg::Toast(text, kind) => self.app.set_toast(text, kind, now),
        }
    }

    fn tick(&mut self) {
        self.app.tick += 1;
        self.refresh_runs();
        self.launch_pending();
        if self.app.tick.is_multiple_of(CAPTURE_EVERY) {
            self.capture_selected();
        }
    }

    /// Refresh the output shown for the selected agent's pane.
    fn capture_selected(&self) {
        let Some(block) = self
            .app
            .selected_run()
            .filter(|r| r.state.is_active())
            .and_then(|r| r.block.clone())
        else {
            return;
        };
        let rex = Arc::clone(&self.rex);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Ok(text) = rex.capture(&block).await {
                let _ = tx.send(Msg::Captured { block, text });
            }
        });
    }

    /// Hooks write run state from other processes, so the database is the source of truth.
    fn refresh_runs(&mut self) {
        if let Ok(runs) = self.store.runs() {
            self.app.set_runs(runs, Utc::now());
        }
    }

    fn toast(&mut self, text: String, kind: ToastKind) {
        self.app.set_toast(text, kind, Utc::now());
    }

    fn fetched(&mut self, source: SourceId, result: Result<Vec<crate::model::Item>, String>) {
        let now = Utc::now();
        let items = match result {
            Ok(items) => items,
            Err(error) => {
                self.app.set_source_error(source, error);
                return;
            }
        };
        let events = match self.store.reconcile(&source, &items, now) {
            Ok(events) => events,
            Err(err) => {
                self.app.set_source_error(source, err.to_string());
                return;
            }
        };
        // A source's first poll only baselines: routing it would launch an agent per existing item.
        let first = self.known.insert(source.clone());
        if !first {
            for event in &events {
                if let ItemEvent::New(item) = event {
                    self.app.mark_fresh(item.id.clone(), now);
                }
            }
            self.route(&events);
        }
        self.app.set_items(source, items, now);
    }

    fn route(&mut self, events: &[ItemEvent]) {
        let now = Utc::now();
        for event in events {
            // Ignored items never start agents on their own.
            let item = match event {
                ItemEvent::New(item) | ItemEvent::Changed { item, .. } => Some(item),
                ItemEvent::Gone(_) => None,
            };
            if item.is_some_and(|item| self.app.is_ignored(&item.id)) {
                continue;
            }
            for planned in actions::plan(&self.config, &self.renderer, event) {
                match (planned.mode, planned.run) {
                    (Mode::Auto, Ok(run)) => {
                        self.reserve(run);
                    }
                    (Mode::Suggest, Ok(_)) => {}
                    (_, Err(err)) => {
                        self.app
                            .set_toast(format!("{}: {err}", planned.rule), ToastKind::Bad, now)
                    }
                }
            }
        }
    }

    /// Record the run now (so it can never be started twice) and queue it for launching.
    fn reserve(&mut self, run: RunSpec) -> bool {
        match self
            .store
            .create_run(&run.item_id, &run.fingerprint, &run.profile, Utc::now())
        {
            Ok(Some(id)) => {
                self.pending.push_back((id, run));
                self.refresh_runs();
                true
            }
            Ok(None) => false,
            Err(err) => {
                self.toast(format!("could not record run: {err}"), ToastKind::Bad);
                false
            }
        }
    }

    fn launch_pending(&mut self) {
        let cap = self.config.rex.max_running.get();
        while self.app.active_count() + self.inflight < cap {
            let Some((id, run)) = self.pending.pop_front() else {
                break;
            };
            self.inflight += 1;
            let rex = Arc::clone(&self.rex);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = rex.spawn(&run, id).await.map_err(|e| e.to_string());
                let _ = tx.send(Msg::Launched { id, result });
            });
        }
    }

    fn launched(&mut self, id: RunId, result: Result<BlockId, String>) {
        self.inflight = self.inflight.saturating_sub(1);
        let now = Utc::now();
        match result {
            Ok(block) => {
                let _ = self.store.set_block(id, &block, now);
            }
            Err(reason) => {
                let _ = self.store.fail_run(id, &reason, now);
                self.toast(format!("agent failed to start: {reason}"), ToastKind::Bad);
            }
        }
        self.refresh_runs();
    }

    fn perform(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            self.perform_one(effect);
        }
    }

    fn perform_one(&mut self, effect: Effect) {
        let now = Utc::now();
        match effect {
            Effect::Quit => self.quit = true,
            Effect::Sync => {
                self.syncs.iter().for_each(|n| n.notify_one());
                self.toast("Syncing…".to_owned(), ToastKind::Info);
            }
            Effect::Open(url) => open_in_browser(&url),
            Effect::Spawn {
                item,
                profile,
                prompt,
            } => self.manual_spawn(&item, &profile, prompt),
            Effect::Focus(block) => self.background(
                |rex| async move { rex.focus(&block).await.map(|()| None) },
                "could not focus the pane",
            ),
            Effect::Reply { block, text } => self.background(
                move |rex| async move {
                    rex.reply(&block, text)
                        .await
                        .map(|()| Some(format!("Sent \"{text}\"")))
                },
                "could not reply",
            ),
            Effect::Dismiss(id, fingerprint) => {
                let _ = self.store.dismiss(&id, &fingerprint);
            }
            Effect::Undismiss(id) => {
                let _ = self.store.undismiss(&id);
            }
            Effect::Ignore(id) => {
                let _ = self.store.ignore(&id, now);
            }
            Effect::Unignore(id) => {
                let _ = self.store.unignore(&id);
            }
            Effect::ClearRun(id) => {
                let _ = self.store.clear_run(id);
            }
            Effect::SetNamespaceHidden(ns, hidden) => {
                let _ = self.store.set_namespace_hidden(&ns, hidden);
            }
            Effect::StopRun { run, block } => {
                self.pending.retain(|(id, _)| *id != run);
                let _ = self.store.set_state(run, RunState::Stopped, now);
                self.refresh_runs();
                if let Some(block) = block {
                    self.background(
                        |rex| async move { rex.close(&block).await.map(|()| None) },
                        "could not close the pane",
                    );
                }
            }
        }
    }

    /// Run a Rex call off the UI thread, toasting its result.
    fn background<F, Fut>(&self, call: F, failure: &'static str)
    where
        F: FnOnce(Arc<Rex>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Option<String>, actions::SpawnError>> + Send,
    {
        let rex = Arc::clone(&self.rex);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let msg = match call(rex).await {
                Ok(Some(text)) => Msg::Toast(text, ToastKind::Info),
                Ok(None) => return,
                Err(err) => Msg::Toast(format!("{failure}: {err}"), ToastKind::Bad),
            };
            let _ = tx.send(msg);
        });
    }

    fn manual_spawn(&mut self, item_id: &ItemId, profile: &ProfileName, prompt: Option<String>) {
        let now = Utc::now();
        let Some(item) = self.app.item(item_id).cloned() else {
            return;
        };
        let Some(definition) = self.config.profiles.get(profile) else {
            return;
        };
        let mut run = match self.renderer.render(profile, definition, &item, None) {
            Ok(run) => run,
            Err(err) => return self.toast(err.to_string(), ToastKind::Bad),
        };
        if let Some(text) = prompt {
            run.prompt = format!("{text}\n\nContext: {} ({})", item.title, item.url);
            // Each custom prompt is its own run, not a repeat of the profile's.
            run.profile = ProfileName::new(format!("prompt-{}", now.timestamp_millis()));
        }
        if self.reserve(run) {
            self.toast(
                format!("Queued {profile} on {}", item.title),
                ToastKind::Info,
            );
        } else {
            self.toast(
                format!("{profile} already ran on this version of the item"),
                ToastKind::Info,
            );
        }
    }
}

fn open_in_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

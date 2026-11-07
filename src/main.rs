mod actions;
mod config;
mod doctor;
mod engine;
mod exec;
mod hooks;
mod model;
mod rules;
mod sources;
mod store;
mod tui;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::exec::SystemRunner;

#[derive(Parser)]
#[command(name = "w0rk", about = "Route notifications to Claude agents in Rex")]
struct Cli {
    /// Config file (default: ~/.config/w0rk/config.toml).
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,
    /// State database (default: ~/.local/state/w0rk/w0rk.db).
    #[arg(long, global = true, env = "W0RK_DB")]
    db: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check that gh, appsignal-cli, rex and claude work.
    Doctor,
    /// Show which agents the rules would spawn for everything currently visible. Spawns nothing.
    Routes,
    /// Fetch every source once, route the new items, and start `auto` runs.
    Poll {
        /// Record what would spawn without opening Rex panes.
        #[arg(long)]
        dry_run: bool,
        /// Also route items that already existed the first time a source is polled.
        #[arg(long)]
        spawn_existing: bool,
    },
    /// Called by Claude Code hooks inside a spawned pane. Never fails.
    Hook { event: hooks::HookEvent },
    /// Called by a spawned agent to report its result.
    Report {
        /// ok | needs-you
        verdict: model::Verdict,
        summary: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut paths = config::Paths::discover()?;
    if let Some(config) = cli.config {
        paths = paths.with_config(config);
    }
    if let Some(db) = cli.db {
        paths = paths.with_database(db);
    }
    match cli.command {
        Some(Command::Poll {
            dry_run,
            spawn_existing,
        }) => run_poll(&paths, dry_run, spawn_existing).await,
        Some(Command::Hook { event }) => {
            run_hook(&paths, event);
            Ok(())
        }
        Some(Command::Report { verdict, summary }) => run_report(&paths, verdict, &summary),
        Some(Command::Doctor) => run_doctor(&paths).await,
        Some(Command::Routes) => run_routes(&paths).await,
        None => tui::run(&paths).await,
    }
}

async fn run_doctor(paths: &config::Paths) -> Result<()> {
    let runner = SystemRunner::default();
    let results = doctor::run_checks(&runner, doctor::checks()).await;
    let mut failed = false;
    for result in &results {
        match &result.outcome {
            Ok(()) => println!("ok    {}", result.name),
            Err(reason) => {
                failed = true;
                println!("FAIL  {}: {reason}", result.name);
            }
        }
    }

    match config::Config::load(&paths.config) {
        Ok(_) => println!("ok    config {}", paths.config.display()),
        Err(err) => {
            failed = true;
            println!("FAIL  config: {err}");
        }
    }

    if failed {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_routes(paths: &config::Paths) -> Result<()> {
    let config = config::Config::load(&paths.config)?;
    let renderer = actions::Renderer::new(&paths.home);
    let runner = SystemRunner::default();

    for source in sources::build(&config) {
        let items = match source.fetch(&runner).await {
            Ok(items) => items,
            Err(err) => {
                println!("{}: {err}", source.id());
                continue;
            }
        };
        println!("{} ({} items)", source.id(), items.len());
        for item in items {
            let event = model::ItemEvent::New(item);
            for planned in actions::plan(&config, &renderer, &event) {
                let mode = match planned.mode {
                    rules::Mode::Auto => "auto",
                    rules::Mode::Suggest => "suggest",
                };
                match planned.run {
                    Ok(run) => println!(
                        "  [{mode}] {} -> {}  {}\n         cwd {}{}{}\n         {}",
                        planned.rule,
                        run.profile,
                        run.item_id,
                        run.cwd.display(),
                        if run.worktree { " (worktree)" } else { "" },
                        if run.ensure_cwd().is_err() {
                            "  !! NOT CLONED, would not spawn"
                        } else {
                            ""
                        },
                        run.claude_command(),
                    ),
                    Err(err) => println!("  [{mode}] {}: {err}", planned.rule),
                }
            }
        }
    }
    Ok(())
}

/// Hooks run inside someone else's Claude session, so any problem is swallowed rather than
/// surfaced as a hook error.
fn run_hook(paths: &config::Paths, event: hooks::HookEvent) {
    let mut sink = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut sink);
    let var = std::env::var(hooks::RUN_ID_ENV).ok();
    let Some(run) = hooks::run_id_from(var.as_deref()) else {
        return;
    };
    if let Ok(store) = store::Store::open(&paths.database) {
        let _ = hooks::apply(&store, run, event, chrono::Utc::now());
    }
}

fn run_report(paths: &config::Paths, verdict: model::Verdict, summary: &str) -> Result<()> {
    let var = std::env::var(hooks::RUN_ID_ENV).ok();
    let run = hooks::run_id_from(var.as_deref()).ok_or_else(|| {
        anyhow::anyhow!(
            "{} is not set: not running inside a w0rk pane",
            hooks::RUN_ID_ENV
        )
    })?;
    let store = store::Store::open(&paths.database)?;
    hooks::report(&store, run, verdict, summary, chrono::Utc::now())?;
    println!("reported {} for run {run}", verdict.as_str());
    Ok(())
}

async fn run_poll(paths: &config::Paths, dry_run: bool, spawn_existing: bool) -> Result<()> {
    use actions::{DryRunSpawner, RexSpawner, Spawner};
    use engine::Outcome;

    let config = config::Config::load(&paths.config)?;
    let mut store = store::Store::open(&paths.database)?;
    let renderer = actions::Renderer::new(&paths.home);
    let runner = SystemRunner::default();
    let spawner: Box<dyn Spawner> = if dry_run {
        Box::new(DryRunSpawner::default())
    } else {
        Box::new(RexSpawner::new(
            SystemRunner::default(),
            config.rex.session.clone(),
            std::env::current_exe()?,
            paths.database.clone(),
        ))
    };

    let known = store.known_sources()?;
    for source in sources::build(&config) {
        let id = source.id();
        let items = match source.fetch(&runner).await {
            Ok(items) => items,
            Err(err) => {
                println!("{id}: fetch failed: {err}");
                continue;
            }
        };
        let now = chrono::Utc::now();
        let events = store.reconcile(&id, &items, now)?;
        if engine::is_first_poll(&known, &id) && !spawn_existing {
            println!(
                "{id}: baseline of {} items recorded, nothing routed (use --spawn-existing to override)",
                items.len()
            );
            continue;
        }
        println!("{id}: {} items, {} events", items.len(), events.len());
        for outcome in
            engine::dispatch(&store, spawner.as_ref(), &config, &renderer, &events, now).await?
        {
            match outcome {
                Outcome::Spawned { rule, id, block } => {
                    println!("  spawned run {id} ({rule}) in {block}")
                }
                Outcome::Suggested { rule, run } => println!("  suggest {rule}: {}", run.item_id),
                Outcome::AlreadyRan { rule } => println!("  {rule}: already ran"),
                Outcome::Failed { rule, reason } => println!("  {rule}: FAILED {reason}"),
            }
        }
    }
    Ok(())
}

//! Actions: turn routed items into agent runs. [`Profile`] says what to run, [`Spawner`] says how.

pub mod profile;
pub mod rex;

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use thiserror::Error;

pub use profile::{Profile, ProfileError, Renderer};
pub use rex::RexSpawner;

use crate::config::Config;
use crate::exec::{CommandSpec, ExecError, Program};
use crate::model::{BlockId, Fingerprint, ItemEvent, ItemId, ProfileName, RunId};
use crate::rules::{self, Mode};

/// Everything needed to start one agent, fully rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    pub item_id: ItemId,
    pub fingerprint: Fingerprint,
    pub profile: ProfileName,
    /// Pane label shown in Rex.
    pub label: String,
    pub cwd: PathBuf,
    pub worktree: bool,
    /// Git ref to fetch from `origin` before creating the worktree.
    pub fetch_ref: Option<String>,
    pub prompt: String,
    pub claude_args: Vec<String>,
}

impl RunSpec {
    /// Spawners call this first so a profile pointing at a repo that isn't cloned fails loudly
    /// instead of starting claude in the wrong place.
    pub fn ensure_cwd(&self) -> Result<(), SpawnError> {
        if self.cwd.is_dir() {
            Ok(())
        } else {
            Err(SpawnError::MissingCwd(self.cwd.clone()))
        }
    }

    /// The interactive `claude` invocation for this run.
    pub fn claude_command(&self) -> CommandSpec {
        CommandSpec::new(Program::new("claude"))
            .args(&self.claude_args)
            .arg(&self.prompt)
    }
}

#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("working directory {} does not exist", .0.display())]
    MissingCwd(PathBuf),
    #[error("rex did not return a block for the new pane")]
    NoBlock,
    #[error("unexpected rex output: {0}")]
    BadOutput(String),
    #[error(transparent)]
    Exec(#[from] ExecError),
}

#[async_trait]
pub trait Spawner: Send + Sync {
    /// Start the agent and return the Rex block it runs in.
    async fn spawn(&self, run: &RunSpec, id: RunId) -> Result<BlockId, SpawnError>;
}

/// Records what would have been spawned. Used by `--dry-run` and tests.
#[derive(Default)]
pub struct DryRunSpawner {
    spawned: Mutex<Vec<RunSpec>>,
}

impl DryRunSpawner {
    #[cfg(test)]
    pub fn spawned(&self) -> Vec<RunSpec> {
        self.spawned.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl Spawner for DryRunSpawner {
    async fn spawn(&self, run: &RunSpec, _id: RunId) -> Result<BlockId, SpawnError> {
        let mut spawned = self.spawned.lock().expect("poisoned");
        spawned.push(run.clone());
        Ok(BlockId::new(format!("dry-{}", spawned.len())))
    }
}

/// A rule matched and its profile rendered (or failed to).
#[derive(Debug)]
pub struct Planned {
    pub rule: String,
    pub mode: Mode,
    pub run: Result<RunSpec, ProfileError>,
}

/// Which runs `event` calls for under the configured rules. Pure: nothing is spawned.
pub fn plan(config: &Config, renderer: &Renderer, event: &ItemEvent) -> Vec<Planned> {
    rules::route(&config.rules, event)
        .into_iter()
        .map(|route| {
            let run = match config.profiles.get(&route.rule.spawn) {
                Some(profile) => {
                    renderer.render(&route.rule.spawn, profile, route.item, route.previous)
                }
                None => unreachable!("Config::parse rejects rules naming unknown profiles"),
            };
            Planned {
                rule: route.rule.name.clone(),
                mode: route.rule.mode,
                run,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::DateTime;
    use serde_json::json;

    use super::*;
    use crate::model::{Fingerprint, Item, ItemKind, SourceId};

    fn config() -> Config {
        Config::parse(
            r#"
            [[rule]]
            name = "review"
            when = { kind = "pr.review" }
            spawn = "code-review"
            mode = "auto"

            [profile.code-review]
            skill = "code-review"
            prompt = "{{ item.url }}"
            cwd = "/work"
            claude_args = ["--permission-mode", "plan"]
            "#,
        )
        .unwrap()
    }

    fn pr() -> Item {
        Item {
            id: ItemId::new("github:r#1"),
            source: SourceId::new("github"),
            kind: ItemKind::new("pr.review"),
            title: String::new(),
            url: "https://x/1".to_owned(),
            fingerprint: Fingerprint::new("a"),
            updated_at: DateTime::UNIX_EPOCH,
            attrs: json!({}),
        }
    }

    #[test]
    fn plan_renders_matching_rules_only() {
        let renderer = Renderer::new(Path::new("/h"));
        let planned = plan(&config(), &renderer, &ItemEvent::New(pr()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].mode, Mode::Auto);
        let run = planned[0].run.as_ref().unwrap();
        assert_eq!(run.prompt, "/code-review https://x/1");

        let mut other = pr();
        other.kind = ItemKind::new("incident.exception");
        assert!(plan(&config(), &renderer, &ItemEvent::New(other)).is_empty());
    }

    #[test]
    fn claude_command_puts_flags_before_the_prompt() {
        let renderer = Renderer::new(Path::new("/h"));
        let run = plan(&config(), &renderer, &ItemEvent::New(pr()))
            .remove(0)
            .run
            .unwrap();
        assert_eq!(
            run.claude_command().to_string(),
            "claude --permission-mode plan /code-review https://x/1"
        );
    }

    #[test]
    fn ensure_cwd_rejects_missing_directories() {
        let renderer = Renderer::new(Path::new("/h"));
        let mut run = plan(&config(), &renderer, &ItemEvent::New(pr()))
            .remove(0)
            .run
            .unwrap();

        run.cwd = std::env::temp_dir();
        assert!(run.ensure_cwd().is_ok());

        run.cwd = PathBuf::from("/definitely/not/cloned");
        assert!(matches!(run.ensure_cwd(), Err(SpawnError::MissingCwd(_))));
    }

    #[tokio::test]
    async fn dry_run_spawner_records_runs() {
        let renderer = Renderer::new(Path::new("/h"));
        let run = plan(&config(), &renderer, &ItemEvent::New(pr()))
            .remove(0)
            .run
            .unwrap();
        let spawner = DryRunSpawner::default();
        let block = spawner.spawn(&run, RunId::new(1)).await.unwrap();
        assert_eq!(block.as_str(), "dry-1");
        assert_eq!(spawner.spawned(), vec![run]);
    }
}

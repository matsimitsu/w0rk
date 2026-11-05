//! Glue: reconcile a source's items, route the resulting events, and start runs.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::actions::Renderer;
use crate::actions::{self, RunSpec, Spawner};
use crate::config::Config;
use crate::model::{BlockId, ItemEvent, RunId, SourceId};
use crate::rules::Mode;
use crate::store::{Store, StoreError};

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// A rule matched in `suggest` mode: shown in the TUI, nothing started.
    Suggested { rule: String, run: RunSpec },
    Spawned {
        rule: String,
        id: RunId,
        block: BlockId,
    },
    /// This item/fingerprint/profile already has a run.
    AlreadyRan { rule: String },
    /// The run could not start (template error, repo not cloned, rex failure). Recorded as stopped
    /// when a run row exists.
    Failed { rule: String, reason: String },
}

/// Start one run, recording it first so a crash or re-poll can never spawn it twice.
pub async fn start_run(
    store: &Store,
    spawner: &dyn Spawner,
    rule: &str,
    run: &RunSpec,
    now: DateTime<Utc>,
) -> Result<Outcome, StoreError> {
    let rule = rule.to_owned();
    let Some(id) = store.create_run(&run.item_id, &run.fingerprint, &run.profile, now)? else {
        return Ok(Outcome::AlreadyRan { rule });
    };
    match spawner.spawn(run, id).await {
        Ok(block) => {
            store.set_block(id, &block, now)?;
            Ok(Outcome::Spawned { rule, id, block })
        }
        Err(err) => {
            store.fail_run(id, &err.to_string(), now)?;
            Ok(Outcome::Failed {
                rule,
                reason: err.to_string(),
            })
        }
    }
}

/// Route events through the rules: `auto` rules start runs, `suggest` rules are returned for the UI.
pub async fn dispatch(
    store: &Store,
    spawner: &dyn Spawner,
    config: &Config,
    renderer: &Renderer,
    events: &[ItemEvent],
    now: DateTime<Utc>,
) -> Result<Vec<Outcome>, StoreError> {
    let mut outcomes = Vec::new();
    for event in events {
        for planned in actions::plan(config, renderer, event) {
            let run = match planned.run {
                Ok(run) => run,
                Err(err) => {
                    outcomes.push(Outcome::Failed {
                        rule: planned.rule,
                        reason: err.to_string(),
                    });
                    continue;
                }
            };
            outcomes.push(match planned.mode {
                Mode::Suggest => Outcome::Suggested {
                    rule: planned.rule,
                    run,
                },
                Mode::Auto => start_run(store, spawner, &planned.rule, &run, now).await?,
            });
        }
    }
    Ok(outcomes)
}

/// Whether `source` has never been reconciled. The first poll of a source finds everything
/// "new"; spawning for all of it would launch an agent per pre-existing PR or incident.
pub fn is_first_poll(known_sources: &HashSet<SourceId>, source: &SourceId) -> bool {
    !known_sources.contains(source)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::DateTime;
    use serde_json::json;

    use super::*;
    use crate::actions::DryRunSpawner;
    use crate::model::{Fingerprint, Item, ItemId, ItemKind};

    fn config(mode: &str) -> Config {
        Config::parse(&format!(
            r#"
            [[rule]]
            name = "review"
            when = {{ kind = "pr.review" }}
            spawn = "p"
            mode = "{mode}"

            [profile.p]
            prompt = "{{{{ item.url }}}}"
            cwd = "{}"
            "#,
            std::env::temp_dir().display()
        ))
        .unwrap()
    }

    fn pr(fingerprint: &str) -> Item {
        Item {
            id: ItemId::new("github:r#1"),
            source: SourceId::new("github"),
            kind: ItemKind::new("pr.review"),
            title: String::new(),
            url: "https://x/1".to_owned(),
            fingerprint: Fingerprint::new(fingerprint),
            updated_at: DateTime::UNIX_EPOCH,
            attrs: json!({}),
        }
    }

    async fn run_dispatch(
        store: &Store,
        cfg: &Config,
        spawner: &DryRunSpawner,
        item: Item,
    ) -> Vec<Outcome> {
        dispatch(
            store,
            spawner,
            cfg,
            &Renderer::new(Path::new("/h")),
            &[ItemEvent::New(item)],
            DateTime::UNIX_EPOCH,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn auto_rules_spawn_and_record_the_block() {
        let store = Store::open_in_memory().unwrap();
        let spawner = DryRunSpawner::default();

        let out = run_dispatch(&store, &config("auto"), &spawner, pr("a")).await;

        assert!(matches!(out.as_slice(), [Outcome::Spawned { .. }]));
        assert_eq!(spawner.spawned().len(), 1);
        let runs = store.runs().unwrap();
        assert_eq!(runs[0].block, Some(BlockId::new("dry-1")));
    }

    #[tokio::test]
    async fn suggest_rules_start_nothing() {
        let store = Store::open_in_memory().unwrap();
        let spawner = DryRunSpawner::default();

        let out = run_dispatch(&store, &config("suggest"), &spawner, pr("a")).await;

        assert!(matches!(out.as_slice(), [Outcome::Suggested { .. }]));
        assert!(spawner.spawned().is_empty());
        assert!(store.runs().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_same_item_and_fingerprint_never_spawns_twice() {
        let store = Store::open_in_memory().unwrap();
        let spawner = DryRunSpawner::default();
        let cfg = config("auto");

        run_dispatch(&store, &cfg, &spawner, pr("a")).await;
        let again = run_dispatch(&store, &cfg, &spawner, pr("a")).await;
        let new_commits = run_dispatch(&store, &cfg, &spawner, pr("b")).await;

        assert!(matches!(again.as_slice(), [Outcome::AlreadyRan { .. }]));
        assert!(matches!(new_commits.as_slice(), [Outcome::Spawned { .. }]));
        assert_eq!(spawner.spawned().len(), 2);
    }

    #[test]
    fn first_poll_is_detected_per_source() {
        let known: HashSet<SourceId> = [SourceId::new("github")].into();
        assert!(!is_first_poll(&known, &SourceId::new("github")));
        assert!(is_first_poll(&known, &SourceId::new("appsignal:server")));
    }
}

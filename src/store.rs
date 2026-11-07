//! SQLite state: which items we've seen (so "new" fires once) and which are dismissed.
//! Runs are added in a later phase.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use thiserror::Error;

use crate::model::{
    BlockId, Fingerprint, Item, ItemEvent, ItemId, Namespace, ProfileName, Run, RunId, RunState,
    SourceId, Verdict,
};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not create {path}: {source}")]
    CreateDir {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

fn parse_time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).map_or(DateTime::UNIX_EPOCH, |t| t.with_timezone(&Utc))
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_owned(),
                source,
            })?;
        }
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS items (
                id          TEXT PRIMARY KEY,
                source      TEXT NOT NULL,
                kind        TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                first_seen  TEXT NOT NULL,
                last_seen   TEXT NOT NULL,
                gone        INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS items_by_source ON items(source);
             CREATE TABLE IF NOT EXISTS polled_sources (source TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS runs (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                item_id     TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                profile     TEXT NOT NULL,
                block       TEXT,
                state       TEXT NOT NULL,
                verdict     TEXT,
                summary     TEXT,
                cleared     INTEGER NOT NULL DEFAULT 0,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                UNIQUE (item_id, fingerprint, profile)
             );
             CREATE TABLE IF NOT EXISTS hidden_namespaces (name TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS ignored (item_id TEXT PRIMARY KEY, ignored_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS dismissals (
                item_id     TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// Diff one source's latest items against what was seen before and record the result.
    ///
    /// Only call this after a *successful* fetch: an empty list from a failed fetch would mark
    /// every item as gone.
    pub fn reconcile(
        &mut self,
        source: &SourceId,
        items: &[Item],
        now: DateTime<Utc>,
    ) -> Result<Vec<ItemEvent>, StoreError> {
        let stamp = now.to_rfc3339();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO polled_sources (source) VALUES (?1)",
            params![source.as_str()],
        )?;

        let mut known: HashMap<String, (String, bool)> = HashMap::new();
        {
            let mut stmt =
                tx.prepare("SELECT id, fingerprint, gone FROM items WHERE source = ?1")?;
            let rows = stmt.query_map(params![source.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?;
            for row in rows {
                let (id, fingerprint, gone) = row?;
                known.insert(id, (fingerprint, gone));
            }
        }

        let mut events = Vec::new();
        for item in items {
            match known.remove(item.id.as_str()) {
                None => {
                    tx.execute(
                        "INSERT INTO items (id, source, kind, fingerprint, first_seen, last_seen)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![
                            item.id.as_str(),
                            source.as_str(),
                            item.kind.as_str(),
                            item.fingerprint.as_str(),
                            stamp
                        ],
                    )?;
                    events.push(ItemEvent::New(item.clone()));
                }
                Some((previous, gone)) => {
                    tx.execute(
                        "UPDATE items SET fingerprint = ?2, last_seen = ?3, gone = 0 WHERE id = ?1",
                        params![item.id.as_str(), item.fingerprint.as_str(), stamp],
                    )?;
                    if gone {
                        events.push(ItemEvent::New(item.clone()));
                    } else if previous != item.fingerprint.as_str() {
                        events.push(ItemEvent::Changed {
                            item: item.clone(),
                            previous: Fingerprint::from(previous),
                        });
                    }
                }
            }
        }

        // Whatever is left was known but not returned this time.
        for (id, (_, gone)) in known {
            if !gone {
                tx.execute("UPDATE items SET gone = 1 WHERE id = ?1", params![id])?;
                events.push(ItemEvent::Gone(ItemId::from(id)));
            }
        }

        tx.commit()?;
        Ok(events)
    }

    /// Hide an item for good, whatever happens to it later. Unlike a dismissal, a new fingerprint
    /// does not bring it back.
    pub fn ignore(&self, id: &ItemId, now: DateTime<Utc>) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO ignored (item_id, ignored_at) VALUES (?1, ?2)",
            params![id.as_str(), now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn unignore(&self, id: &ItemId) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM ignored WHERE item_id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }

    pub fn ignored(&self) -> Result<std::collections::HashSet<ItemId>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT item_id FROM ignored")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(ItemId::from)
            .collect())
    }

    /// Hide a finished run from the AGENTS list.
    pub fn clear_run(&self, run: RunId) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET cleared = 1 WHERE id = ?1",
            params![run.get()],
        )?;
        Ok(())
    }

    /// Runs that were queued in memory when w0rk last exited can't be resumed.
    pub fn interrupt_unlaunched_runs(&self, now: DateTime<Utc>) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET state = 'stopped', summary = 'w0rk exited before this started',
                    updated_at = ?1
             WHERE state = 'queued' AND block IS NULL",
            params![now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn hidden_namespaces(&self) -> Result<std::collections::BTreeSet<Namespace>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT name FROM hidden_namespaces")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(Namespace::from)
            .collect())
    }

    pub fn set_namespace_hidden(&self, ns: &Namespace, hidden: bool) -> Result<(), StoreError> {
        if hidden {
            self.conn.execute(
                "INSERT OR IGNORE INTO hidden_namespaces (name) VALUES (?1)",
                params![ns.as_str()],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM hidden_namespaces WHERE name = ?1",
                params![ns.as_str()],
            )?;
        }
        Ok(())
    }

    pub fn dismissals(&self) -> Result<HashMap<ItemId, Fingerprint>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT item_id, fingerprint FROM dismissals")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(id, fp)| (ItemId::from(id), Fingerprint::from(fp)))
            .collect())
    }

    /// Sources that have been reconciled at least once.
    pub fn known_sources(&self) -> Result<std::collections::HashSet<SourceId>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT source FROM polled_sources")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(SourceId::from)
            .collect())
    }

    /// Reserve a run for (item, fingerprint, profile). `None` means one already exists, so the
    /// caller must not spawn again.
    pub fn create_run(
        &self,
        item: &ItemId,
        fingerprint: &Fingerprint,
        profile: &ProfileName,
        now: DateTime<Utc>,
    ) -> Result<Option<RunId>, StoreError> {
        let stamp = now.to_rfc3339();
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO runs (item_id, fingerprint, profile, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?4)",
            params![item.as_str(), fingerprint.as_str(), profile.as_str(), stamp],
        )?;
        Ok((changed == 1).then(|| RunId::new(self.conn.last_insert_rowid())))
    }

    pub fn set_block(
        &self,
        run: RunId,
        block: &BlockId,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET block = ?2, state = 'running', updated_at = ?3 WHERE id = ?1",
            params![run.get(), block.as_str(), now.to_rfc3339()],
        )?;
        Ok(())
    }

    /// A stopped run stays stopped: the user killed it, so late hook events must not revive it.
    /// A `done` run can resume (the user sent a follow-up prompt).
    pub fn set_state(
        &self,
        run: RunId,
        state: RunState,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET state = ?2, updated_at = ?3 WHERE id = ?1 AND state != 'stopped'",
            params![run.get(), state.as_str(), now.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Like [`Store::set_state`], but only when the run is currently in one of `from`.
    pub fn set_state_from(
        &self,
        run: RunId,
        state: RunState,
        from: &[RunState],
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let allowed = from
            .iter()
            .map(|s| format!("'{}'", s.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        self.conn.execute(
            &format!(
                "UPDATE runs SET state = ?2, updated_at = ?3 WHERE id = ?1 AND state IN ({allowed})"
            ),
            params![run.get(), state.as_str(), now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn report(
        &self,
        run: RunId,
        verdict: Verdict,
        summary: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET verdict = ?2, summary = ?3, updated_at = ?4 WHERE id = ?1",
            params![run.get(), verdict.as_str(), summary, now.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Mark a run that never got a pane (spawn failed) as stopped, keeping the reason.
    pub fn fail_run(&self, run: RunId, reason: &str, now: DateTime<Utc>) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE runs SET state = 'stopped', summary = ?2, updated_at = ?3 WHERE id = ?1",
            params![run.get(), reason, now.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<Run>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, item_id, fingerprint, profile, block, state, verdict, summary,
                    created_at, updated_at
             FROM runs WHERE cleared = 0 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let state: String = row.get(5)?;
            let verdict: Option<String> = row.get(6)?;
            Ok(Run {
                id: RunId::new(row.get(0)?),
                item_id: ItemId::from(row.get::<_, String>(1)?),
                fingerprint: Fingerprint::from(row.get::<_, String>(2)?),
                profile: ProfileName::from(row.get::<_, String>(3)?),
                block: row.get::<_, Option<String>>(4)?.map(BlockId::from),
                state: state.parse().unwrap_or(RunState::Stopped),
                verdict: verdict.and_then(|v| v.parse().ok()),
                summary: row.get(7)?,
                created_at: parse_time(&row.get::<_, String>(8)?),
                updated_at: parse_time(&row.get::<_, String>(9)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Hide an item until its fingerprint changes.
    pub fn dismiss(&self, id: &ItemId, fingerprint: &Fingerprint) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO dismissals (item_id, fingerprint) VALUES (?1, ?2)
             ON CONFLICT(item_id) DO UPDATE SET fingerprint = excluded.fingerprint",
            params![id.as_str(), fingerprint.as_str()],
        )?;
        Ok(())
    }

    pub fn undismiss(&self, id: &ItemId) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM dismissals WHERE item_id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ItemKind;

    fn item(id: &str, fingerprint: &str) -> Item {
        Item {
            id: ItemId::new(id),
            source: SourceId::new("github"),
            kind: ItemKind::new("pr.review"),
            title: "t".to_owned(),
            url: "u".to_owned(),
            fingerprint: Fingerprint::new(fingerprint),
            updated_at: DateTime::UNIX_EPOCH,
            attrs: serde_json::Value::Null,
        }
    }

    fn source() -> SourceId {
        SourceId::new("github")
    }

    #[test]
    fn first_poll_reports_new_then_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let items = [item("a", "1"), item("b", "1")];

        let first = store.reconcile(&source(), &items, Utc::now()).unwrap();
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|e| matches!(e, ItemEvent::New(_))));

        let second = store.reconcile(&source(), &items, Utc::now()).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn fingerprint_change_reports_previous() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .reconcile(&source(), &[item("a", "1")], Utc::now())
            .unwrap();

        let events = store
            .reconcile(&source(), &[item("a", "2")], Utc::now())
            .unwrap();
        assert_eq!(
            events,
            vec![ItemEvent::Changed {
                item: item("a", "2"),
                previous: Fingerprint::new("1")
            }]
        );
    }

    #[test]
    fn disappearing_item_is_gone_once_and_new_when_it_returns() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .reconcile(&source(), &[item("a", "1")], Utc::now())
            .unwrap();

        let gone = store.reconcile(&source(), &[], Utc::now()).unwrap();
        assert_eq!(gone, vec![ItemEvent::Gone(ItemId::new("a"))]);
        assert!(
            store
                .reconcile(&source(), &[], Utc::now())
                .unwrap()
                .is_empty()
        );

        let back = store
            .reconcile(&source(), &[item("a", "1")], Utc::now())
            .unwrap();
        assert_eq!(back, vec![ItemEvent::New(item("a", "1"))]);
    }

    #[test]
    fn an_empty_first_poll_still_counts_as_polled() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(store.known_sources().unwrap().is_empty());
        store.reconcile(&source(), &[], Utc::now()).unwrap();
        assert!(store.known_sources().unwrap().contains(&source()));
    }

    #[test]
    fn sources_do_not_affect_each_other() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .reconcile(&source(), &[item("a", "1")], Utc::now())
            .unwrap();
        let other = SourceId::new("appsignal");
        assert!(store.reconcile(&other, &[], Utc::now()).unwrap().is_empty());
    }

    fn run_args() -> (ItemId, Fingerprint, ProfileName) {
        (
            ItemId::new("a"),
            Fingerprint::new("1"),
            ProfileName::new("p"),
        )
    }

    #[test]
    fn a_run_is_created_once_per_item_fingerprint_and_profile() {
        let store = Store::open_in_memory().unwrap();
        let (item, fp, profile) = run_args();
        let now = DateTime::UNIX_EPOCH;

        assert!(
            store
                .create_run(&item, &fp, &profile, now)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .create_run(&item, &fp, &profile, now)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .create_run(&item, &Fingerprint::new("2"), &profile, now)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .create_run(&item, &fp, &ProfileName::new("q"), now)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn run_lifecycle_and_report() {
        let store = Store::open_in_memory().unwrap();
        let (item, fp, profile) = run_args();
        let now = DateTime::UNIX_EPOCH;
        let id = store
            .create_run(&item, &fp, &profile, now)
            .unwrap()
            .unwrap();
        assert_eq!(store.runs().unwrap()[0].state, RunState::Queued);

        store.set_block(id, &BlockId::new("block:1"), now).unwrap();
        store.set_state(id, RunState::Waiting, now).unwrap();
        store
            .report(id, Verdict::NeedsYou, "odd postinstall script", now)
            .unwrap();
        let run = &store.runs().unwrap()[0];
        assert_eq!(run.block, Some(BlockId::new("block:1")));
        assert_eq!(run.state, RunState::Waiting);
        assert_eq!(run.verdict, Some(Verdict::NeedsYou));
        assert_eq!(run.summary.as_deref(), Some("odd postinstall script"));
    }

    #[test]
    fn stopped_runs_are_not_revived_but_done_runs_can_resume() {
        let store = Store::open_in_memory().unwrap();
        let (item, fp, profile) = run_args();
        let now = DateTime::UNIX_EPOCH;
        let id = store
            .create_run(&item, &fp, &profile, now)
            .unwrap()
            .unwrap();
        store.set_state(id, RunState::Done, now).unwrap();
        store.set_state(id, RunState::Running, now).unwrap();
        assert_eq!(store.runs().unwrap()[0].state, RunState::Running);

        store.set_state(id, RunState::Stopped, now).unwrap();
        store.set_state(id, RunState::Running, now).unwrap();
        assert_eq!(store.runs().unwrap()[0].state, RunState::Stopped);
    }

    #[test]
    fn conditional_transition_only_applies_from_listed_states() {
        let store = Store::open_in_memory().unwrap();
        let (item, fp, profile) = run_args();
        let now = DateTime::UNIX_EPOCH;
        let id = store
            .create_run(&item, &fp, &profile, now)
            .unwrap()
            .unwrap();
        store.set_state(id, RunState::Done, now).unwrap();
        store
            .set_state_from(id, RunState::Waiting, &[RunState::Running], now)
            .unwrap();
        assert_eq!(store.runs().unwrap()[0].state, RunState::Done);
        store
            .set_state_from(
                id,
                RunState::Waiting,
                &[RunState::Done, RunState::Running],
                now,
            )
            .unwrap();
        assert_eq!(store.runs().unwrap()[0].state, RunState::Waiting);
    }

    #[test]
    fn cleared_runs_disappear_and_unlaunched_runs_are_interrupted() {
        let store = Store::open_in_memory().unwrap();
        let now = DateTime::UNIX_EPOCH;
        let a = store
            .create_run(
                &ItemId::new("a"),
                &Fingerprint::new("1"),
                &ProfileName::new("p"),
                now,
            )
            .unwrap()
            .unwrap();
        let b = store
            .create_run(
                &ItemId::new("b"),
                &Fingerprint::new("1"),
                &ProfileName::new("p"),
                now,
            )
            .unwrap()
            .unwrap();
        store.set_block(b, &BlockId::new("block:1"), now).unwrap();

        store.interrupt_unlaunched_runs(now).unwrap();
        let runs = store.runs().unwrap();
        assert_eq!(
            runs.iter().find(|r| r.id == a).unwrap().state,
            RunState::Stopped
        );
        assert_eq!(
            runs.iter().find(|r| r.id == b).unwrap().state,
            RunState::Running
        );

        store.clear_run(a).unwrap();
        assert_eq!(store.runs().unwrap().len(), 1);
    }

    #[test]
    fn hidden_namespaces_and_dismissals_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let ns = Namespace::new("admin");
        store.set_namespace_hidden(&ns, true).unwrap();
        assert!(store.hidden_namespaces().unwrap().contains(&ns));
        store.set_namespace_hidden(&ns, false).unwrap();
        assert!(store.hidden_namespaces().unwrap().is_empty());

        store
            .dismiss(&ItemId::new("a"), &Fingerprint::new("1"))
            .unwrap();
        assert_eq!(
            store.dismissals().unwrap()[&ItemId::new("a")],
            Fingerprint::new("1")
        );
    }

    #[test]
    fn failed_spawn_is_stopped_with_a_reason() {
        let store = Store::open_in_memory().unwrap();
        let (item, fp, profile) = run_args();
        let now = DateTime::UNIX_EPOCH;
        let id = store
            .create_run(&item, &fp, &profile, now)
            .unwrap()
            .unwrap();
        store.fail_run(id, "not cloned", now).unwrap();
        let run = &store.runs().unwrap()[0];
        assert_eq!(run.state, RunState::Stopped);
        assert_eq!(run.summary.as_deref(), Some("not cloned"));
    }

    #[test]
    fn ignored_items_are_remembered_until_unignored() {
        let store = Store::open_in_memory().unwrap();
        let id = ItemId::new("appsignal:AppSignal#1");
        let now = DateTime::UNIX_EPOCH;
        assert!(store.ignored().unwrap().is_empty());

        store.ignore(&id, now).unwrap();
        store.ignore(&id, now).unwrap();
        assert_eq!(store.ignored().unwrap().len(), 1);

        store.unignore(&id).unwrap();
        assert!(store.ignored().unwrap().is_empty());
    }

    #[test]
    fn dismissal_is_stored_per_fingerprint_and_can_be_undone() {
        let store = Store::open_in_memory().unwrap();
        let id = ItemId::new("a");
        assert!(store.dismissals().unwrap().is_empty());

        store.dismiss(&id, &Fingerprint::new("1")).unwrap();
        store.dismiss(&id, &Fingerprint::new("2")).unwrap();
        assert_eq!(
            store.dismissals().unwrap()[&id],
            Fingerprint::new("2"),
            "re-dismissing replaces"
        );

        store.undismiss(&id).unwrap();
        assert!(store.dismissals().unwrap().is_empty());
    }
}

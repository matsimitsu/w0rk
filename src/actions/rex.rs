//! Spawns agents as Rex panes: `rex run -s <session> ... -- env COCKPIT_RUN_ID=<id> claude ...`.
//!
//! Progress comes back through Claude Code hooks (`cockpit hook <event>`), injected per run with
//! `--settings` so the user's own settings are untouched.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{RunSpec, SpawnError, Spawner};
use crate::exec::{CliRunner, CommandSpec, Program};
use crate::model::{BlockId, RexSession, RunId};

/// Hook events cockpit listens to, and the `cockpit hook` argument for each.
/// Directory inside a repo that holds cockpit's worktrees.
pub const WORKTREES_DIR: &str = ".worktrees";

pub const HOOKS: [(&str, &str); 4] = [
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("Notification", "notification"),
    ("Stop", "stop"),
];

#[derive(Deserialize)]
struct Sessions {
    sessions: Vec<SessionEntry>,
}

#[derive(Deserialize)]
struct SessionEntry {
    label: String,
    #[serde(default)]
    session_id: String,
}

#[derive(Deserialize)]
struct Clients {
    clients: Vec<ClientEntry>,
}

#[derive(Deserialize)]
struct ClientEntry {
    client_id: String,
    info: ClientInfo,
}

#[derive(Deserialize)]
struct ClientInfo {
    kind: String,
}

#[derive(Deserialize)]
struct Created {
    block_ids: Vec<String>,
}

pub struct RexSpawner<R> {
    cli: R,
    session: RexSession,
    /// The cockpit binary the hooks call back into.
    exe: PathBuf,
    /// The state database hooks and `cockpit report` must write to (passed as `COCKPIT_DB`).
    database: PathBuf,
}

impl<R: CliRunner> RexSpawner<R> {
    pub fn new(cli: R, session: RexSession, exe: PathBuf, database: PathBuf) -> Self {
        Self {
            cli,
            session,
            exe,
            database,
        }
    }

    fn rex() -> CommandSpec {
        CommandSpec::new(Program::new("rex"))
    }

    pub fn list_sessions_spec() -> CommandSpec {
        Self::rex().args(["ls", "--json"])
    }

    pub fn new_session_spec(session: &RexSession) -> CommandSpec {
        Self::rex().args(["new", session.as_str(), "--json"])
    }

    /// Where a run's worktree lives: `<repo>/.worktrees/cockpit-run-<id>`. Inside the repo so
    /// Claude Code's trust for the repo covers it.
    pub fn worktree_path(repo: &Path, id: RunId) -> PathBuf {
        repo.join(WORKTREES_DIR).join(format!("cockpit-run-{id}"))
    }

    fn git(repo: &Path) -> CommandSpec {
        CommandSpec::new(Program::new("git")).cwd(repo)
    }

    pub fn exclude_path_spec(repo: &Path) -> CommandSpec {
        Self::git(repo).args(["rev-parse", "--git-path", "info/exclude"])
    }

    pub fn fetch_spec(repo: &Path, git_ref: &str) -> CommandSpec {
        Self::git(repo).args(["fetch", "origin", git_ref])
    }

    pub fn worktree_spec(repo: &Path, path: &Path, from_fetch: bool) -> CommandSpec {
        Self::git(repo)
            .args(["worktree", "add", "--detach"])
            .arg(path.to_string_lossy())
            .arg(if from_fetch { "FETCH_HEAD" } else { "HEAD" })
    }

    /// The `--settings` JSON that routes Claude Code hook events to `cockpit hook`.
    pub fn hook_settings(exe: &Path) -> String {
        let exe = exe.to_string_lossy().replace('\'', r"'\''");
        let hooks: serde_json::Map<_, _> = HOOKS
            .iter()
            .map(|(event, arg)| {
                let command = format!("'{exe}' hook {arg}");
                (
                    (*event).to_owned(),
                    json!([{ "hooks": [{ "type": "command", "command": command }] }]),
                )
            })
            .collect();
        json!({ "hooks": hooks }).to_string()
    }

    pub fn pane_spec(&self, run: &RunSpec, id: RunId, cwd: &Path) -> CommandSpec {
        Self::rex()
            .args(["run", "-s", self.session.as_str(), "--label", &run.label])
            .args(["--keep-open", "--focus=false", "--json", "-c"])
            .arg(cwd.to_string_lossy())
            .args(["--", "env"])
            .arg(format!("COCKPIT_RUN_ID={id}"))
            .arg(format!("COCKPIT_DB={}", self.database.display()))
            .args(["claude", "--settings"])
            .arg(Self::hook_settings(&self.exe))
            .args(&run.claude_args)
            .arg(&run.prompt)
    }

    pub fn list_clients_spec() -> CommandSpec {
        Self::rex().args(["client", "ls", "--json"])
    }

    pub fn select_session_spec(client: &str, session_id: &str) -> CommandSpec {
        Self::rex()
            .args(["-C", client, "do", "session.select"])
            .arg(format!("session_id={session_id}"))
    }

    pub fn focus_block_spec(&self, block: &BlockId) -> CommandSpec {
        Self::rex().args([
            "block",
            "focus",
            block.as_str(),
            "-s",
            self.session.as_str(),
        ])
    }

    pub fn send_spec(&self, block: &BlockId, text: &str) -> CommandSpec {
        Self::rex()
            .args(["send", "-s", self.session.as_str(), "-b", block.as_str()])
            .arg(text)
    }

    pub fn enter_spec(&self, block: &BlockId) -> CommandSpec {
        Self::rex().args([
            "send-key",
            "-s",
            self.session.as_str(),
            "-b",
            block.as_str(),
            "enter",
        ])
    }

    pub fn close_spec(&self, block: &BlockId) -> CommandSpec {
        Self::rex().args([
            "block",
            "close",
            block.as_str(),
            "-s",
            self.session.as_str(),
        ])
    }

    pub fn capture_spec(&self, block: &BlockId) -> CommandSpec {
        Self::rex().args([
            "capture",
            "-s",
            self.session.as_str(),
            "-b",
            block.as_str(),
            "--trim",
        ])
    }

    /// Bring the agent's pane to the front in the Rex app. Showing the session is best-effort:
    /// without a connected app the block is still focused.
    pub async fn focus(&self, block: &BlockId) -> Result<(), SpawnError> {
        let _ = self.show_session().await;
        self.cli.run(&self.focus_block_spec(block)).await?;
        Ok(())
    }

    async fn show_session(&self) -> Result<(), SpawnError> {
        let sessions = Self::list_sessions_spec();
        let listing: Sessions = self.cli.run(&sessions).await?.json(&sessions)?;
        let session = listing
            .sessions
            .into_iter()
            .find(|s| s.label == self.session.as_str())
            .ok_or_else(|| SpawnError::BadOutput("session not found".to_owned()))?;
        let clients = Self::list_clients_spec();
        let listing: Clients = self.cli.run(&clients).await?.json(&clients)?;
        let client = listing
            .clients
            .into_iter()
            .find(|c| c.info.kind == "app")
            .ok_or_else(|| SpawnError::BadOutput("no Rex app connected".to_owned()))?;
        self.cli
            .run(&Self::select_session_spec(
                &client.client_id,
                &session.session_id,
            ))
            .await?;
        Ok(())
    }

    /// Type an answer into the agent's pane and press Enter.
    pub async fn reply(&self, block: &BlockId, text: &str) -> Result<(), SpawnError> {
        self.cli.run(&self.send_spec(block, text)).await?;
        self.cli.run(&self.enter_spec(block)).await?;
        Ok(())
    }

    pub async fn close(&self, block: &BlockId) -> Result<(), SpawnError> {
        self.cli.run(&self.close_spec(block)).await?;
        Ok(())
    }

    pub async fn capture(&self, block: &BlockId) -> Result<String, SpawnError> {
        Ok(self
            .cli
            .run(&self.capture_spec(block))
            .await?
            .as_str()
            .to_owned())
    }

    async fn ensure_session(&self) -> Result<(), SpawnError> {
        let spec = Self::list_sessions_spec();
        let listing: Sessions = self.cli.run(&spec).await?.json(&spec)?;
        if listing
            .sessions
            .iter()
            .any(|s| s.label == self.session.as_str())
        {
            return Ok(());
        }
        self.cli.run(&Self::new_session_spec(&self.session)).await?;
        Ok(())
    }

    /// Keep `.worktrees/` out of `git status` via the repo-local exclude file (never committed).
    async fn ignore_worktrees(&self, repo: &Path) -> Result<(), SpawnError> {
        let spec = Self::exclude_path_spec(repo);
        let out = self.cli.run(&spec).await?;
        let exclude = repo.join(out.as_str().trim());
        let entry = format!("/{WORKTREES_DIR}/");
        let current = std::fs::read_to_string(&exclude).unwrap_or_default();
        if current.lines().any(|line| line == entry) {
            return Ok(());
        }
        let io_err =
            |e: std::io::Error| SpawnError::BadOutput(format!("{}: {e}", exclude.display()));
        if let Some(dir) = exclude.parent() {
            std::fs::create_dir_all(dir).map_err(io_err)?;
        }
        let sep = if current.is_empty() || current.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        std::fs::write(&exclude, format!("{current}{sep}{entry}\n")).map_err(io_err)
    }

    async fn make_worktree(&self, run: &RunSpec, id: RunId) -> Result<PathBuf, SpawnError> {
        let path = Self::worktree_path(&run.cwd, id);
        self.ignore_worktrees(&run.cwd).await?;
        if let Some(git_ref) = &run.fetch_ref {
            self.cli.run(&Self::fetch_spec(&run.cwd, git_ref)).await?;
        }
        self.cli
            .run(&Self::worktree_spec(
                &run.cwd,
                &path,
                run.fetch_ref.is_some(),
            ))
            .await?;
        Ok(path)
    }
}

#[async_trait]
impl<R: CliRunner> Spawner for RexSpawner<R> {
    async fn spawn(&self, run: &RunSpec, id: RunId) -> Result<BlockId, SpawnError> {
        run.ensure_cwd()?;
        self.ensure_session().await?;
        let cwd = if run.worktree {
            self.make_worktree(run, id).await?
        } else {
            run.cwd.clone()
        };
        let spec = self.pane_spec(run, id, &cwd);
        let created: Created = self.cli.run(&spec).await?.json(&spec)?;
        created
            .block_ids
            .into_iter()
            .next()
            .map(BlockId::from)
            .ok_or(SpawnError::NoBlock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::testing::FakeRunner;
    use crate::model::{Fingerprint, ItemId, ProfileName};

    fn run(cwd: PathBuf, worktree: bool, fetch_ref: Option<&str>) -> RunSpec {
        RunSpec {
            item_id: ItemId::new("github:r#1"),
            fingerprint: Fingerprint::new("abc"),
            profile: ProfileName::new("code-review"),
            label: "github:r#1".to_owned(),
            cwd,
            worktree,
            fetch_ref: fetch_ref.map(str::to_owned),
            prompt: "/code-review https://x/1".to_owned(),
            claude_args: vec!["--permission-mode".to_owned(), "plan".to_owned()],
        }
    }

    fn spawner(cli: FakeRunner) -> RexSpawner<FakeRunner> {
        RexSpawner::new(
            cli,
            RexSession::new("cockpit"),
            PathBuf::from("/bin/cockpit"),
            PathBuf::from("/state/c.db"),
        )
    }

    const BLOCK: &str = r#"{"window_id":"window:1","block_ids":["block:42"],"revision":2}"#;

    #[test]
    fn hook_settings_cover_every_event_and_quote_the_path() {
        let settings: serde_json::Value = serde_json::from_str(
            &RexSpawner::<FakeRunner>::hook_settings(Path::new("/o'k/cockpit")),
        )
        .unwrap();
        for (event, arg) in HOOKS {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert_eq!(command, format!(r"'/o'\''k/cockpit' hook {arg}"));
        }
    }

    #[test]
    fn worktree_lives_inside_the_checkout() {
        let path = RexSpawner::<FakeRunner>::worktree_path(
            Path::new("/w/appsignal-server"),
            RunId::new(7),
        );
        assert_eq!(
            path,
            Path::new("/w/appsignal-server/.worktrees/cockpit-run-7")
        );
    }

    #[test]
    fn pane_command_passes_run_id_settings_flags_and_prompt() {
        let s = spawner(FakeRunner::new());
        let text = s
            .pane_spec(
                &run("/tmp".into(), false, None),
                RunId::new(3),
                Path::new("/tmp"),
            )
            .to_string();
        assert!(text.starts_with("rex run -s cockpit --label github:r#1 --keep-open --focus=false --json -c /tmp -- env COCKPIT_RUN_ID=3 COCKPIT_DB=/state/c.db claude --settings "));
        assert!(text.ends_with("--permission-mode plan /code-review https://x/1"));
    }

    #[tokio::test]
    async fn spawns_in_an_existing_session() {
        let dir = std::env::temp_dir();
        let r = run(dir.clone(), false, None);
        let s = spawner(FakeRunner::new());
        let cli = FakeRunner::new()
            .respond(
                RexSpawner::<FakeRunner>::list_sessions_spec(),
                r#"{"sessions":[{"label":"cockpit"}]}"#,
            )
            .respond(s.pane_spec(&r, RunId::new(1), &dir), BLOCK);
        let spawner = spawner(cli);

        let block = spawner.spawn(&r, RunId::new(1)).await.unwrap();

        assert_eq!(block.as_str(), "block:42");
        let calls: Vec<String> = spawner
            .cli
            .calls()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            !calls.iter().any(|c| c.starts_with("rex new")),
            "session already existed"
        );
    }

    #[tokio::test]
    async fn creates_the_session_when_missing() {
        let dir = std::env::temp_dir();
        let r = run(dir.clone(), false, None);
        let cli = FakeRunner::new()
            .respond(
                RexSpawner::<FakeRunner>::list_sessions_spec(),
                r#"{"sessions":[]}"#,
            )
            .respond(
                RexSpawner::<FakeRunner>::new_session_spec(&RexSession::new("cockpit")),
                "{}",
            )
            .respond(
                spawner(FakeRunner::new()).pane_spec(&r, RunId::new(1), &dir),
                BLOCK,
            );
        let spawner = spawner(cli);

        spawner.spawn(&r, RunId::new(1)).await.unwrap();

        assert!(
            spawner
                .cli
                .calls()
                .iter()
                .any(|c| c.to_string() == "rex new cockpit --json")
        );
    }

    #[tokio::test]
    async fn worktree_runs_fetch_then_add_then_open_the_pane_inside_it() {
        let repo = std::env::temp_dir().join(format!("cockpit-wt-test-{}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let r = run(repo.clone(), true, Some("pull/1/head"));
        let wt = RexSpawner::<FakeRunner>::worktree_path(&repo, RunId::new(9));
        let cli = FakeRunner::new()
            .respond(
                RexSpawner::<FakeRunner>::list_sessions_spec(),
                r#"{"sessions":[{"label":"cockpit"}]}"#,
            )
            .respond(
                RexSpawner::<FakeRunner>::exclude_path_spec(&repo),
                ".git/info/exclude\n",
            )
            .respond(
                RexSpawner::<FakeRunner>::fetch_spec(&repo, "pull/1/head"),
                "",
            )
            .respond(
                RexSpawner::<FakeRunner>::worktree_spec(&repo, &wt, true),
                "",
            )
            .respond(
                spawner(FakeRunner::new()).pane_spec(&r, RunId::new(9), &wt),
                BLOCK,
            );
        let spawner = spawner(cli);

        spawner.spawn(&r, RunId::new(9)).await.unwrap();

        let calls: Vec<String> = spawner
            .cli
            .calls()
            .iter()
            .map(ToString::to_string)
            .collect();
        let fetch = calls
            .iter()
            .position(|c| c.starts_with("git fetch origin pull/1/head"))
            .unwrap();
        let add = calls
            .iter()
            .position(|c| c.starts_with("git worktree add --detach"))
            .unwrap();
        assert!(fetch < add);

        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude, "/.worktrees/\n");
        // A second run must not duplicate the entry.
        spawner.ignore_worktrees(&repo).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap(),
            "/.worktrees/\n"
        );
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[tokio::test]
    async fn focus_shows_the_session_in_the_app_then_focuses_the_block() {
        let s = spawner(FakeRunner::new());
        let block = BlockId::new("block:1");
        let cli = FakeRunner::new()
            .respond(
                RexSpawner::<FakeRunner>::list_sessions_spec(),
                r#"{"sessions":[{"session_id":"session:9","label":"cockpit"}]}"#,
            )
            .respond(
                RexSpawner::<FakeRunner>::list_clients_spec(),
                r#"{"clients":[{"client_id":"client:cli","info":{"kind":"cli"}},{"client_id":"client:app","info":{"kind":"app"}}]}"#,
            )
            .respond(RexSpawner::<FakeRunner>::select_session_spec("client:app", "session:9"), "")
            .respond(s.focus_block_spec(&block), "");
        let spawner = spawner(cli);

        spawner.focus(&block).await.unwrap();

        let calls: Vec<String> = spawner
            .cli
            .calls()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(calls.last().unwrap(), "rex block focus block:1 -s cockpit");
        assert!(
            calls.contains(&"rex -C client:app do session.select session_id=session:9".to_owned())
        );
    }

    #[tokio::test]
    async fn focus_still_works_without_an_app_client() {
        let s = spawner(FakeRunner::new());
        let block = BlockId::new("block:1");
        let cli = FakeRunner::new()
            .respond(
                RexSpawner::<FakeRunner>::list_sessions_spec(),
                r#"{"sessions":[{"session_id":"s","label":"cockpit"}]}"#,
            )
            .respond(
                RexSpawner::<FakeRunner>::list_clients_spec(),
                r#"{"clients":[]}"#,
            )
            .respond(s.focus_block_spec(&block), "");
        assert!(spawner(cli).focus(&block).await.is_ok());
    }

    #[tokio::test]
    async fn reply_types_the_answer_then_presses_enter() {
        let s = spawner(FakeRunner::new());
        let block = BlockId::new("block:1");
        let cli = FakeRunner::new()
            .respond(s.send_spec(&block, "y"), "")
            .respond(s.enter_spec(&block), "");
        let spawner = spawner(cli);
        spawner.reply(&block, "y").await.unwrap();
        let calls: Vec<String> = spawner
            .cli
            .calls()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            calls,
            [
                "rex send -s cockpit -b block:1 y",
                "rex send-key -s cockpit -b block:1 enter"
            ]
        );
    }

    #[tokio::test]
    async fn missing_checkout_never_reaches_rex() {
        let r = run("/definitely/not/cloned".into(), false, None);
        let spawner = spawner(FakeRunner::new());
        assert!(matches!(
            spawner.spawn(&r, RunId::new(1)).await,
            Err(SpawnError::MissingCwd(_))
        ));
        assert!(spawner.cli.calls().is_empty());
    }
}

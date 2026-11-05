//! `~/.config/cockpit/config.toml`: hand-edited, no config UI.
//!
//! Rules, profiles and sections are added to this struct as those systems land. Unknown keys are
//! rejected so typos surface instead of silently doing nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use std::collections::BTreeMap;

use crate::actions::{Profile, ProfileError, Renderer};
use crate::model::{AppName, Environment, Namespace, Owner, ProfileName, RepoSlug, RexSession};
use crate::rules::{Predicate, Rule};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("rule `{rule}` spawns unknown profile `{profile}`")]
    UnknownProfile { rule: String, profile: ProfileName },
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error("$HOME is not set")]
    NoHome,
}

/// Seconds between polls of one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct PollSeconds(u64);

impl Default for PollSeconds {
    fn default() -> Self {
        Self(60)
    }
}

impl PollSeconds {
    pub fn duration(self) -> Duration {
        Duration::from_secs(self.0)
    }
}

fn default_poll() -> PollSeconds {
    PollSeconds::default()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubRole {
    ReviewRequested,
    Assignee,
    Author,
}

impl GithubRole {
    /// The `gh search prs` flag that selects this role.
    pub fn search_flag(&self) -> &'static str {
        match self {
            Self::ReviewRequested => "--review-requested",
            Self::Assignee => "--assignee",
            Self::Author => "--author",
        }
    }
}

/// One `gh search prs` query. Its `kind` becomes the `ItemKind` of every PR it returns.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubQuery {
    pub name: String,
    pub kind: String,
    pub role: GithubRole,
    /// Who the role applies to; defaults to `@me`. Use `app/dependabot` with role `author`.
    #[serde(default = "me")]
    pub who: String,
    /// Only PRs in these organizations/users. Empty means any.
    #[serde(default)]
    pub owners: Vec<Owner>,
    /// Only PRs in these repositories. Empty means any.
    #[serde(default)]
    pub repos: Vec<RepoSlug>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub exclude_labels: Vec<String>,
    #[serde(default)]
    pub include_drafts: bool,
}

fn me() -> String {
    "@me".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(default = "default_poll")]
    pub poll_seconds: PollSeconds,
    #[serde(default, rename = "query")]
    pub queries: Vec<GithubQuery>,
}

/// AppSignal incident states, as `appsignal-cli --state` spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum IncidentState {
    Open,
    Wip,
    Closed,
}

impl IncidentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Wip => "WIP",
            Self::Closed => "CLOSED",
        }
    }
}

fn default_states() -> Vec<IncidentState> {
    vec![IncidentState::Open]
}

/// Hours to look back on the very first run, when there is no previous invocation to compare to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct LookbackHours(i64);

impl LookbackHours {
    pub fn get(self) -> i64 {
        self.0
    }
}

impl Default for LookbackHours {
    fn default() -> Self {
        Self(24)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppsignalApp {
    /// Which incident states to list. Several states are fetched separately and merged.
    #[serde(default = "default_states")]
    pub states: Vec<IncidentState>,
    /// Only list incidents created since cockpit last ran. Incidents without a creation time are
    /// treated as old.
    #[serde(default)]
    pub new_since_last_run: bool,
    pub name: AppName,
    #[serde(default = "production")]
    pub environment: Environment,
    /// Only incidents in these namespaces are polled. Empty means all namespaces.
    #[serde(default)]
    pub namespaces: Vec<Namespace>,
}

fn production() -> Environment {
    Environment::new("production")
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppsignalConfig {
    /// Organization slug, used only to build deep links to incidents.
    pub org: Option<String>,
    #[serde(default = "default_poll")]
    pub poll_seconds: PollSeconds,
    /// How far back "new since last run" looks the first time cockpit runs.
    #[serde(default)]
    pub first_run_lookback_hours: LookbackHours,
    #[serde(default, rename = "app")]
    pub apps: Vec<AppsignalApp>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcesConfig {
    pub github: Option<GithubConfig>,
    pub appsignal: Option<AppsignalConfig>,
}

/// How many agents may be running or waiting at once; further runs queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct MaxRunning(usize);

impl MaxRunning {
    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for MaxRunning {
    fn default() -> Self {
        Self(4)
    }
}

/// A titled list in the TUI holding the items its predicate matches.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    pub title: String,
    #[serde(default)]
    pub when: Predicate,
}

impl SectionConfig {
    /// Used when the config defines no `[[section]]`.
    pub fn defaults() -> Vec<Self> {
        let parsed: Result<Config, _> = Config::parse(
            r#"
            [[section]]
            title = "TO REVIEW"
            when = { kind = { glob = "pr.*" }, not = { kind = "pr.mine" } }

            [[section]]
            title = "MY PULL REQUESTS"
            when = { kind = "pr.mine" }

            [[section]]
            title = "INCIDENTS"
            when = { kind = { glob = "incident.*" } }
            "#,
        );
        parsed.map(|c| c.sections).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RexConfig {
    #[serde(default)]
    pub max_running: MaxRunning,
    /// Rex session agents run in; created on first spawn if missing.
    #[serde(default = "default_session")]
    pub session: RexSession,
}

fn default_session() -> RexSession {
    RexSession::new("cockpit")
}

impl Default for RexConfig {
    fn default() -> Self {
        Self {
            session: default_session(),
            max_running: MaxRunning::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub rex: RexConfig,
    #[serde(default)]
    pub source: SourcesConfig,
    #[serde(default, rename = "section")]
    pub sections: Vec<SectionConfig>,
    #[serde(default, rename = "rule")]
    pub rules: Vec<Rule>,
    #[serde(default, rename = "profile")]
    pub profiles: BTreeMap<ProfileName, Profile>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Cross-reference checks serde can't do: rules name real profiles and templates parse.
    fn validate(&self) -> Result<(), ConfigError> {
        for rule in &self.rules {
            if !self.profiles.contains_key(&rule.spawn) {
                return Err(ConfigError::UnknownProfile {
                    rule: rule.name.clone(),
                    profile: rule.spawn.clone(),
                });
            }
        }
        let renderer = Renderer::new(Path::new(""));
        for (name, profile) in &self.profiles {
            renderer.check(name, profile)?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text)
    }
}

/// Where cockpit keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub database: PathBuf,
}

impl Paths {
    pub fn from_home(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
            config: home.join(".config/cockpit/config.toml"),
            database: home.join(".local/state/cockpit/cockpit.db"),
        }
    }

    /// Use a different config file, keeping everything else where it is.
    #[must_use]
    pub fn with_config(mut self, config: PathBuf) -> Self {
        self.config = config;
        self
    }

    #[must_use]
    pub fn with_database(mut self, database: PathBuf) -> Self {
        self.database = database;
        self
    }

    pub fn discover() -> Result<Self, ConfigError> {
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoHome)?;
        Ok(Self::from_home(Path::new(&home)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[source.github]
poll_seconds = 45

[[source.github.query]]
name = "review-requests"
kind = "pr.review"
role = "review-requested"
repos = ["appsignal/appsignal-server"]
exclude_labels = ["wip"]

[[source.github.query]]
name = "dependabot"
kind = "pr.dependabot"
role = "author"
who = "app/dependabot"

[[source.appsignal.app]]
name = "AppSignal"
namespaces = ["web", "background"]
"#;

    #[test]
    fn parses_sources() {
        let config = Config::parse(SAMPLE).unwrap();
        let github = config.source.github.unwrap();
        assert_eq!(github.poll_seconds.duration(), Duration::from_secs(45));
        assert_eq!(github.queries.len(), 2);
        assert_eq!(github.queries[0].who, "@me");
        assert_eq!(github.queries[0].role.search_flag(), "--review-requested");
        assert!(!github.queries[0].include_drafts);
        assert_eq!(github.queries[1].who, "app/dependabot");

        let appsignal = config.source.appsignal.unwrap();
        assert_eq!(appsignal.poll_seconds.duration(), Duration::from_secs(60));
        assert_eq!(appsignal.apps[0].environment.as_str(), "production");
        assert_eq!(appsignal.apps[0].namespaces.len(), 2);
    }

    #[test]
    fn rules_must_name_existing_profiles() {
        let err = Config::parse("[[rule]]\nname = \"r\"\nspawn = \"nope\"").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownProfile { .. }));
    }

    #[test]
    fn profile_template_errors_surface_at_load() {
        let err = Config::parse("[profile.p]\nprompt = \"{{\"\ncwd = \"/\"").unwrap_err();
        assert!(matches!(err, ConfigError::Profile(_)));
    }

    #[test]
    fn default_sections_split_review_requests_own_prs_and_incidents() {
        let sections = SectionConfig::defaults();
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["TO REVIEW", "MY PULL REQUESTS", "INCIDENTS"]);
    }

    #[test]
    fn empty_config_is_valid() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn typos_are_rejected() {
        assert!(Config::parse("[source.github]\npoll_secs = 1").is_err());
    }

    #[test]
    fn paths_live_under_home() {
        let paths = Paths::from_home(Path::new("/home/me"));
        assert_eq!(
            paths.config,
            Path::new("/home/me/.config/cockpit/config.toml")
        );
        assert_eq!(
            paths.database,
            Path::new("/home/me/.local/state/cockpit/cockpit.db")
        );
    }
}

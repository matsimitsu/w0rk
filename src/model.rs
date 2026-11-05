//! Domain types shared by every system (RUST-1: newtypes over primitives).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        #[allow(dead_code)] // every newtype gets the same API, used or not
        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

string_newtype!(
    /// Stable identity of an item, e.g. `github:appsignal/appsignal-server#18350`.
    ItemId
);
string_newtype!(
    /// Which source produced an item, e.g. `github` or `appsignal:server`.
    SourceId
);
string_newtype!(
    /// What the item is, e.g. `pr.review` or `incident.exception`. Rules and views key on this.
    ItemKind
);
string_newtype!(
    /// Changes whenever the item changed in a way worth reacting to (PR head sha, incident state).
    Fingerprint
);
string_newtype!(
    /// `owner/name` of a GitHub repository.
    RepoSlug
);
string_newtype!(
    /// A GitHub user or organization, e.g. `appsignal`.
    Owner
);
string_newtype!(
    /// AppSignal application name as listed by `appsignal-cli apps list`.
    AppName
);
string_newtype!(
    /// AppSignal environment, e.g. `production`.
    Environment
);
string_newtype!(
    /// AppSignal namespace, e.g. `web` or `background`.
    Namespace
);
string_newtype!(
    /// Name of a `[profile.*]` entry in the config.
    ProfileName
);
string_newtype!(
    /// Label of the Rex session agents run in.
    RexSession
);
string_newtype!(
    /// A Rex block (terminal pane) id.
    BlockId
);

/// Row id of a spawned agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(i64);

impl RunId {
    pub fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A normalized notification from any source. Everything downstream (store, rules, views) only sees this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub source: SourceId,
    pub kind: ItemKind,
    pub title: String,
    pub url: String,
    pub fingerprint: Fingerprint,
    pub updated_at: DateTime<Utc>,
    /// Source-specific fields (repo, author, ci, namespace, count, ...), addressable by rules via JSON pointer.
    pub attrs: serde_json::Value,
}

/// What changed for an item between two polls of its source.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemEvent {
    New(Item),
    Changed { item: Item, previous: Fingerprint },
    Gone(ItemId),
}

/// Lifecycle of a spawned agent, as shown in the AGENTS section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    /// The agent is waiting for an answer from the user.
    Waiting,
    Done,
    Stopped,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Stopped => "stopped",
        }
    }

    /// Whether the agent can still change state on its own.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Waiting)
    }
}

impl std::str::FromStr for RunState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waiting" => Ok(Self::Waiting),
            "done" => Ok(Self::Done),
            "stopped" => Ok(Self::Stopped),
            other => Err(format!("unknown run state `{other}`")),
        }
    }
}

/// What the agent concluded, reported via `cockpit report`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Ok,
    NeedsYou,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NeedsYou => "needs-you",
        }
    }
}

impl std::str::FromStr for Verdict {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ok" => Ok(Self::Ok),
            "needs-you" => Ok(Self::NeedsYou),
            other => Err(format!("unknown verdict `{other}` (use ok or needs-you)")),
        }
    }
}

/// One spawned agent, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub id: RunId,
    pub item_id: ItemId,
    pub fingerprint: Fingerprint,
    pub profile: ProfileName,
    pub block: Option<BlockId>,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

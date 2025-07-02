mod slack;
use base::{Config, Workspace};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("No today found")]
    NoToday,
}

pub struct Syncer<'a> {
    config: &'a Config,
    workspace: &'a Workspace,
    state_dir: PathBuf,
}

impl<'a> Syncer<'a> {
    pub fn new(
        config: &'a Config,
        state_dir: &Path,
        workspace: &'a Workspace,
    ) -> Result<Self, SyncError> {
        fs::create_dir_all(state_dir)?;

        Ok(Self {
            config,
            workspace,
            state_dir: state_dir.into(),
        })
    }

    pub async fn sync(&self) -> Result<(), SyncError> {
        let today = match self.workspace.today() {
            Some(today) => today,
            None => {
                return Err(SyncError::NoToday);
            }
        };

        if let Some(slack_config) = &self.config.slack {
            let mut slack =
                slack::Slack::new(&self.state_dir, &slack_config.token, &slack_config.channel)?;
            slack.sync_message(today).await?;
        }

        Ok(())
    }
}

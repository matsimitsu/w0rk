//! Sources turn CLI output into normalized [`Item`]s. A new source is one adapter here plus config.

mod appsignal;
mod github;

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

pub use appsignal::AppsignalSource;
pub use github::GithubSource;

use crate::config::Config;
use crate::exec::{CliRunner, ExecError};
use crate::model::{Item, SourceId};

#[derive(Debug, Error)]
pub enum SourceError {
    #[error(transparent)]
    Exec(#[from] ExecError),
}

#[async_trait]
pub trait Source: Send + Sync {
    /// Items from one source are reconciled together, so a failed fetch must return `Err`, never a
    /// partial list.
    fn id(&self) -> SourceId;
    fn poll_every(&self) -> Duration;
    async fn fetch(&self, cli: &dyn CliRunner) -> Result<Vec<Item>, SourceError>;
}

/// Build every source the config asks for.
pub fn build(config: &Config) -> Vec<Box<dyn Source>> {
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    if let Some(github) = &config.source.github {
        sources.push(Box::new(GithubSource::new(github.clone())));
    }
    if let Some(appsignal) = &config.source.appsignal {
        for app in &appsignal.apps {
            sources.push(Box::new(AppsignalSource::new(
                app.clone(),
                appsignal.org.clone(),
                appsignal.poll_seconds,
            )));
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_makes_one_github_source_and_one_per_appsignal_app() {
        let config = Config::parse(
            r#"
            [source.github]
            [[source.github.query]]
            name = "r"
            kind = "pr.review"
            role = "review-requested"

            [[source.appsignal.app]]
            name = "AppSignal"
            [[source.appsignal.app]]
            name = "PublicApi"
            "#,
        )
        .unwrap();

        let ids: Vec<String> = build(&config).iter().map(|s| s.id().to_string()).collect();
        assert_eq!(
            ids,
            ["github", "appsignal:AppSignal", "appsignal:PublicApi"]
        );
    }
}

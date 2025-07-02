pub use config::Config;
pub use day::Day;
pub use task::{State as TaskState, Task};
use thiserror::Error;
pub use workspace::Workspace;

mod config;
mod day;
mod recurring_task;
mod task;
mod workspace;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Time parse error: {0}")]
    TimeParse(#[from] time::error::Parse),
    #[error("Time format error: {0}")]
    TimeFormat(#[from] time::error::Format),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Error while parsing: \"{0}\". Expected format: \"* [] @<interval> <name>\"")]
    InvalidRecurringTaskSyntax(String),
    #[error("Error while parsing: \"{0}\". Expected format: \"* [] <name>\"")]
    InvalidTaskSyntax(String),
    #[error("Error while parsing interval: \"{0}\". Expected one of: [daily, weekly, monthly, weekday, weekend]")]
    InvalidIntervalSyntax(String),
    #[error("Invalid workspace name: \"{0}\"")]
    InvalidWorkspaceName(String),
    #[error("Workspace is not a directory")]
    WorkspaceIsNotDirectory,
    #[error("Invalid day path: \"{0}\"")]
    InvalidDayPath(String),
    #[error("Day already exists: {0}")]
    DayAlreadyExists(String),
}

#[cfg(test)]
mod tests {

    pub mod helpers {
        use std::env::current_dir;
        use std::path::PathBuf;

        pub fn test_fixtures_path() -> PathBuf {
            let current_dir = current_dir().expect("Could not get current dir");
            // Check for two levels of nesting
            if current_dir.join("../test_fixtures").exists() {
                current_dir.join("../test_fixtures")
            } else {
                current_dir.join("../../test_fixtures")
            }
        }
    }
}

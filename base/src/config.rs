use lazy_static::lazy_static;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use time::format_description::{parse_owned, OwnedFormatItem};

pub const RECURRING_FILE: &str = ".recurring.md";
pub const DAY_EXTENTION: &str = "md";

lazy_static! {
    pub static ref DAY_FORMAT: OwnedFormatItem = parse_owned::<2>("[year]-[month]-[day]").unwrap();
}

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub work_dir: PathBuf,
    pub slack: Option<SlackConfig>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SlackConfig {
    pub token: String,
    pub channel: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            work_dir: "./work_dir".into(),
            slack: None,
        }
    }
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self, crate::Error> {
        let config_file = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&config_file)?;
        Ok(config)
    }
}

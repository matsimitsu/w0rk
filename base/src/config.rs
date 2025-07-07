use lazy_static::lazy_static;
use regex::Regex;
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
    #[serde(default)]
    pub rewrites: Vec<Rewrite>,
}

#[derive(Debug, Clone)]
pub struct Rewrite {
    pub from: Regex,
    pub to: String,
}

impl<'de> Deserialize<'de> for Rewrite {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper {
            from: String,
            to: String,
        }

        let helper = Helper::deserialize(deserializer)?;

        // Validate that the regex compiles
        let from = match Regex::new(&helper.from) {
            Ok(regex) => regex,
            Err(e) => {
                return Err(serde::de::Error::custom(format!(
                    "Invalid regex pattern '{}': {}",
                    helper.from, e
                )));
            }
        };

        Ok(Rewrite {
            from,
            to: helper.to,
        })
    }
}

impl Rewrite {
    pub fn rewrite(&self, text: &mut String) {
        *text = self.from.replace_all(text, &self.to).to_string();
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite() {
        let mut text = String::from("Skip validations when setting removing flag on site #13462");
        let rewrite = Rewrite {
            from: Regex::new(r"#(\d+)").unwrap(),
            to: "github.com/foo/$1".to_string(),
        };
        rewrite.rewrite(&mut text);
        assert_eq!(
            text,
            "Skip validations when setting removing flag on site github.com/foo/13462"
        );
    }

    #[test]
    fn test_rewrite_multiple() {
        let mut text = String::from("test #13462 and #13463");
        let rewrite = Rewrite {
            from: Regex::new(r"#(\d+)").unwrap(),
            to: "github.com/$1".to_string(),
        };
        rewrite.rewrite(&mut text);
        assert_eq!(text, "test github.com/13462 and github.com/13463");
    }
}

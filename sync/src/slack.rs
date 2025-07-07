use super::SyncError;
use base::{Day, Rewrite, TaskState};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use time::Date;

pub trait SlackMessage {
    fn to_message(&self, rewrites: &[Rewrite]) -> String;
    fn date(&self) -> Date;
}

pub trait SlackEmoji {
    fn to_emoji(&self) -> String;
}

impl SlackEmoji for TaskState {
    fn to_emoji(&self) -> String {
        match self {
            TaskState::Blocked => ":todo_paused:",
            TaskState::Completed => ":todo_done:",
            TaskState::InProgress => ":todo_doing:",
            TaskState::Incomplete => ":todo:",
        }
        .to_string()
    }
}

impl SlackMessage for Day {
    fn to_message(&self, rewrites: &[Rewrite]) -> String {
        let mut text = "".to_string();

        for task in &self.tasks {
            if task.subtasks.is_empty() {
                text.push_str(&format!(
                    "{} {}\n",
                    task.state.to_emoji(),
                    rewrite_name(&task.name, rewrites)
                ));
            } else {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("*{}*\n", task.name));
                for subtask in &task.subtasks {
                    text.push_str(&format!(
                        "{} {}\n",
                        subtask.state.to_emoji(),
                        rewrite_name(&subtask.name, rewrites)
                    ));
                }
                text.push('\n');
            }
        }
        text
    }

    fn date(&self) -> Date {
        self.date
    }
}

fn rewrite_name(name: &str, rewrites: &[Rewrite]) -> String {
    let mut name = name.to_string();
    for rewrite in rewrites {
        rewrite.rewrite(&mut name);
    }
    name
}

pub type SlackSyncState = Vec<SlackDayState>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SlackDayState {
    pub channel_id: String,
    pub ts: String,
    pub date: Date,
}

pub struct Slack {
    client: reqwest::Client,
    channel_id: String,
    token: String,
    state_path: PathBuf,
    state: SlackSyncState,
}

#[derive(Deserialize, Debug)]
pub struct Response {
    pub ok: bool,
    #[allow(dead_code)]
    pub error: Option<String>,
    pub ts: Option<String>,
}

impl Slack {
    pub fn new(state_dir: &Path, token: &str, channel_id: &str) -> Result<Self, SyncError> {
        let state_path = state_dir.join("slack.json");

        let state = match Path::new(&state_path).exists() {
            true => {
                let state_file = std::fs::read_to_string(&state_path)?;
                serde_json::from_str(&state_file)?
            }
            false => Vec::new(),
        };

        Ok(Self {
            client: reqwest::Client::new(),
            channel_id: channel_id.to_string(),
            token: token.to_string(),
            state_path,
            state,
        })
    }

    fn write_state(&self) -> Result<(), SyncError> {
        let state_file = std::fs::File::create(&self.state_path)?;
        serde_json::to_writer(state_file, &self.state)?;
        Ok(())
    }

    async fn post(
        &self,
        path: &str,
        content: serde_json::Value,
    ) -> Result<Response, reqwest::Error> {
        self.client
            .request(reqwest::Method::POST, path)
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer ".to_string() + &self.token)
            .json(&content)
            .send()
            .await?
            .json::<Response>()
            .await
    }

    pub async fn sync_message<M>(
        &mut self,
        message: M,
        rewrites: &[Rewrite],
    ) -> Result<(), SyncError>
    where
        M: SlackMessage,
    {
        let date = message.date();
        let state = self.state.iter().find(|state| state.date == date);
        let text = message.to_message(rewrites);

        match state {
            Some(state) => {
                self.update_message(state.ts.to_owned(), text).await?;
            }
            None => {
                let result = self.send_message(text).await?;
                if result.ok {
                    self.state.push(SlackDayState {
                        channel_id: self.channel_id.clone(),
                        ts: result.ts.unwrap(),
                        date,
                    });
                    self.write_state()?;
                }
            }
        }

        Ok(())
    }

    async fn send_message(&self, message: String) -> Result<Response, SyncError> {
        let result = self
            .post(
                "https://slack.com/api/chat.postMessage",
                serde_json::json!({
                    "channel": &self.channel_id,
                    "blocks": [
                        {
                            "type": "context",
                            "elements": [
                                {
                                    "type": "mrkdwn",
                                    "text": message
                                }
                            ]
                        }
                    ]
                }),
            )
            .await?;

        Ok(result)
    }

    async fn update_message(
        &self,
        ts: String,
        message: String,
    ) -> Result<Response, reqwest::Error> {
        let result = self
            .post(
                "https://slack.com/api/chat.update",
                serde_json::json!({
                  "channel": &self.channel_id,
                  "ts": ts,
                  "blocks": [
                      {
                          "type": "context",
                          "elements": [
                              {
                                  "type": "mrkdwn",
                                  "text": message
                              }
                          ]
                      }
                  ],
                }),
            )
            .await?;

        Ok(result)
    }
}

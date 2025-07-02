use super::SyncError;
use base::{Day, TaskState};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use time::Date;

pub trait SlackMessage {
    fn to_message(&self) -> String;
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
    fn to_message(&self) -> String {
        let mut text = "".to_string();

        for task in &self.tasks {
            if task.subtasks.is_empty() {
                text.push_str(&format!("{} {}\n", task.state.to_emoji(), task.name));
            } else {
                if !text.is_empty() {
                    text.push_str(&format!("\n"));
                }
                text.push_str(&format!("*{}*\n", task.name));
                for subtask in &task.subtasks {
                    text.push_str(&format!("{} {}\n", subtask.state.to_emoji(), subtask.name));
                }
                text.push_str(&format!("\n"));
            }
        }

        text.push_str("\nPowered by <https://github.com/matsimitsu/w0rk|w0rk>");
        text
    }

    fn date(&self) -> Date {
        self.date
    }
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

    pub async fn sync_message<M>(&mut self, message: M) -> Result<(), SyncError>
    where
        M: SlackMessage,
    {
        let date = message.date();
        let state = self.state.iter().find(|state| state.date == date);

        match state {
            Some(state) => {
                self.update_message(&state.ts, message).await?;
            }
            None => {
                let result = self.send_message(message).await?;
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

    async fn send_message<M>(&self, message: M) -> Result<Response, SyncError>
    where
        M: SlackMessage,
    {
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
                                    "text": message.to_message()
                                }
                            ]
                        }
                    ]
                }),
            )
            .await?;

        Ok(result)
    }

    async fn update_message<M>(&self, ts: &str, message: M) -> Result<Response, reqwest::Error>
    where
        M: SlackMessage,
    {
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
                                  "text": message.to_message()
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

//! The single place external CLIs (`gh`, `appsignal-cli`, `rex`) are invoked.
//! Sources and spawners depend on the [`CliRunner`] trait so tests can use fixtures.

use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::process::Command;

/// Name of an executable on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Program(String);

impl Program {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A fully described invocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommandSpec {
    program: Program,
    args: Vec<String>,
    cwd: Option<PathBuf>,
}

impl CommandSpec {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            args: Vec::new(),
            cwd: None,
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

impl fmt::Display for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program.as_str())?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("could not start `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("`{command}` timed out after {seconds}s")]
    Timeout { command: String, seconds: u64 },
    #[error("`{command}` exited with {code}: {stderr}")]
    Failed {
        command: String,
        code: String,
        stderr: String,
    },
    #[error("`{command}` printed no JSON")]
    NoJson { command: String },
    #[error("`{command}` printed invalid JSON: {source}")]
    InvalidJson {
        command: String,
        source: serde_json::Error,
    },
}

/// Captured stdout of a successful command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stdout(String);

impl Stdout {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse the JSON document in the output, skipping any preamble such as an update banner.
    pub fn json<T: DeserializeOwned>(&self, spec: &CommandSpec) -> Result<T, ExecError> {
        let start = self.0.find(['{', '[']).ok_or_else(|| ExecError::NoJson {
            command: spec.to_string(),
        })?;
        let mut stream = serde_json::Deserializer::from_str(&self.0[start..]).into_iter::<T>();
        match stream.next() {
            Some(Ok(value)) => Ok(value),
            Some(Err(source)) => Err(ExecError::InvalidJson {
                command: spec.to_string(),
                source,
            }),
            None => Err(ExecError::NoJson {
                command: spec.to_string(),
            }),
        }
    }
}

#[async_trait]
pub trait CliRunner: Send + Sync {
    async fn run(&self, spec: &CommandSpec) -> Result<Stdout, ExecError>;
}

/// Runs real processes with a timeout.
#[derive(Debug, Clone)]
pub struct SystemRunner {
    timeout: Duration,
}

impl SystemRunner {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

#[async_trait]
impl CliRunner for SystemRunner {
    async fn run(&self, spec: &CommandSpec) -> Result<Stdout, ExecError> {
        let mut command = Command::new(spec.program.as_str());
        command
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &spec.cwd {
            command.current_dir(dir);
        }

        let output = tokio::time::timeout(self.timeout, command.output())
            .await
            .map_err(|_| ExecError::Timeout {
                command: spec.to_string(),
                seconds: self.timeout.as_secs(),
            })?
            .map_err(|source| ExecError::Spawn {
                command: spec.to_string(),
                source,
            })?;

        if output.status.success() {
            Ok(Stdout::new(String::from_utf8_lossy(&output.stdout)))
        } else {
            Err(ExecError::Failed {
                command: spec.to_string(),
                code: output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |c| c.to_string()),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

#[cfg(test)]
pub mod testing {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// Canned responses keyed by the rendered command line. Unknown commands fail loudly.
    #[derive(Default)]
    pub struct FakeRunner {
        responses: Mutex<HashMap<CommandSpec, Stdout>>,
        calls: Mutex<Vec<CommandSpec>>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self::default()
        }

        #[must_use]
        pub fn respond(self, spec: CommandSpec, stdout: &str) -> Self {
            self.responses
                .lock()
                .expect("poisoned")
                .insert(spec, Stdout::new(stdout));
            self
        }

        pub fn calls(&self) -> Vec<CommandSpec> {
            self.calls.lock().expect("poisoned").clone()
        }
    }

    #[async_trait]
    impl CliRunner for FakeRunner {
        async fn run(&self, spec: &CommandSpec) -> Result<Stdout, ExecError> {
            self.calls.lock().expect("poisoned").push(spec.clone());
            self.responses
                .lock()
                .expect("poisoned")
                .get(spec)
                .cloned()
                .ok_or_else(|| ExecError::Failed {
                    command: spec.to_string(),
                    code: "fake".to_owned(),
                    stderr: "no canned response".to_owned(),
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CommandSpec {
        CommandSpec::new(Program::new("appsignal-cli")).arg("apps")
    }

    #[test]
    fn json_skips_update_banner() {
        let out = Stdout::new("+----+\n| Newer version available |\n+----+\n{\"apps\": [1, 2]}\n");
        let value: serde_json::Value = out.json(&spec()).unwrap();
        assert_eq!(value["apps"][1], 2);
    }

    #[test]
    fn json_reports_missing_document() {
        let err = Stdout::new("nothing here")
            .json::<serde_json::Value>(&spec())
            .unwrap_err();
        assert!(matches!(err, ExecError::NoJson { .. }));
    }

    #[test]
    fn json_reports_invalid_document() {
        let err = Stdout::new("{oops")
            .json::<serde_json::Value>(&spec())
            .unwrap_err();
        assert!(matches!(err, ExecError::InvalidJson { .. }));
    }

    #[tokio::test]
    async fn system_runner_captures_stdout_and_failures() {
        let runner = SystemRunner::new(Duration::from_secs(5));
        let ok = runner
            .run(&CommandSpec::new(Program::new("echo")).arg("hi"))
            .await
            .unwrap();
        assert_eq!(ok.as_str().trim(), "hi");

        let err = runner
            .run(&CommandSpec::new(Program::new("sh")).args(["-c", "echo bad >&2; exit 3"]))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExecError::Failed { ref code, ref stderr, .. } if code == "3" && stderr == "bad")
        );
    }

    #[tokio::test]
    async fn system_runner_times_out() {
        let runner = SystemRunner::new(Duration::from_millis(50));
        let err = runner
            .run(&CommandSpec::new(Program::new("sleep")).arg("5"))
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }));
    }

    #[tokio::test]
    async fn fake_runner_returns_canned_output_and_records_calls() {
        let runner = testing::FakeRunner::new().respond(spec(), "[1]");
        let value: Vec<u8> = runner.run(&spec()).await.unwrap().json(&spec()).unwrap();
        assert_eq!(value, vec![1]);
        assert_eq!(runner.calls(), vec![spec()]);
    }
}

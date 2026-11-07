//! `w0rk doctor`: verify the external tools w0rk depends on are usable.

use crate::exec::{CliRunner, CommandSpec, Program};

pub struct Check {
    pub name: &'static str,
    pub spec: CommandSpec,
}

pub struct CheckResult {
    pub name: &'static str,
    pub outcome: Result<(), String>,
}

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            name: "gh authenticated",
            spec: CommandSpec::new(Program::new("gh")).args(["auth", "status"]),
        },
        Check {
            name: "appsignal-cli can list apps",
            spec: CommandSpec::new(Program::new("appsignal-cli"))
                .args(["apps", "list", "-o", "json"]),
        },
        Check {
            name: "rex server reachable",
            spec: CommandSpec::new(Program::new("rex")).args(["whoami", "--json"]),
        },
        Check {
            name: "claude installed",
            spec: CommandSpec::new(Program::new("claude")).arg("--version"),
        },
    ]
}

pub async fn run_checks(runner: &impl CliRunner, checks: Vec<Check>) -> Vec<CheckResult> {
    let mut results = Vec::with_capacity(checks.len());
    for check in checks {
        let outcome = runner
            .run(&check.spec)
            .await
            .map(|_| ())
            .map_err(|err| err.to_string());
        results.push(CheckResult {
            name: check.name,
            outcome,
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::testing::FakeRunner;

    #[tokio::test]
    async fn reports_per_check_outcome() {
        let all = checks();
        let gh = all[0].spec.clone();
        let runner = FakeRunner::new().respond(gh, "ok");

        let results = run_checks(&runner, checks()).await;

        assert_eq!(results.len(), 4);
        assert!(results[0].outcome.is_ok());
        assert!(results[1].outcome.is_err());
    }
}

//! Pull requests via `gh search prs`, plus `gh pr view` for head sha, reviews and CI.
//! The per-PR details are cached until the PR's `updatedAt` moves.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::{Source, SourceError};
use crate::config::{GithubConfig, GithubQuery, GithubRole};
use crate::exec::{CliRunner, CommandSpec, Program};
use crate::model::{Fingerprint, Item, ItemId, ItemKind, RepoSlug, SourceId};

const SEARCH_FIELDS: &str =
    "number,title,url,repository,author,updatedAt,createdAt,isDraft,labels,commentsCount";
const DETAIL_FIELDS: &str = "headRefOid,reviewDecision,reviews,statusCheckRollup";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    number: u64,
    title: String,
    url: String,
    repository: Repository,
    author: Author,
    updated_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    is_draft: bool,
    labels: Vec<Label>,
    comments_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    name_with_owner: RepoSlug,
}

#[derive(Debug, Deserialize)]
struct Author {
    login: String,
    is_bot: bool,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Login {
    login: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Commit {
    oid: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Review {
    /// `null` when the reviewer's account was deleted.
    author: Option<Login>,
    state: String,
    #[serde(rename = "submittedAt")]
    submitted_at: Option<DateTime<Utc>>,
    commit: Option<Commit>,
}

/// Either a check run (`status`/`conclusion`) or a commit status (`state`).
#[derive(Debug, Clone, Default, Deserialize)]
struct Check {
    status: Option<String>,
    conclusion: Option<String>,
    state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Details {
    head_ref_oid: String,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    reviews: Vec<Review>,
    #[serde(default)]
    status_check_rollup: Vec<Check>,
}

impl Details {
    /// The newest review per reviewer, oldest reviewer first.
    fn latest_reviews(&self) -> Vec<&Review> {
        let mut latest: Vec<&Review> = Vec::new();
        for review in &self.reviews {
            let Some(author) = &review.author else {
                continue;
            };
            match latest
                .iter_mut()
                .find(|r| r.author.as_ref().is_some_and(|a| a.login == author.login))
            {
                Some(slot) => {
                    if review.submitted_at >= slot.submitted_at {
                        *slot = review;
                    }
                }
                None => latest.push(review),
            }
        }
        latest
    }

    fn review_by(&self, login: &str) -> Option<&Review> {
        self.latest_reviews()
            .into_iter()
            .find(|r| r.author.as_ref().is_some_and(|a| a.login == login))
    }

    /// `failure` beats `pending` beats `success`; `none` when there are no checks.
    fn ci(&self) -> &'static str {
        const BAD: [&str; 5] = [
            "FAILURE",
            "TIMED_OUT",
            "CANCELLED",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
        ];
        let failed = |c: &Check| {
            c.conclusion.as_deref().is_some_and(|v| BAD.contains(&v))
                || c.state
                    .as_deref()
                    .is_some_and(|v| matches!(v, "FAILURE" | "ERROR"))
        };
        let pending = |c: &Check| {
            c.status.as_deref().is_some_and(|v| v != "COMPLETED")
                || c.state
                    .as_deref()
                    .is_some_and(|v| matches!(v, "PENDING" | "EXPECTED"))
        };
        if self.status_check_rollup.is_empty() {
            "none"
        } else if self.status_check_rollup.iter().any(failed) {
            "failure"
        } else if self.status_check_rollup.iter().any(pending) {
            "pending"
        } else {
            "success"
        }
    }
}

pub struct GithubSource {
    config: GithubConfig,
    details: Mutex<HashMap<ItemId, (DateTime<Utc>, Details)>>,
    me: Mutex<Option<String>>,
}

impl GithubSource {
    pub fn new(config: GithubConfig) -> Self {
        Self {
            config,
            details: Mutex::new(HashMap::new()),
            me: Mutex::new(None),
        }
    }

    fn search_spec(query: &GithubQuery) -> CommandSpec {
        let mut spec = CommandSpec::new(Program::new("gh"))
            .args(["search", "prs", "--state=open", "--limit=100"])
            .arg(format!("{}={}", query.role.search_flag(), query.who))
            .arg(format!("--json={SEARCH_FIELDS}"));
        if !query.include_drafts {
            spec = spec.arg("--draft=false");
        }
        for owner in &query.owners {
            spec = spec.arg(format!("--owner={owner}"));
        }
        for repo in &query.repos {
            spec = spec.arg(format!("--repo={repo}"));
        }
        for label in &query.labels {
            spec = spec.arg(format!("--label={label}"));
        }
        spec
    }

    fn details_spec(repo: &RepoSlug, number: u64) -> CommandSpec {
        CommandSpec::new(Program::new("gh")).args([
            "pr".to_owned(),
            "view".to_owned(),
            number.to_string(),
            format!("--repo={repo}"),
            format!("--json={DETAIL_FIELDS}"),
        ])
    }

    fn whoami_spec() -> CommandSpec {
        CommandSpec::new(Program::new("gh")).args(["api", "user", "--jq", ".login"])
    }

    /// The authenticated user's login, looked up once. Without it, review history is just empty.
    async fn me(&self, cli: &dyn CliRunner) -> String {
        if let Some(login) = self.me.lock().expect("poisoned").clone() {
            return login;
        }
        let login = cli
            .run(&Self::whoami_spec())
            .await
            .map(|out| out.as_str().trim().to_owned())
            .unwrap_or_default();
        if !login.is_empty() {
            *self.me.lock().expect("poisoned") = Some(login.clone());
        }
        login
    }

    fn cached(&self, id: &ItemId, updated_at: DateTime<Utc>) -> Option<Details> {
        let details = self.details.lock().expect("poisoned");
        details
            .get(id)
            .filter(|(seen, _)| *seen == updated_at)
            .map(|(_, d)| d.clone())
    }

    async fn details(
        &self,
        cli: &dyn CliRunner,
        id: &ItemId,
        pr: &PullRequest,
    ) -> Result<Details, SourceError> {
        if let Some(details) = self.cached(id, pr.updated_at) {
            return Ok(details);
        }
        let spec = Self::details_spec(&pr.repository.name_with_owner, pr.number);
        let details: Details = cli.run(&spec).await?.json(&spec)?;
        self.details
            .lock()
            .expect("poisoned")
            .insert(id.clone(), (pr.updated_at, details.clone()));
        Ok(details)
    }

    fn to_item(query: &GithubQuery, me: &str, pr: PullRequest, details: &Details) -> Item {
        let repo = pr.repository.name_with_owner;
        let labels: Vec<String> = pr.labels.into_iter().map(|l| l.name).collect();
        let decision = details.review_decision.clone().unwrap_or_default();
        let latest = details.latest_reviews();
        let mine = (!me.is_empty()).then(|| details.review_by(me)).flatten();
        let reviewed_by_me = mine.is_some();
        let requested = query.role == GithubRole::ReviewRequested;
        let my_commit = mine.and_then(|r| r.commit.as_ref()).map(|c| c.oid.clone());
        // Review-role items keep the bare head sha as fingerprint, so a `changed` rule can hand the
        // previous sha to the agent. Author-role items also change when reviews arrive.
        let fingerprint = if query.role == GithubRole::Author {
            format!(
                "{}|{}|{}",
                details.head_ref_oid,
                decision,
                details.reviews.len()
            )
        } else {
            details.head_ref_oid.clone()
        };
        Item {
            id: item_id(&repo, pr.number),
            source: SourceId::new("github"),
            kind: ItemKind::new(&query.kind),
            title: pr.title,
            url: pr.url,
            fingerprint: Fingerprint::new(fingerprint),
            updated_at: pr.updated_at,
            attrs: json!({
                "query": query.name,
                "mine": query.role == GithubRole::Author,
                "repo": repo,
                "number": pr.number,
                "author": pr.author.login,
                "author_is_bot": pr.author.is_bot,
                "draft": pr.is_draft,
                "labels": labels,
                "comments": pr.comments_count,
                "created_at": pr.created_at,
                "head_sha": details.head_ref_oid,
                "ci": details.ci(),
                "review_decision": decision,
                "reviews": latest
                    .iter()
                    .filter_map(|r| r.author.as_ref().map(|a| format!("{} {}", a.login, r.state)))
                    .collect::<Vec<_>>(),
                "reviewed_by_me": reviewed_by_me,
                "my_review": mine.map(|r| r.state.clone()).unwrap_or_default(),
                "my_review_commit": my_commit,
                // Requested again after I already reviewed.
                "rereview": requested && reviewed_by_me,
                "new_commits_since_my_review": my_commit
                    .as_deref()
                    .is_some_and(|oid| oid != details.head_ref_oid),
            }),
        }
    }
}

fn item_id(repo: &RepoSlug, number: u64) -> ItemId {
    ItemId::new(format!("github:{repo}#{number}"))
}

#[async_trait]
impl Source for GithubSource {
    fn id(&self) -> SourceId {
        SourceId::new("github")
    }

    fn poll_every(&self) -> Duration {
        self.config.poll_seconds.duration()
    }

    async fn fetch(&self, cli: &dyn CliRunner) -> Result<Vec<Item>, SourceError> {
        let me = self.me(cli).await;
        let mut items: Vec<Item> = Vec::new();
        for query in &self.config.queries {
            let spec = Self::search_spec(query);
            let prs: Vec<PullRequest> = cli.run(&spec).await?.json(&spec)?;
            for pr in prs {
                let id = item_id(&pr.repository.name_with_owner, pr.number);
                // The first query (in config order) that matches a PR decides its kind.
                if items.iter().any(|item| item.id == id) {
                    continue;
                }
                if pr
                    .labels
                    .iter()
                    .any(|l| query.exclude_labels.contains(&l.name))
                {
                    continue;
                }
                let details = self.details(cli, &id, &pr).await?;
                items.push(Self::to_item(query, &me, pr, &details));
            }
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::exec::testing::FakeRunner;

    const SEARCH: &str = r#"[
      {"author":{"id":"x","is_bot":true,"login":"dependabot[bot]","type":"Bot","url":"u"},
       "commentsCount":0,"createdAt":"2026-11-05T06:04:21Z","isDraft":false,
       "labels":[{"name":"dependencies"}],"number":18350,
       "repository":{"name":"appsignal-server","nameWithOwner":"appsignal/appsignal-server"},
       "title":"Bump stripe","updatedAt":"2026-11-05T06:04:25Z",
       "url":"https://github.com/appsignal/appsignal-server/pull/18350"},
      {"author":{"id":"y","is_bot":false,"login":"lipskis","type":"User","url":"u"},
       "commentsCount":2,"createdAt":"2026-11-05T07:00:00Z","isDraft":false,
       "labels":[{"name":"wip"}],"number":2427,
       "repository":{"name":"p","nameWithOwner":"appsignal/appsignal-processor-rs"},
       "title":"Add crate","updatedAt":"2026-11-05T08:08:36Z","url":"https://x/2427"}
    ]"#;

    fn config(extra: &str) -> GithubConfig {
        Config::parse(&format!(
            r#"
            [source.github]
            [[source.github.query]]
            name = "review-requests"
            kind = "pr.review"
            role = "review-requested"
            {extra}
            "#
        ))
        .unwrap()
        .source
        .github
        .unwrap()
    }

    fn details_response(sha: &str) -> String {
        format!(r#"{{"headRefOid":"{sha}"}}"#)
    }

    fn runner(source: &GithubSource) -> FakeRunner {
        FakeRunner::new()
            .respond(GithubSource::whoami_spec(), "matsimitsu\n")
            .respond(GithubSource::search_spec(&source.config.queries[0]), SEARCH)
            .respond(
                GithubSource::details_spec(&RepoSlug::new("appsignal/appsignal-server"), 18350),
                &details_response("aaa"),
            )
            .respond(
                GithubSource::details_spec(
                    &RepoSlug::new("appsignal/appsignal-processor-rs"),
                    2427,
                ),
                &details_response("bbb"),
            )
    }

    #[test]
    fn search_spec_reflects_the_query() {
        let cfg = config(
            r#"owners = ["appsignal"]
            repos = ["appsignal/appsignal-server"]
            labels = ["dependencies"]"#,
        );
        let spec = GithubSource::search_spec(&cfg.queries[0]).to_string();
        assert!(spec.starts_with("gh search prs --state=open"));
        assert!(spec.contains("--review-requested=@me"));
        assert!(spec.contains("--draft=false"));
        assert!(spec.contains("--owner=appsignal"));
        assert!(spec.contains("--repo=appsignal/appsignal-server"));
        assert!(spec.contains("--label=dependencies"));
    }

    #[tokio::test]
    async fn maps_prs_to_items_with_head_sha_fingerprint() {
        let source = GithubSource::new(config(""));
        let items = source.fetch(&runner(&source)).await.unwrap();

        assert_eq!(items.len(), 2);
        let first = &items[0];
        assert_eq!(first.id.as_str(), "github:appsignal/appsignal-server#18350");
        assert_eq!(first.kind.as_str(), "pr.review");
        assert_eq!(first.fingerprint.as_str(), "aaa");
        assert_eq!(first.attrs["author"], "dependabot[bot]");
        assert_eq!(first.attrs["author_is_bot"], true);
        assert_eq!(first.attrs["labels"][0], "dependencies");
        assert_eq!(first.attrs["rereview"], false);
        assert_eq!(first.attrs["ci"], "none");
    }

    #[tokio::test]
    async fn excluded_labels_are_dropped() {
        let source = GithubSource::new(config(r#"exclude_labels = ["wip"]"#));
        let items = source.fetch(&runner(&source)).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].attrs["number"], 18350);
    }

    #[tokio::test]
    async fn details_are_only_refetched_when_the_pr_updates() {
        let source = GithubSource::new(config(""));
        let cli = runner(&source);
        source.fetch(&cli).await.unwrap();
        source.fetch(&cli).await.unwrap();

        let calls: Vec<String> = cli.calls().iter().map(ToString::to_string).collect();
        let detail_calls = calls.iter().filter(|c| c.starts_with("gh pr view")).count();
        let whoami_calls = calls
            .iter()
            .filter(|c| c.starts_with("gh api user"))
            .count();
        assert_eq!(detail_calls, 2, "one per PR, not per poll");
        assert_eq!(whoami_calls, 1, "login is looked up once");
    }

    #[tokio::test]
    async fn cli_failure_is_an_error_not_an_empty_list() {
        let source = GithubSource::new(config(""));
        assert!(source.fetch(&FakeRunner::new()).await.is_err());
    }

    fn reviewed(decision: &str, reviews: &str, sha: &str) -> String {
        format!(
            r#"{{"headRefOid":"{sha}","reviewDecision":"{decision}","reviews":{reviews},
                 "statusCheckRollup":[{{"status":"COMPLETED","conclusion":"SUCCESS"}}]}}"#
        )
    }

    async fn first_item(config: GithubConfig, details: &str) -> Item {
        let source = GithubSource::new(config);
        let cli = FakeRunner::new()
            .respond(GithubSource::whoami_spec(), "matsimitsu")
            .respond(GithubSource::search_spec(&source.config.queries[0]), SEARCH)
            .respond(
                GithubSource::details_spec(&RepoSlug::new("appsignal/appsignal-server"), 18350),
                details,
            )
            .respond(
                GithubSource::details_spec(
                    &RepoSlug::new("appsignal/appsignal-processor-rs"),
                    2427,
                ),
                &details_response("bbb"),
            );
        source.fetch(&cli).await.unwrap().remove(0)
    }

    #[tokio::test]
    async fn a_pr_you_reviewed_and_were_asked_about_again_is_a_rereview() {
        let reviews = r#"[
          {"author":{"login":"matsimitsu"},"state":"CHANGES_REQUESTED","submittedAt":"2026-11-01T10:00:00Z","commit":{"oid":"old"}},
          {"author":{"login":"someone"},"state":"APPROVED","submittedAt":"2026-11-02T10:00:00Z","commit":{"oid":"old"}}]"#;
        let item = first_item(config(""), &reviewed("REVIEW_REQUIRED", reviews, "new")).await;

        assert_eq!(item.attrs["rereview"], true);
        assert_eq!(item.attrs["reviewed_by_me"], true);
        assert_eq!(item.attrs["my_review"], "CHANGES_REQUESTED");
        assert_eq!(item.attrs["my_review_commit"], "old");
        assert_eq!(item.attrs["new_commits_since_my_review"], true);
        assert_eq!(
            item.fingerprint.as_str(),
            "new",
            "review kinds keep the bare sha"
        );
    }

    #[tokio::test]
    async fn only_the_latest_review_per_person_counts() {
        let reviews = r#"[
          {"author":{"login":"matsimitsu"},"state":"CHANGES_REQUESTED","submittedAt":"2026-11-01T10:00:00Z","commit":{"oid":"a"}},
          {"author":{"login":"matsimitsu"},"state":"APPROVED","submittedAt":"2026-11-03T10:00:00Z","commit":{"oid":"b"}}]"#;
        let item = first_item(config(""), &reviewed("APPROVED", reviews, "b")).await;
        assert_eq!(item.attrs["my_review"], "APPROVED");
        assert_eq!(item.attrs["new_commits_since_my_review"], false);
        assert_eq!(item.attrs["reviews"], json!(["matsimitsu APPROVED"]));
    }

    #[tokio::test]
    async fn own_prs_carry_the_review_decision_and_change_when_reviews_arrive() {
        let own = config(r#"owners = ["appsignal"]"#);
        let mut own = own;
        own.queries[0].role = GithubRole::Author;
        own.queries[0].kind = "pr.mine".to_owned();

        let none = first_item(own.clone(), &reviewed("REVIEW_REQUIRED", "[]", "s1")).await;
        let reviews = r#"[{"author":{"login":"sam"},"state":"APPROVED","submittedAt":"2026-11-03T10:00:00Z","commit":{"oid":"s1"}}]"#;
        let approved = first_item(own, &reviewed("APPROVED", reviews, "s1")).await;

        assert_eq!(approved.attrs["review_decision"], "APPROVED");
        assert_eq!(approved.attrs["rereview"], false, "not a review request");
        assert_ne!(
            none.fingerprint, approved.fingerprint,
            "a review makes the item change"
        );
    }

    #[test]
    fn ci_summary_prefers_failure_then_pending() {
        let check = |status: &str, conclusion: Option<&str>| Check {
            status: Some(status.to_owned()),
            conclusion: conclusion.map(str::to_owned),
            state: None,
        };
        let details = |checks: Vec<Check>| Details {
            head_ref_oid: "x".to_owned(),
            review_decision: None,
            reviews: Vec::new(),
            status_check_rollup: checks,
        };
        assert_eq!(details(vec![]).ci(), "none");
        assert_eq!(
            details(vec![check("COMPLETED", Some("SUCCESS"))]).ci(),
            "success"
        );
        assert_eq!(
            details(vec![
                check("COMPLETED", Some("SUCCESS")),
                check("IN_PROGRESS", None)
            ])
            .ci(),
            "pending"
        );
        assert_eq!(
            details(vec![
                check("IN_PROGRESS", None),
                check("COMPLETED", Some("FAILURE"))
            ])
            .ci(),
            "failure"
        );
    }
}

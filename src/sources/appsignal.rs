//! Exception incidents via `appsignal-cli incidents list-exceptions`, one source per configured app.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::{Source, SourceError};
use crate::config::{AppsignalApp, IncidentState, PollSeconds};
use crate::exec::{CliRunner, CommandSpec, Program};
use crate::model::{Fingerprint, Item, ItemId, ItemKind, SourceId};

/// Limit per state call. "New since last run" asks for more, since it filters client-side.
const LIMIT: &str = "50";
const LIMIT_SINCE: &str = "100";

#[derive(Debug, Deserialize)]
struct Listing {
    incidents: Vec<Incident>,
}

#[derive(Debug, Deserialize)]
struct Assignee {
    id: String,
    name: Option<String>,
    email: Option<String>,
}

impl Assignee {
    /// Whatever identifies the person best: name, then email, then id.
    fn label(&self) -> &str {
        self.name
            .as_deref()
            .or(self.email.as_deref())
            .unwrap_or(&self.id)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Incident {
    number: u64,
    state: String,
    severity: String,
    count: u64,
    // AppSignal returns null for any of these on some incidents.
    created_at: Option<DateTime<Utc>>,
    last_occurred_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    exception_name: String,
    exception_message: String,
    action_names: Vec<String>,
    namespace: String,
    first_backtrace_line: Option<String>,
    #[serde(default)]
    assignees: Vec<Assignee>,
}

#[derive(Debug, Deserialize)]
struct AppListing {
    apps: Vec<AppEntry>,
}

#[derive(Debug, Deserialize)]
struct AppEntry {
    id: String,
    name: String,
    environment: String,
}

pub struct AppsignalSource {
    app: AppsignalApp,
    org: Option<String>,
    poll: PollSeconds,
    /// When set (and the app opts in), only incidents created at or after this are listed.
    since: Option<DateTime<Utc>>,
}

impl AppsignalSource {
    pub fn new(
        app: AppsignalApp,
        org: Option<String>,
        poll: PollSeconds,
        since: Option<DateTime<Utc>>,
    ) -> Self {
        let since = since.filter(|_| app.new_since_last_run);
        Self {
            app,
            org,
            poll,
            since,
        }
    }

    fn list_spec(&self, state: IncidentState) -> CommandSpec {
        // Creation order puts the newest incidents first, which is what "new since" needs.
        let (order, limit) = if self.since.is_some() {
            ("--order=ID", LIMIT_SINCE)
        } else {
            ("--order=LAST", LIMIT)
        };
        let mut spec = CommandSpec::new(Program::new("appsignal-cli"))
            .args(["incidents", "list-exceptions", "-o=json", order])
            .arg(format!("--limit={limit}"))
            .arg(format!("--state={}", state.as_str()))
            .arg(format!("--app={}", self.app.name))
            .arg(format!("--environment={}", self.app.environment));
        if !self.app.namespaces.is_empty() {
            let joined: Vec<&str> = self.app.namespaces.iter().map(|n| n.as_str()).collect();
            spec = spec.arg(format!("--namespaces={}", joined.join(",")));
        }
        spec
    }

    fn apps_spec() -> CommandSpec {
        CommandSpec::new(Program::new("appsignal-cli")).args(["apps", "list", "-o=json"])
    }

    /// Resolve the app id for deep links. Links are a nicety, so a failure just means no URL.
    async fn app_id(&self, cli: &dyn CliRunner) -> Option<String> {
        let spec = Self::apps_spec();
        let listing: AppListing = cli.run(&spec).await.ok()?.json(&spec).ok()?;
        listing
            .apps
            .into_iter()
            .find(|a| {
                a.name == self.app.name.as_str() && a.environment == self.app.environment.as_str()
            })
            .map(|a| a.id)
    }

    fn url(&self, app_id: Option<&str>, number: u64) -> String {
        match (&self.org, app_id) {
            (Some(org), Some(id)) => {
                format!("https://appsignal.com/{org}/sites/{id}/exceptions/incidents/{number}")
            }
            _ => String::new(),
        }
    }

    fn is_wanted(&self, incident: &Incident) -> bool {
        match self.since {
            None => true,
            Some(since) => incident.created_at.is_some_and(|created| created >= since),
        }
    }

    fn to_item(&self, app_id: Option<&str>, incident: Incident) -> Item {
        let updated_at = incident
            .updated_at
            .or(incident.last_occurred_at)
            .or(incident.created_at)
            .unwrap_or(DateTime::UNIX_EPOCH);
        let assignees: Vec<&str> = incident.assignees.iter().map(Assignee::label).collect();
        Item {
            id: ItemId::new(format!("appsignal:{}#{}", self.app.name, incident.number)),
            source: self.id(),
            kind: ItemKind::new("incident.exception"),
            title: incident.exception_name.clone(),
            url: self.url(app_id, incident.number),
            fingerprint: Fingerprint::new(&incident.state),
            updated_at,
            attrs: json!({
                "app": self.app.name,
                "environment": self.app.environment,
                "number": incident.number,
                "state": incident.state,
                "severity": incident.severity,
                "assignees": assignees,
                "assigned": !assignees.is_empty(),
                "namespace": incident.namespace,
                "exception_name": incident.exception_name,
                "message": incident.exception_message,
                "actions": incident.action_names,
                "count": incident.count,
                "first_backtrace_line": incident.first_backtrace_line,
                "created_at": incident.created_at,
                "last_occurred_at": incident.last_occurred_at,
            }),
        }
    }
}

#[async_trait]
impl Source for AppsignalSource {
    fn id(&self) -> SourceId {
        SourceId::new(format!("appsignal:{}", self.app.name))
    }

    fn poll_every(&self) -> Duration {
        self.poll.duration()
    }

    async fn fetch(&self, cli: &dyn CliRunner) -> Result<Vec<Item>, SourceError> {
        let mut incidents: Vec<Incident> = Vec::new();
        for state in &self.app.states {
            let spec = self.list_spec(*state);
            let listing: Listing = cli.run(&spec).await?.json(&spec)?;
            incidents.extend(listing.incidents.into_iter().filter(|i| self.is_wanted(i)));
        }
        let app_id = if self.org.is_some() && !incidents.is_empty() {
            self.app_id(cli).await
        } else {
            None
        };
        Ok(incidents
            .into_iter()
            .map(|incident| self.to_item(app_id.as_deref(), incident))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::exec::testing::FakeRunner;

    const LISTING: &str = r#"{"incidents":[{
      "__typename":"ExceptionIncident","id":"6aab","number":14036,"state":"OPEN",
      "severity":"UNTRIAGED","description":"","count":279820,
      "createdAt":"2026-09-17T01:05:00Z","lastOccurredAt":"2026-11-05T19:37:00Z",
      "updatedAt":"2026-11-05T06:00:17Z","exceptionName":"Error",
      "exceptionMessage":"boom","actionNames":["GraphQL Auth Error"],
      "namespace":"web","firstBacktraceLine":"a.js:71","digests":["1"],
      "assignees":[{"id":"53c6","name":"Thijs Cadier","email":null},
                   {"id":"77aa","name":null,"email":"sam@example.com"}]}]}"#;

    const APPS: &str = r#"{"apps":[{"id":"55df","name":"AppSignal","environment":"production"}]}"#;

    fn app(extra: &str) -> AppsignalApp {
        Config::parse(&format!(
            r#"
            [source.appsignal]
            [[source.appsignal.app]]
            name = "AppSignal"
            namespaces = ["web", "background"]
            {extra}
            "#
        ))
        .unwrap()
        .source
        .appsignal
        .unwrap()
        .apps
        .remove(0)
    }

    fn source(org: Option<&str>) -> AppsignalSource {
        AppsignalSource::new(
            app(""),
            org.map(str::to_owned),
            PollSeconds::default(),
            None,
        )
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn list_spec_filters_by_app_namespaces_and_state() {
        let spec = source(None).list_spec(IncidentState::Wip).to_string();
        assert!(spec.contains("--app=AppSignal"));
        assert!(spec.contains("--environment=production"));
        assert!(spec.contains("--namespaces=web,background"));
        assert!(spec.contains("--state=WIP"));
        assert!(spec.contains("--order=LAST"));
    }

    #[tokio::test]
    async fn maps_incidents_to_items_with_state_and_assignees() {
        let src = source(None);
        let cli = FakeRunner::new().respond(src.list_spec(IncidentState::Open), LISTING);
        let items = src.fetch(&cli).await.unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.id.as_str(), "appsignal:AppSignal#14036");
        assert_eq!(item.kind.as_str(), "incident.exception");
        assert_eq!(item.fingerprint.as_str(), "OPEN");
        assert_eq!(item.attrs["state"], "OPEN");
        assert_eq!(item.attrs["assigned"], true);
        assert_eq!(
            item.attrs["assignees"],
            serde_json::json!(["Thijs Cadier", "sam@example.com"]),
            "name first, email when there is no name"
        );
        assert_eq!(item.attrs["count"], 279_820);
        assert_eq!(item.url, "");
    }

    #[tokio::test]
    async fn unassigned_incidents_say_so() {
        let src = source(None);
        let listing = LISTING.replace(
            r#""assignees":[{"id":"53c6","name":"Thijs Cadier","email":null},
                   {"id":"77aa","name":null,"email":"sam@example.com"}]"#,
            r#""assignees":[]"#,
        );
        let cli = FakeRunner::new().respond(src.list_spec(IncidentState::Open), &listing);
        let item = &src.fetch(&cli).await.unwrap()[0];
        assert_eq!(item.attrs["assigned"], false);
        assert_eq!(item.attrs["assignees"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn fetches_each_configured_state_and_merges() {
        let src = AppsignalSource::new(
            app(r#"states = ["OPEN", "WIP"]"#),
            None,
            PollSeconds::default(),
            None,
        );
        let wip = LISTING
            .replace("14036", "14037")
            .replace(r#""state":"OPEN""#, r#""state":"WIP""#);
        let cli = FakeRunner::new()
            .respond(src.list_spec(IncidentState::Open), LISTING)
            .respond(src.list_spec(IncidentState::Wip), &wip);

        let items = src.fetch(&cli).await.unwrap();

        let states: Vec<&str> = items
            .iter()
            .map(|i| i.attrs["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["OPEN", "WIP"]);
    }

    #[tokio::test]
    async fn since_last_run_keeps_only_incidents_created_after_the_cutoff() {
        let src = AppsignalSource::new(
            app("new_since_last_run = true"),
            None,
            PollSeconds::default(),
            Some(at("2026-10-01T00:00:00Z")),
        );
        let old = LISTING
            .replace("14036", "1")
            .replace("2026-09-17T01:05:00Z", "2026-09-01T00:00:00Z");
        let no_creation_time = LISTING.replace("14036", "2").replace(
            r#""createdAt":"2026-09-17T01:05:00Z""#,
            r#""createdAt":null"#,
        );
        let new = LISTING
            .replace("14036", "3")
            .replace("2026-09-17T01:05:00Z", "2026-11-04T00:00:00Z");
        let merged = format!(
            r#"{{"incidents":[{},{},{}]}}"#,
            incident_json(&new),
            incident_json(&old),
            incident_json(&no_creation_time),
        );
        let cli = FakeRunner::new().respond(src.list_spec(IncidentState::Open), &merged);

        let items = src.fetch(&cli).await.unwrap();

        let numbers: Vec<u64> = items
            .iter()
            .map(|i| i.attrs["number"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, [3]);
        assert!(
            src.list_spec(IncidentState::Open)
                .to_string()
                .contains("--order=ID")
        );
    }

    #[tokio::test]
    async fn the_cutoff_is_ignored_unless_the_app_opts_in() {
        let src = AppsignalSource::new(
            app(""),
            None,
            PollSeconds::default(),
            Some(at("2030-01-01T00:00:00Z")),
        );
        let cli = FakeRunner::new().respond(src.list_spec(IncidentState::Open), LISTING);
        assert_eq!(src.fetch(&cli).await.unwrap().len(), 1);
    }

    /// The single incident object inside a `{"incidents":[...]}` document.
    fn incident_json(listing: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(listing).unwrap();
        value["incidents"][0].to_string()
    }

    #[tokio::test]
    async fn tolerates_null_timestamps() {
        let src = source(None);
        let listing = LISTING
            .replace(
                r#""createdAt":"2026-09-17T01:05:00Z""#,
                r#""createdAt":null"#,
            )
            .replace(
                r#""updatedAt":"2026-11-05T06:00:17Z""#,
                r#""updatedAt":null"#,
            );
        let cli = FakeRunner::new().respond(src.list_spec(IncidentState::Open), &listing);

        let items = src.fetch(&cli).await.unwrap();

        assert_eq!(
            items[0].updated_at.to_rfc3339(),
            "2026-11-05T19:37:00+00:00",
            "falls back to lastOccurredAt"
        );
        assert!(items[0].attrs["created_at"].is_null());
    }

    #[tokio::test]
    async fn builds_deep_link_when_org_is_configured() {
        let src = source(Some("appsignal"));
        let cli = FakeRunner::new()
            .respond(src.list_spec(IncidentState::Open), LISTING)
            .respond(AppsignalSource::apps_spec(), APPS);
        let items = src.fetch(&cli).await.unwrap();
        assert_eq!(
            items[0].url,
            "https://appsignal.com/appsignal/sites/55df/exceptions/incidents/14036"
        );
    }

    #[tokio::test]
    async fn failing_app_lookup_only_loses_the_link() {
        let src = source(Some("appsignal"));
        let cli = FakeRunner::new().respond(src.list_spec(IncidentState::Open), LISTING);
        let items = src.fetch(&cli).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "");
    }

    #[tokio::test]
    async fn cli_failure_is_an_error_not_an_empty_list() {
        assert!(source(None).fetch(&FakeRunner::new()).await.is_err());
    }
}

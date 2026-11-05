//! Per-kind presentation. A [`View`] turns an [`Item`] into a list row and a details pane; the
//! generic view handles any kind nobody wrote a view for, so a new source renders on day one.

use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::model::Item;

pub const DIM: Color = Color::Rgb(95, 104, 115);
pub const RED: Color = Color::Rgb(240, 113, 120);
pub const YELLOW: Color = Color::Rgb(230, 192, 123);
pub const GREEN: Color = Color::Rgb(156, 204, 133);
pub const BLUE: Color = Color::Rgb(130, 170, 255);
pub const MAGENTA: Color = Color::Rgb(199, 146, 234);
pub const CYAN: Color = Color::Rgb(127, 212, 212);

/// The coloured dot in front of an item: its own state, before any agent is layered on top.
///
/// Shape says what kind of attention it needs, colour says which flavour:
/// - `●` needs you: blue review request, magenta Dependabot, red open incident,
///   green approved, red changes requested
/// - `◆` yellow: a re-review (you reviewed already and were asked again)
/// - `◐` yellow: someone is on it (incident in progress or assigned, PR with comments)
/// - `○` dim or green: nothing for you to do (waiting on reviewers, incident closed)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dot {
    pub glyph: char,
    pub color: Color,
}

impl Dot {
    const fn new(glyph: char, color: Color) -> Self {
        Self { glyph, color }
    }
}

pub struct RowParts {
    pub dot: Dot,
    pub tag: String,
    pub tag_color: Color,
    pub reference: String,
    pub title: String,
    pub age: DateTime<Utc>,
}

pub trait View: Sync {
    fn handles(&self, item: &Item) -> bool;
    fn row(&self, item: &Item) -> RowParts;
    fn detail(&self, item: &Item) -> Vec<Line<'static>>;
}

static VIEWS: [&dyn View; 3] = [&PullRequestView, &IncidentView, &GenericView];

pub fn view_for(item: &Item) -> &'static dyn View {
    VIEWS
        .iter()
        .copied()
        .find(|v| v.handles(item))
        .unwrap_or(&GenericView)
}

fn text(item: &Item, pointer: &str) -> String {
    match item.attrs.pointer(pointer) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn list(item: &Item, pointer: &str) -> String {
    item.attrs
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

pub fn heading(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_owned(),
        Style::default().fg(MAGENTA).add_modifier(Modifier::BOLD),
    ))
}

pub fn title_line(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_owned(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

pub fn dim_line(label: &str) -> Line<'static> {
    Line::from(Span::styled(label.to_owned(), Style::default().fg(DIM)))
}

pub fn kv(key: &str, value: impl Into<String>, color: Option<Color>) -> Line<'static> {
    let value_style = color.map_or_else(Style::default, |c| Style::default().fg(c));
    Line::from(vec![
        Span::styled(format!("{key:<11}"), Style::default().fg(DIM)),
        Span::styled(value.into(), value_style),
    ])
}

fn flag(item: &Item, pointer: &str) -> bool {
    item.attrs.pointer(pointer).and_then(Value::as_bool) == Some(true)
}

/// Dependabot PRs are recognisable by their author, or by the `Bump ...` title it writes.
fn is_dependabot(item: &Item) -> bool {
    text(item, "/author").contains("dependabot") || item.title.starts_with("Bump ")
}

/// Tag, colour and dot for one of your own PRs, from how its reviews are going.
fn own_pr_state(item: &Item) -> (&'static str, Color, Dot) {
    match text(item, "/review_decision").as_str() {
        "APPROVED" => ("approved", GREEN, Dot::new('●', GREEN)),
        "CHANGES_REQUESTED" => ("changes", RED, Dot::new('●', RED)),
        _ if !list(item, "/reviews").is_empty() => ("reviewed", YELLOW, Dot::new('◐', YELLOW)),
        _ => ("waiting", DIM, Dot::new('○', DIM)),
    }
}

fn ci_color(ci: &str) -> Option<Color> {
    match ci {
        "success" => Some(GREEN),
        "failure" => Some(RED),
        "pending" => Some(YELLOW),
        _ => Some(DIM),
    }
}

/// `login`, with `(bot)` for automated authors.
fn author_label(item: &Item) -> String {
    let login = text(item, "/author");
    if item
        .attrs
        .pointer("/author_is_bot")
        .and_then(Value::as_bool)
        == Some(true)
    {
        format!("{login} (bot)")
    } else {
        login
    }
}

fn labels_label(item: &Item) -> String {
    let labels = list(item, "/labels");
    if labels.is_empty() {
        "none".to_owned()
    } else {
        labels
    }
}

fn first_name(full: &str) -> &str {
    full.split([' ', '@']).next().unwrap_or(full)
}

struct PullRequestView;

impl View for PullRequestView {
    fn handles(&self, item: &Item) -> bool {
        item.kind.as_str().starts_with("pr.")
    }

    fn row(&self, item: &Item) -> RowParts {
        let (tag, tag_color, dot) = if flag(item, "/mine") {
            own_pr_state(item)
        } else if flag(item, "/rereview") {
            ("re-review", YELLOW, Dot::new('◆', YELLOW))
        } else if is_dependabot(item) {
            ("deps", MAGENTA, Dot::new('●', MAGENTA))
        } else if item.kind.as_str() == "pr.review" {
            ("review", BLUE, Dot::new('●', BLUE))
        } else {
            (
                item.kind.as_str().trim_start_matches("pr."),
                Color::Reset,
                Dot::new('●', DIM),
            )
        };
        let repo = text(item, "/repo");
        let short = repo.rsplit('/').next().unwrap_or_default();
        let ci_failed = text(item, "/ci") == "failure";
        RowParts {
            dot,
            tag: tag.to_owned(),
            tag_color,
            reference: format!("{short}#{}", text(item, "/number")),
            title: if ci_failed {
                format!("✗ {}", item.title)
            } else {
                item.title.clone()
            },
            age: item.updated_at,
        }
    }

    fn detail(&self, item: &Item) -> Vec<Line<'static>> {
        let mut lines = vec![
            title_line(&item.title),
            dim_line(&format!(
                "{}#{}",
                text(item, "/repo"),
                text(item, "/number")
            )),
            Line::default(),
            kv(
                "author",
                author_label(item),
                is_dependabot(item).then_some(MAGENTA),
            ),
            kv("labels", labels_label(item), None),
            kv("comments", text(item, "/comments"), None),
            kv(
                "head",
                text(item, "/head_sha").chars().take(10).collect::<String>(),
                None,
            ),
        ];
        lines.push(kv("ci", text(item, "/ci"), ci_color(&text(item, "/ci"))));
        lines.push(Line::default());
        lines.push(heading("REVIEWS"));
        if flag(item, "/rereview") {
            let newer = if flag(item, "/new_commits_since_my_review") {
                ", new commits since"
            } else {
                ""
            };
            lines.push(kv(
                "re-review",
                format!("you reviewed ({}){newer}", text(item, "/my_review")),
                Some(YELLOW),
            ));
        }
        let decision = text(item, "/review_decision");
        if !decision.is_empty() {
            lines.push(kv("decision", decision.clone(), decision_color(&decision)));
        }
        let reviews = list_items(item, "/reviews");
        if reviews.is_empty() {
            lines.push(dim_line("none yet"));
        }
        lines.extend(reviews.iter().map(|r| Line::from(format!("  {r}"))));
        lines.push(Line::default());
        lines.push(kv("url", item.url.clone(), Some(DIM)));
        lines
    }
}

fn decision_color(decision: &str) -> Option<Color> {
    match decision {
        "APPROVED" => Some(GREEN),
        "CHANGES_REQUESTED" => Some(RED),
        _ => None,
    }
}

fn list_items(item: &Item, pointer: &str) -> Vec<String> {
    item.attrs
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn state_color(state: &str) -> Color {
    match state {
        "OPEN" => RED,
        "WIP" => YELLOW,
        "CLOSED" => GREEN,
        _ => Color::Reset,
    }
}

fn assigned_label(item: &Item) -> String {
    let who = list(item, "/assignees");
    if who.is_empty() {
        "nobody".to_owned()
    } else {
        who
    }
}

fn assigned_color(item: &Item) -> Option<Color> {
    list_items(item, "/assignees").is_empty().then_some(DIM)
}

struct IncidentView;

impl View for IncidentView {
    fn handles(&self, item: &Item) -> bool {
        item.kind.as_str().starts_with("incident.")
    }

    fn row(&self, item: &Item) -> RowParts {
        let assigned = flag(item, "/assigned");
        let (tag, tag_color, dot) = match text(item, "/state").as_str() {
            "WIP" => ("wip", YELLOW, Dot::new('◐', YELLOW)),
            "CLOSED" => ("closed", GREEN, Dot::new('○', GREEN)),
            _ if assigned => ("err", RED, Dot::new('◐', YELLOW)),
            _ => ("err", RED, Dot::new('●', RED)),
        };
        let assignee = match list_items(item, "/assignees").first() {
            Some(name) => format!(" @{}", first_name(name)),
            None => String::new(),
        };
        RowParts {
            dot,
            tag: tag.to_owned(),
            tag_color,
            reference: format!("{}#{}", text(item, "/app"), text(item, "/number")),
            title: format!("{} · {}{assignee}", item.title, text(item, "/namespace")),
            age: item.updated_at,
        }
    }

    fn detail(&self, item: &Item) -> Vec<Line<'static>> {
        let mut lines = vec![
            title_line(&item.title),
            dim_line(&text(item, "/message")),
            Line::default(),
            kv(
                "app",
                format!("{} · {}", text(item, "/app"), text(item, "/environment")),
                None,
            ),
            kv(
                "state",
                text(item, "/state"),
                Some(state_color(&text(item, "/state"))),
            ),
            kv("assigned", assigned_label(item), assigned_color(item)),
            kv("namespace", text(item, "/namespace"), None),
            kv("action", list(item, "/actions"), None),
            kv("count", text(item, "/count"), None),
            kv("severity", text(item, "/severity"), None),
            kv("last seen", text(item, "/last_occurred_at"), None),
        ];
        let backtrace = text(item, "/first_backtrace_line");
        if !backtrace.is_empty() {
            lines.push(Line::default());
            lines.push(heading("BACKTRACE"));
            lines.push(dim_line(&format!("  {backtrace}")));
        }
        lines
    }
}

/// Fallback for any kind without its own view: id, title and the top-level attributes.
struct GenericView;

impl View for GenericView {
    fn handles(&self, _: &Item) -> bool {
        true
    }

    fn row(&self, item: &Item) -> RowParts {
        RowParts {
            dot: Dot::new('•', DIM),
            tag: item.kind.to_string(),
            tag_color: Color::Reset,
            reference: item.id.to_string(),
            title: item.title.clone(),
            age: item.updated_at,
        }
    }

    fn detail(&self, item: &Item) -> Vec<Line<'static>> {
        let mut lines = vec![
            title_line(&item.title),
            dim_line(item.id.as_str()),
            Line::default(),
        ];
        if let Some(map) = item.attrs.as_object() {
            lines.extend(map.iter().map(|(k, v)| {
                let shown = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                kv(k, shown, None)
            }));
        }
        lines
    }
}

/// Compact and at most four characters: `30s`, `5m`, `2h`, `3d`, `12w`, `5y`.
pub fn age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds().max(0);
    let days = secs / 86_400;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ if days < 100 => format!("{days}d"),
        _ if days < 365 => format!("{}w", days / 7),
        _ => format!("{}y", days / 365),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{Fingerprint, ItemId, ItemKind, SourceId};

    fn item(kind: &str, attrs: Value) -> Item {
        Item {
            id: ItemId::new("x:1"),
            source: SourceId::new("x"),
            kind: ItemKind::new(kind),
            title: "Title".to_owned(),
            url: "https://u".to_owned(),
            fingerprint: Fingerprint::new("f"),
            updated_at: DateTime::UNIX_EPOCH,
            attrs,
        }
    }

    #[test]
    fn pr_row_uses_short_repo_and_number() {
        let pr = item(
            "pr.review",
            json!({"repo": "appsignal/appsignal-server", "number": 12}),
        );
        let row = view_for(&pr).row(&pr);
        assert_eq!(row.reference, "appsignal-server#12");
        assert_eq!(row.tag, "review");
    }

    #[test]
    fn incident_row_combines_name_and_namespace() {
        let inc = item(
            "incident.exception",
            json!({"app": "AppSignal", "number": 7, "namespace": "web"}),
        );
        let row = view_for(&inc).row(&inc);
        assert_eq!(row.reference, "AppSignal#7");
        assert_eq!(row.title, "Title · web");
    }

    #[test]
    fn dependabot_prs_are_tagged_and_show_bot_author_and_labels() {
        let pr = item(
            "pr.review",
            json!({"repo": "appsignal/appsignal-server", "number": 3, "author": "dependabot[bot]",
                   "author_is_bot": true, "labels": ["dependencies", "ruby"], "query": "reviews"}),
        );
        let view = view_for(&pr);
        assert_eq!(view.row(&pr).tag, "deps");
        let detail: Vec<String> = view.detail(&pr).iter().map(ToString::to_string).collect();
        assert!(
            detail.iter().any(|l| l.contains("dependabot[bot] (bot)")),
            "{detail:?}"
        );
        assert!(
            detail.iter().any(|l| l.contains("dependencies, ruby")),
            "{detail:?}"
        );
    }

    #[test]
    fn human_prs_say_when_they_have_no_labels() {
        let pr = item(
            "pr.review",
            json!({"repo": "a/b", "number": 4, "author": "alex", "author_is_bot": false, "labels": []}),
        );
        assert_eq!(view_for(&pr).row(&pr).tag, "review");
        let detail: Vec<String> = view_for(&pr)
            .detail(&pr)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            detail
                .iter()
                .any(|l| l.starts_with("labels") && l.contains("none")),
            "{detail:?}"
        );
    }

    #[test]
    fn incidents_show_state_and_assignees() {
        let attrs = json!({"app": "AppSignal", "number": 7, "namespace": "web", "state": "WIP",
                           "assignees": ["Thijs Cadier", "sam@example.com"]});
        let inc = item("incident.exception", attrs);
        let row = view_for(&inc).row(&inc);
        assert_eq!(row.tag, "wip");
        assert_eq!(row.title, "Title · web @Thijs");
        let detail: Vec<String> = view_for(&inc)
            .detail(&inc)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            detail
                .iter()
                .any(|l| l.starts_with("state") && l.contains("WIP"))
        );
        assert!(
            detail
                .iter()
                .any(|l| l.starts_with("assigned") && l.contains("Thijs Cadier, sam@example.com"))
        );
    }

    #[test]
    fn unassigned_open_incidents_read_as_nobody() {
        let inc = item(
            "incident.exception",
            json!({"app": "A", "number": 1, "namespace": "web", "state": "OPEN", "assignees": []}),
        );
        let row = view_for(&inc).row(&inc);
        assert_eq!(
            (row.tag.as_str(), row.title.as_str()),
            ("err", "Title · web")
        );
        let detail: Vec<String> = view_for(&inc)
            .detail(&inc)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            detail
                .iter()
                .any(|l| l.starts_with("assigned") && l.contains("nobody"))
        );
    }

    #[test]
    fn unknown_kinds_fall_back_to_the_generic_view() {
        let other = item("slack.mention", json!({"channel": "ops"}));
        let view = view_for(&other);
        assert_eq!(view.row(&other).reference, "x:1");
        let detail = view.detail(&other);
        assert!(detail.iter().any(|l| l.to_string().contains("channel")));
    }

    #[test]
    fn age_is_compact() {
        let t = DateTime::UNIX_EPOCH;
        let after = |s| t + chrono::Duration::seconds(s);
        assert_eq!(age(after(30), t), "30s");
        assert_eq!(age(after(300), t), "5m");
        assert_eq!(age(after(7200), t), "2h");
        assert_eq!(age(after(172_800), t), "2d");
        assert_eq!(age(after(100 * 86_400), t), "14w");
        assert_eq!(age(after(1464 * 86_400), t), "4y");
        assert!(
            age(after(40_000 * 86_400), t).chars().count() <= 4,
            "always fits the column"
        );
    }
}

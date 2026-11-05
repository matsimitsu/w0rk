//! Declarative routing: predicates over [`Item`] plus rules that map matching events to profiles.
//!
//! ```toml
//! [[rule]]
//! name = "new-incidents"
//! trigger = "new"                      # or "changed"
//! when = { kind = "incident.exception", attrs = { "/namespace" = { in = ["web"] } } }
//! spawn = "debug-incident"
//! mode = "auto"                        # or "suggest"
//! ```
//!
//! The same [`Predicate`] drives TUI sections and filters.

use std::collections::BTreeMap;

use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::model::{Fingerprint, Item, ItemEvent, ProfileName};

/// A regex compiled when the config is loaded, so typos fail at startup.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "String")]
pub struct RegexPattern {
    source: String,
    regex: Regex,
}

impl TryFrom<String> for RegexPattern {
    type Error = regex::Error;

    fn try_from(source: String) -> Result<Self, Self::Error> {
        let regex = Regex::new(&source)?;
        Ok(Self { source, regex })
    }
}

impl PartialEq for RegexPattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

/// A glob compiled when the config is loaded.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "String")]
pub struct GlobPattern {
    source: String,
    matcher: GlobMatcher,
}

impl TryFrom<String> for GlobPattern {
    type Error = globset::Error;

    fn try_from(source: String) -> Result<Self, Self::Error> {
        let matcher = Glob::new(&source)?.compile_matcher();
        Ok(Self { source, matcher })
    }
}

impl PartialEq for GlobPattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

/// A JSON pointer into `Item::attrs`, e.g. `/namespace`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct AttrPointer(String);

impl TryFrom<String> for AttrPointer {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.starts_with('/') {
            Ok(Self(value))
        } else {
            Err(format!("attribute pointer `{value}` must start with `/`"))
        }
    }
}

/// A plain value to compare for equality. Tables are not allowed, so a mistyped operator name
/// is an error instead of a comparison that silently never matches.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Literal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl Literal {
    fn to_value(&self) -> Value {
        match self {
            Self::Bool(b) => Value::from(*b),
            Self::Int(i) => Value::from(*i),
            Self::Float(f) => Value::from(*f),
            Self::Text(s) => Value::from(s.as_str()),
        }
    }
}

/// Operators on one value. Every operator that is present must hold.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Op {
    #[serde(rename = "in")]
    one_of: Option<Vec<Literal>>,
    glob: Option<GlobPattern>,
    regex: Option<RegexPattern>,
    gt: Option<f64>,
    lt: Option<f64>,
    /// Array contains the element, or string contains the substring.
    contains: Option<Literal>,
    exists: Option<bool>,
}

impl Op {
    fn matches(&self, found: Option<&Value>) -> bool {
        let Some(value) = found else {
            return self.exists == Some(false);
        };
        self.exists != Some(false)
            && self
                .one_of
                .as_ref()
                .is_none_or(|options| options.iter().any(|o| o.to_value() == *value))
            && self
                .glob
                .as_ref()
                .is_none_or(|g| value.as_str().is_some_and(|s| g.matcher.is_match(s)))
            && self
                .regex
                .as_ref()
                .is_none_or(|r| value.as_str().is_some_and(|s| r.regex.is_match(s)))
            && self
                .gt
                .is_none_or(|gt| value.as_f64().is_some_and(|n| n > gt))
            && self
                .lt
                .is_none_or(|lt| value.as_f64().is_some_and(|n| n < lt))
            && self.contains.as_ref().is_none_or(|needle| {
                let needle = needle.to_value();
                match value {
                    Value::Array(items) => items.contains(&needle),
                    Value::String(text) => needle.as_str().is_some_and(|n| text.contains(n)),
                    _ => false,
                }
            })
    }
}

/// Either a plain value (equality) or a table of operators.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Matcher {
    Op(Box<Op>),
    Equals(Literal),
}

impl Matcher {
    fn matches(&self, found: Option<&Value>) -> bool {
        match self {
            Self::Op(op) => op.matches(found),
            Self::Equals(literal) => found.is_some_and(|v| *v == literal.to_value()),
        }
    }
}

/// All present conditions must hold. An empty predicate matches everything.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Predicate {
    kind: Option<Matcher>,
    source: Option<Matcher>,
    #[serde(default)]
    attrs: BTreeMap<AttrPointer, Matcher>,
    /// If non-empty, at least one of these must match.
    #[serde(default)]
    any: Vec<Predicate>,
    not: Option<Box<Predicate>>,
}

impl Predicate {
    pub fn matches(&self, item: &Item) -> bool {
        let kind = Value::from(item.kind.as_str());
        let source = Value::from(item.source.as_str());
        self.kind.as_ref().is_none_or(|m| m.matches(Some(&kind)))
            && self
                .source
                .as_ref()
                .is_none_or(|m| m.matches(Some(&source)))
            && self
                .attrs
                .iter()
                .all(|(pointer, m)| m.matches(item.attrs.pointer(&pointer.0)))
            && (self.any.is_empty() || self.any.iter().any(|p| p.matches(item)))
            && self.not.as_ref().is_none_or(|p| !p.matches(item))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The item appeared (or reappeared).
    #[default]
    New,
    /// The item's fingerprint changed, e.g. new commits on a PR.
    Changed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Spawn right away.
    Auto,
    /// Only offer it in the TUI; the user presses `s`.
    #[default]
    Suggest,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default)]
    pub when: Predicate,
    pub spawn: ProfileName,
    #[serde(default)]
    pub mode: Mode,
}

impl Rule {
    fn fires_on<'a>(&self, event: &'a ItemEvent) -> Option<(&'a Item, Option<&'a Fingerprint>)> {
        match (self.trigger, event) {
            (Trigger::New, ItemEvent::New(item)) => Some((item, None)),
            (Trigger::Changed, ItemEvent::Changed { item, previous }) => {
                Some((item, Some(previous)))
            }
            _ => None,
        }
    }
}

/// A rule that matched an event.
#[derive(Debug, Clone, PartialEq)]
pub struct Route<'a> {
    pub rule: &'a Rule,
    pub item: &'a Item,
    /// The fingerprint before the change, for `changed` triggers.
    pub previous: Option<&'a Fingerprint>,
}

/// Every rule that matches `event`, in config order.
pub fn route<'a>(rules: &'a [Rule], event: &'a ItemEvent) -> Vec<Route<'a>> {
    rules
        .iter()
        .filter_map(|rule| {
            let (item, previous) = rule.fires_on(event)?;
            rule.when.matches(item).then_some(Route {
                rule,
                item,
                previous,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use serde_json::json;

    use super::*;
    use crate::model::{ItemId, ItemKind, SourceId};

    fn item(kind: &str, attrs: Value) -> Item {
        Item {
            id: ItemId::new("x"),
            source: SourceId::new("github"),
            kind: ItemKind::new(kind),
            title: String::new(),
            url: String::new(),
            fingerprint: Fingerprint::new("2"),
            updated_at: DateTime::UNIX_EPOCH,
            attrs,
        }
    }

    fn predicate(toml_text: &str) -> Predicate {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn empty_predicate_matches_everything() {
        assert!(Predicate::default().matches(&item("any", Value::Null)));
    }

    #[test]
    fn kind_and_literal_attrs() {
        let p = predicate(
            r#"kind = "pr.review"
            attrs = { "/draft" = false, "/repo" = "appsignal/server" }"#,
        );
        assert!(p.matches(&item(
            "pr.review",
            json!({"draft": false, "repo": "appsignal/server"})
        )));
        assert!(!p.matches(&item(
            "pr.review",
            json!({"draft": true, "repo": "appsignal/server"})
        )));
        assert!(!p.matches(&item(
            "pr.other",
            json!({"draft": false, "repo": "appsignal/server"})
        )));
        assert!(
            !p.matches(&item("pr.review", json!({"repo": "appsignal/server"}))),
            "missing attr"
        );
    }

    #[test]
    fn operators() {
        let p = predicate(
            r#"attrs = { "/namespace" = { in = ["web", "background"] },
                         "/count" = { gt = 10 },
                         "/author" = { glob = "dependabot*" },
                         "/message" = { regex = "^Too many" },
                         "/labels" = { contains = "dependencies" } }"#,
        );
        let good = json!({"namespace": "web", "count": 11, "author": "dependabot[bot]",
                          "message": "Too many alerts", "labels": ["dependencies"]});
        assert!(p.matches(&item("k", good.clone())));

        for (key, bad) in [
            ("namespace", json!("admin")),
            ("count", json!(10)),
            ("author", json!("alex")),
            ("message", json!("A Too many")),
            ("labels", json!(["wip"])),
        ] {
            let mut attrs = good.clone();
            attrs[key] = bad;
            assert!(!p.matches(&item("k", attrs)), "{key} should fail");
        }
    }

    #[test]
    fn exists_checks_presence() {
        let absent = predicate(r#"attrs = { "/x" = { exists = false } }"#);
        assert!(absent.matches(&item("k", json!({}))));
        assert!(!absent.matches(&item("k", json!({"x": 1}))));
    }

    #[test]
    fn any_and_not() {
        let p = predicate(
            r#"any = [ { kind = "a" }, { kind = "b" } ]
               not = { attrs = { "/muted" = true } }"#,
        );
        assert!(p.matches(&item("a", json!({}))));
        assert!(p.matches(&item("b", json!({}))));
        assert!(!p.matches(&item("c", json!({}))));
        assert!(!p.matches(&item("a", json!({"muted": true}))));
    }

    #[test]
    fn bad_config_is_rejected_at_parse_time() {
        assert!(toml::from_str::<Predicate>(r#"attrs = { "/x" = { regex = "(" } }"#).is_err());
        assert!(
            toml::from_str::<Predicate>(r#"attrs = { "x" = 1 }"#).is_err(),
            "pointer needs /"
        );
        assert!(
            toml::from_str::<Predicate>(r#"attrs = { "/x" = { inn = ["a"] } }"#).is_err(),
            "typo'd operator"
        );
        assert!(toml::from_str::<Predicate>(r#"colour = "red""#).is_err());
    }

    fn rules() -> Vec<Rule> {
        #[derive(Deserialize)]
        struct Wrapper {
            rule: Vec<Rule>,
        }
        toml::from_str::<Wrapper>(
            r#"
            [[rule]]
            name = "review"
            when = { kind = "pr.review" }
            spawn = "code-review"
            mode = "auto"

            [[rule]]
            name = "re-review"
            trigger = "changed"
            when = { kind = "pr.review" }
            spawn = "re-review"
            "#,
        )
        .unwrap()
        .rule
    }

    #[test]
    fn new_and_changed_triggers_route_separately() {
        let rules = rules();
        let pr = item("pr.review", json!({}));

        let new = ItemEvent::New(pr.clone());
        let routed = route(&rules, &new);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].rule.name, "review");
        assert_eq!(routed[0].rule.mode, Mode::Auto);
        assert_eq!(routed[0].previous, None);

        let previous = Fingerprint::new("1");
        let changed = ItemEvent::Changed {
            item: pr,
            previous: previous.clone(),
        };
        let routed = route(&rules, &changed);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].rule.name, "re-review");
        assert_eq!(
            routed[0].rule.mode,
            Mode::Suggest,
            "mode defaults to suggest"
        );
        assert_eq!(routed[0].previous, Some(&previous));
    }

    #[test]
    fn gone_events_never_route() {
        assert!(route(&rules(), &ItemEvent::Gone(ItemId::new("x"))).is_empty());
    }
}

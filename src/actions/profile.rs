//! `[profile.*]`: what to run for a routed item. Prompt and cwd are minijinja templates over
//! `item` (the full normalized item) and `previous` (the fingerprint before a `changed` event).

use std::path::{Path, PathBuf};

use minijinja::{Environment, UndefinedBehavior, Value, context};
use serde::Deserialize;
use thiserror::Error;

use super::RunSpec;
use crate::model::{Fingerprint, Item, ProfileName};

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("profile `{profile}`: {source}")]
    Template {
        profile: ProfileName,
        source: minijinja::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Template for the prompt. With `skill` set it becomes the skill's argument.
    pub prompt: String,
    /// Run `/skill <prompt>` instead of a bare prompt.
    pub skill: Option<String>,
    /// Template for the working directory; a leading `~/` is expanded.
    pub cwd: String,
    /// Check the PR out into its own git worktree first.
    #[serde(default)]
    pub worktree: bool,
    /// Template for a git ref to fetch from `origin` before creating the worktree, e.g.
    /// `pull/{{ item.attrs.number }}/head`. Without it the worktree starts at the repo's HEAD.
    pub fetch_ref: Option<String>,
    /// Extra arguments for `claude`, e.g. `["--permission-mode", "acceptEdits"]`.
    #[serde(default)]
    pub claude_args: Vec<String>,
}

/// Renders profiles. Undefined variables are errors, so a typo never silently produces an empty prompt.
pub struct Renderer {
    env: Environment<'static>,
    home: PathBuf,
}

impl Renderer {
    pub fn new(home: &Path) -> Self {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.add_filter("basename", |path: String| {
            path.rsplit('/').next().unwrap_or_default().to_owned()
        });
        Self {
            env,
            home: home.to_owned(),
        }
    }

    /// Check that a profile's templates at least parse.
    pub fn check(&self, name: &ProfileName, profile: &Profile) -> Result<(), ProfileError> {
        let templates = [&profile.prompt, &profile.cwd]
            .into_iter()
            .chain(profile.fetch_ref.as_ref());
        for template in templates {
            self.env
                .template_from_str(template)
                .map_err(|source| ProfileError::Template {
                    profile: name.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    pub fn render(
        &self,
        name: &ProfileName,
        profile: &Profile,
        item: &Item,
        previous: Option<&Fingerprint>,
    ) -> Result<RunSpec, ProfileError> {
        let wrap = |source| ProfileError::Template {
            profile: name.clone(),
            source,
        };
        let ctx = context! {
            item => Value::from(minijinja::value::Serde(item)),
            previous => previous.map(Fingerprint::as_str),
        };
        let body = self.env.render_str(&profile.prompt, &ctx).map_err(wrap)?;
        let cwd = self.env.render_str(&profile.cwd, &ctx).map_err(wrap)?;
        let fetch_ref = profile
            .fetch_ref
            .as_ref()
            .map(|t| self.env.render_str(t, &ctx).map_err(wrap))
            .transpose()?;
        let prompt = match &profile.skill {
            Some(skill) => format!("/{skill} {body}").trim_end().to_owned(),
            None => body,
        };
        Ok(RunSpec {
            item_id: item.id.clone(),
            fingerprint: item.fingerprint.clone(),
            profile: name.clone(),
            label: item.id.to_string(),
            cwd: self.expand_home(&cwd),
            worktree: profile.worktree,
            fetch_ref,
            prompt,
            claude_args: profile.claude_args.clone(),
        })
    }

    fn expand_home(&self, path: &str) -> PathBuf {
        match path.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            None => PathBuf::from(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use serde_json::json;

    use super::*;
    use crate::model::{ItemId, ItemKind, SourceId};

    fn item() -> Item {
        Item {
            id: ItemId::new("github:appsignal/appsignal-server#1"),
            source: SourceId::new("github"),
            kind: ItemKind::new("pr.review"),
            title: "Fix it".to_owned(),
            url: "https://github.com/appsignal/appsignal-server/pull/1".to_owned(),
            fingerprint: Fingerprint::new("bbb"),
            updated_at: DateTime::UNIX_EPOCH,
            attrs: json!({"repo": "appsignal/appsignal-server", "number": 7}),
        }
    }

    fn profile(toml_text: &str) -> Profile {
        toml::from_str(toml_text).unwrap()
    }

    fn name() -> ProfileName {
        ProfileName::new("code-review")
    }

    #[test]
    fn renders_skill_prompt_and_home_relative_cwd() {
        let p = profile(
            r#"skill = "code-review"
            prompt = "{{ item.url }}"
            cwd = "~/Sites/appsignal/{{ item.attrs.repo | basename }}"
            worktree = true
            fetch_ref = "pull/{{ item.attrs.number }}/head"
            claude_args = ["--permission-mode", "plan"]"#,
        );
        let run = Renderer::new(Path::new("/home/me"))
            .render(&name(), &p, &item(), None)
            .unwrap();

        assert_eq!(
            run.prompt,
            "/code-review https://github.com/appsignal/appsignal-server/pull/1"
        );
        assert_eq!(
            run.cwd,
            Path::new("/home/me/Sites/appsignal/appsignal-server")
        );
        assert!(run.worktree);
        assert_eq!(run.fetch_ref.as_deref(), Some("pull/7/head"));
        assert_eq!(run.claude_args, ["--permission-mode", "plan"]);
        assert_eq!(run.item_id.as_str(), "github:appsignal/appsignal-server#1");
    }

    #[test]
    fn previous_fingerprint_is_available_for_delta_reviews() {
        let p = profile(
            r#"prompt = "{% if previous %}Only review {{ previous }}..{{ item.fingerprint }}{% else %}Full review{% endif %}"
            cwd = "/tmp""#,
        );
        let r = Renderer::new(Path::new("/h"));
        let before = Fingerprint::new("aaa");
        assert_eq!(
            r.render(&name(), &p, &item(), Some(&before))
                .unwrap()
                .prompt,
            "Only review aaa..bbb"
        );
        assert_eq!(
            r.render(&name(), &p, &item(), None).unwrap().prompt,
            "Full review"
        );
    }

    #[test]
    fn undefined_variables_are_errors() {
        let p = profile("prompt = \"{{ item.nope }}\"\ncwd = \"/tmp\"");
        let err = Renderer::new(Path::new("/h"))
            .render(&name(), &p, &item(), None)
            .unwrap_err();
        assert!(err.to_string().contains("code-review"));
    }

    #[test]
    fn check_catches_syntax_errors() {
        let p = profile("prompt = \"{{ \"\ncwd = \"/tmp\"");
        assert!(Renderer::new(Path::new("/h")).check(&name(), &p).is_err());
    }
}

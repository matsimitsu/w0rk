# w0rk

A terminal UI that pulls your notifications through their CLIs (`gh`, `appsignal-cli`), routes them
with rules, and starts Claude Code agents for them in Rex panes.

- **To review**: review requests, Dependabot updates and re-reviews, tagged and dotted by state.
- **My pull requests**: your open PRs with their review status.
- **Incidents**: AppSignal exceptions in the apps and namespaces you choose, optionally only the ones
  first seen within a rolling window (`new_within_hours`), like AppSignal's "new" filter.
- **Agents**: every agent w0rk started, with live state (running, waiting on you, done) and output.

## Install

Requires a Rust toolchain, plus `gh`, `appsignal-cli`, `rex` and `claude` on your `PATH`.

```sh
# straight from GitHub
cargo install --git https://github.com/matsimitsu/w0rk --locked

# or from a checkout
git clone https://github.com/matsimitsu/w0rk && cd w0rk
make install     # cargo install --path . --locked

w0rk doctor      # checks gh, appsignal-cli, rex, claude and your config
w0rk
```

Both put `w0rk` in `~/.cargo/bin`. Keep that on your `PATH`: agents call back into the installed
binary for their hooks, so reinstall after changing w0rk.

Prebuilt binaries (`cargo binstall`) and a Homebrew tap are not available yet: both need tagged GitHub
releases with built artifacts.

## Configure

Copy `config.example.toml` to `~/.config/w0rk/config.toml` and edit it. Everything lives there:
sources, `[profile.*]` (what an agent is told), `[[rule]]` (which items offer or start which profile)
and optional `[[section]]`s. State is kept in `~/.local/state/w0rk/w0rk.db`.

### Examples

Poll your review requests and open PRs (needs `gh` logged in; no tokens go in the config):

```toml
[source.github]
poll_seconds = 60

[[source.github.query]]
name = "review-requests"
kind = "pr.review"
role = "review-requested"   # review-requested | assignee | author
owners = ["my-org"]
exclude_labels = ["wip"]
```

Watch AppSignal incidents (needs `appsignal-cli` authenticated):

```toml
[[source.appsignal.app]]
name = "My App"
environment = "production"
namespaces = ["web", "background"]
states = ["OPEN", "WIP"]
new_within_hours = 24
```

Tell an agent what to do, and which items offer it:

```toml
[profile.code-review]
prompt = "Use the code-review skill on {{ item.url }}."
cwd = "~/Sites/{{ item.attrs.repo | basename }}"
worktree = true
fetch_ref = "pull/{{ item.attrs.number }}/head"

[[rule]]
name = "review-requests"
trigger = "new"
when = { kind = "pr.review" }
spawn = "code-review"
mode = "suggest"            # suggest = press `s`, auto = start right away
```

See `config.example.toml` for the full set (re-reviews, Dependabot, incidents, sections).

Rules default to `mode = "suggest"`: the item shows up and you press `s`. Switch a rule to `auto` to
start agents on their own for items that arrive while w0rk is running.

## Keys

`j/k` move · `Tab` next section · `⏎` open · `s` run a profile · `p` ask your own prompt ·
`g` show the agent in Rex · `y/n` answer a waiting agent · `x` dismiss (until it changes) ·
`i` ignore for good · `I` show ignored · `u` undo · `f` filter namespaces · `r` sync · `?` help (with
the dot legend) · `q` quit. The mouse works too.

## Development

```sh
cargo nextest run
cargo clippy --all-targets -- -D warnings
```

See `CLAUDE.md` for the design principles.

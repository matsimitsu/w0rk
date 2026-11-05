# cockpit

A terminal UI that pulls your notifications through their CLIs (`gh`, `appsignal-cli`), routes them
with rules, and starts Claude Code agents for them in Rex panes.

- **To review**: review requests, Dependabot updates and re-reviews, tagged and dotted by state.
- **My pull requests**: your open PRs with their review status.
- **Incidents**: AppSignal exceptions in the apps and namespaces you choose, optionally only the ones
  that are new since cockpit last ran.
- **Agents**: every agent cockpit started, with live state (running, waiting on you, done) and output.

## Install

```sh
cargo install --path .
cockpit doctor   # checks gh, appsignal-cli, rex, claude and your config
cockpit
```

Agents call back into the installed binary for their hooks, so reinstall after changing cockpit and
keep it on your `PATH` (`~/.cargo/bin`).

## Configure

Copy `config.example.toml` to `~/.config/cockpit/config.toml` and edit it. Everything lives there:
sources, `[profile.*]` (what an agent is told), `[[rule]]` (which items offer or start which profile)
and optional `[[section]]`s. State is kept in `~/.local/state/cockpit/cockpit.db`.

Rules default to `mode = "suggest"`: the item shows up and you press `s`. Switch a rule to `auto` to
start agents on their own for items that arrive while cockpit is running.

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

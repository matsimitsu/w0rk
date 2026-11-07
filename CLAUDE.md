# W0rk

A Rust TUI that polls notification sources through their CLIs (`gh`, `appsignal-cli`), routes items with
config-defined rules, and spawns Claude Code agents in Rex panes. No API clients: external tools are only
ever invoked through `CliRunner` (read) and `Spawner` (act).

Plan: `~/.claude/plans/i-want-to-make-replicated-octopus.md`. Design mock (option A): see the plan's UI section.

## Architecture: four systems. Extend them, don't add one-off code

1. **Source** (`src/sources/`): `CliRunner` output → normalized `Item`. A new source is one adapter.
2. **Rules** (`src/rules.rs`): declarative predicates over `Item`. Also drives TUI sections and filters.
3. **Profile + Spawner** (`src/actions/`): what to run for a match, and how (Rex).
4. **View** (`src/ui/views`): per `(source, kind)` row/detail rendering, with a generic fallback.

New slices (re-review delta, Dependabot merge, new sources) must be config plus an adapter, not new plumbing.

## Principles

- **RUST-1 Newtypes over primitives.** Domain values get their own type (`ItemId`, `Fingerprint`, `RepoSlug`,
  `ProfileName`, `BlockId`, `RunId`, `Namespace`, ...). No bare `String`/`u64` in domain signatures.
- **RUST-2 No function takes two `String`/`&str` arguments.** Use newtypes (or a params struct) so arguments
  can't be swapped. The same goes for two bare numbers or bools.
- **RUST-3 Idiomatic Rust.** Clippy clean (`-D warnings`), `thiserror` for module errors and `anyhow` only at
  the binary edge, iterators over index loops, `From`/`TryFrom`/`FromStr` for conversions, enums over
  stringly-typed state, no `unwrap`/`expect` outside tests.
- **TEST-1 Test with `cargo nextest run`**, not `cargo test` (doctests: `cargo test --doc`). Tests never shell
  out to `gh`, `appsignal-cli` or `rex`; they use fixtures through the `CliRunner`/`Spawner` traits.

## Commands

- `cargo nextest run`
- `cargo clippy --all-targets -- -D warnings`
- `cargo fmt`

## Config

One hand-edited TOML file: `~/.config/w0rk/config.toml`. No config UI. State: `~/.local/state/w0rk/w0rk.db`.

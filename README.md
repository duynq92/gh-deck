# ghdeck

Keyboard-driven terminal UI for GitHub pull requests, issues, releases and
Actions, across all your repositories. Every label shows its shortcut.

## Install

Requires a signed-in [GitHub CLI](https://cli.github.com) (`gh auth login`) with
access to this repository. macOS and Linux:

```sh
gh extension install duynq92/gh-deck
gh deck              # all your repositories
gh deck owner/repo   # one repository
gh extension upgrade deck
```

Or build the `ghdeck` command from source with Rust 1.90+:
`cargo install --path . --locked`.

## Release

Set `version` in `Cargo.toml` (and run `cargo build` to update `Cargo.lock`), merge,
then push a matching tag such as `v0.1.0`. The Release workflow builds and
publishes the binaries.

## Keys

Press `?` in the app for the current tab's shortcuts.

| Where | Keys |
|---|---|
| Everywhere | `1`-`4`/`Tab` tabs, `j`/`k` move, `g`/`G` top/end, `Enter` open, `Esc` back, `p` repo, `f` filter, `s` state, `/` find, `>` more, `R` refresh, `?` help, `q` quit |
| Pull requests | `i` info, `d` diff, `h` checks, `w` web, `a` approve, `x` request changes, `c` comment, `m` merge, `X` close/reopen, `b` checkout |
| Issues | `i` info, `w` web, `n` new, `c` comment, `l` labels, `a` assignees, `X` close/reopen |
| Releases | `i` info, `w` web, `D` download, `n` new, `e` edit, `P` publish, `X` delete |
| Actions | `i` jobs, `l` log, `L` failed log, `w` web, `r` rerun all, `F` rerun failed, `x` cancel, `t` trigger workflow |
| Forms | `Ctrl+S` submit, `Tab` next field, `Esc` cancel |

Close, delete, publish, rerun and cancel ask for `y` first. Merge and review
actions open a form that is only sent with `Ctrl+S`.

## Design

- No async runtime and no timers: the app blocks until a key or result arrives,
  so it uses no CPU while idle. Refresh is manual.
- All GitHub access runs `gh` (never through a shell); ghdeck never sees your token.
  `--jq` trims responses inside `gh` before they reach ghdeck.
- Logs keep their last 2 MiB and diffs their first 2 MiB.

## Limits

- The Actions tab without a chosen repository reads the 12 most recently pushed
  ones (one request each), 5 runs per repository.
- The Releases tab without a chosen repository shows the 3 newest releases of the
  50 most recently pushed ones.
- The repository picker lists the 100 most recently pushed; use
  `gh deck owner/repo` for others.
- `b` checkout only works when ghdeck was started inside a clone of that repository.

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
| Everywhere | `1`-`4`/`Tab` tabs, `j`/`k` move, `g`/`G` top/end, `Enter` open, `Esc` back, `o` organization, `p` repo, `f` filter, `s` state, `/` find, `>` more, `R` refresh, `?` help, `!` last message, `q` quit |
| Pull requests | `i` info, `d` diff, `n`/`N` next/previous file, `h` checks, `w` web, `a` approve, `x` request changes, `c` comment, `v` reviewers (your own PRs), `m` merge, `X` close/reopen, `b` checkout, `t` excluded teams |
| Issues | `i` info, `w` web, `n` new, `c` comment, `l` labels, `a` assignees, `X` close/reopen |
| Releases | `i` info, `w` web, `D` download, `n` new, `e` edit, `P` publish, `X` delete |
| Actions | `i` jobs, `l` log, `L` failed log, `w` web, `r` rerun all, `F` rerun failed, `x` cancel, `t` trigger workflow |
| Forms | `Ctrl+S` submit, `Tab` next field, `Esc` cancel |

Reviewer marks: `✓` approved, `✗` changes requested, `●` waiting, `·` commented,
`-` dismissed. The `f` filters for pull requests are: opened by me, review
requested from me, from any of my teams, or from one team, and involving me.

`o`, `p`, `f` and `s` take several choices: `Space` marks, `Ctrl+X`
clears, `Enter` applies. Choices
in one selector are alternatives; the selectors narrow each other. Without marks,
`Enter` chooses just the highlighted row.

Pull requests start with the filters opened by me, review requested from me,
involving me, and review requested from my teams. `t` leaves noisy teams out of
*my teams*; pull requests that ask you or another of your teams still show. The
list is saved in `~/.config/ghdeck/excluded-teams` (or `$XDG_CONFIG_HOME`).

Close, delete, publish, rerun, cancel and reviewer changes ask for `y` first. Merge and review
actions open a form that is only sent with `Ctrl+S`.

## Design

- No async runtime and no timers: the app blocks until a key or result arrives,
  so it uses no CPU while idle. Refresh is manual.
- All GitHub access runs `gh` (never through a shell); ghdeck never sees your token.
  `--jq` trims responses inside `gh` before they reach ghdeck.
- Logs keep their last 2 MiB and diffs their first 2 MiB.

## Limits

- Every list shows 50 rows; `>` loads more pull requests and issues.
- Without chosen repositories, Actions reads the 12 most recently pushed ones (one
  request each) and Releases the 3 newest releases of the 50 most recently pushed.
- All tabs include your organizations; `o` narrows to some or to your own repositories.
- Reviewers, approval and checks of pull requests appear a few seconds after the list.
- The repository picker lists the 500 most recently pushed; use
  `gh deck owner/repo` for others.
- `b` checkout only works when ghdeck was started inside a clone of that repository.

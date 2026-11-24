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

The bottom line shows what is useful right now. `.` lists every action by group: press
its letter, or move with the arrows and press `Enter`. `?` explains the current tab's shortcuts.

| Where | Keys |
|---|---|
| Everywhere | `1`-`5`/`Tab` tabs, `j`/`k` move, `g`/`G` top/end, `Enter` open, `Esc` back, `o` organization, `p` repo, `f` filter, `s` state, `/` find, `>` more, `R` refresh, `z` zoom, `.` all actions, `?` help, `!` last message, `q` quit |
| Pull requests | `i` info, `M` raw/rendered Markdown, `d` diff, `n`/`N` next/previous file, `h` checks, `w` web, `l` line comment, `u`/`U` next/previous discussion, `O` resolved discussions, `a` approve, `x` request changes, `c` comment, `A` AI review, `P` post findings, `v` reviewers, `r` ready (from draft), `e` title, `E` description and `T` checkboxes (your own PRs), `m` merge, `X` close/reopen, `b` checkout, `t` hide teams |
| Issues | `i` info, `w` web, `n` new, `c` comment, `l` labels, `a` assignees, `X` close/reopen |
| Releases | `i` info, `w` web, `D` download, `n` new, `e` edit, `P` publish, `X` delete |
| Actions | `i` jobs, `l` log, `L` failed log, `w` web, `r` rerun all, `F` rerun failed, `x` cancel, `t` trigger workflow |
| Activity | `S` mark all seen, `B` show bots, and every pull request key |
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

Close, delete, publish, rerun, cancel, mark ready and reviewer changes ask for `y` first. Merge, review
and text actions open a form that is only sent with `Ctrl+S`.

Discussions are shown under their line of code; resolved ones are folded until `O`.
`l` starts line mode on the code: move with `j`/`k`, `Space` starts a range, `Enter`
comments, and on a discussion `Enter` replies and `x` resolves or reopens. Comments
are posted at once, not as part of a pending review.

`m` first shows conflicts, approval, checks and open discussions, and offers only the
merge methods the repository allows. Organizations, repositories, filters, states and
the raw/rendered choice are remembered in `~/.config/ghdeck/view.json`; a repository
given on the command line is not.

Titles and descriptions are loaded when you press the key. Saving is refused when
they changed on GitHub meanwhile. Ticking checkboxes saves at once; unticking asks.

## Activity

The Activity tab lists what others did on pull requests you opened, review, commented
on, or were asked about (hidden teams excepted): approvals, requested changes, comments,
mentions, commits, merges and failed checks of your own. `Enter` opens the pull request,
at the discussion when there is one, and marks it seen; `●` marks what is new, also in
the pull request list. It covers the 50 most recently updated ones and loads on start,
on `R` and after your own actions, never on a timer. Seen marks stay on this computer
(`~/.config/ghdeck/seen.json`); your GitHub notifications are not touched.

## AI review

`A` on an open pull request asks an AI tool for a first review. Findings appear under
their line of code: `Space` accepts, `e` edits, `d` discards, `P` posts the accepted
ones as one comment review. Nothing is posted otherwise, and it never approves.

The tool runs on your machine with your login and **sends title, description and diff
to its provider**. Claude Code runs without a shell, files, plugins or project
instructions. Codex is not enabled yet: it cannot be kept from reading your files.
Lock files, generated files and what exceeds 200 KB are left out and named.

Settings in `~/.config/ghdeck/ai.json`, all optional:

```json
{
  "tool": "claude",
  "model": "sonnet",
  "custom": ["my-reviewer", "--json"],
  "blocked": ["secret-org", "org/private-repo"]
}
```

A custom command gets the prompt on stdin and prints `{"findings": [{"path", "line",
"side", "severity", "title", "body"}]}`. It is run without a shell; its safety is yours.

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

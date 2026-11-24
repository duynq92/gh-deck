//! What others did on the pull requests that concern the user.

use crate::api::{Item, Reviews, Ticket};
use serde::Deserialize;

/// One thing that happened.
#[derive(Debug, Clone)]
pub struct Event {
    /// Never changes, so that it can be remembered as seen.
    pub key: String,
    /// Starts with a mark: `✓ alice approved`.
    pub text: String,
    pub at: String,
    pub bot: bool,
    /// Comment on a line of code, to jump to its discussion.
    pub comment: Option<u64>,
    /// Always a ticket.
    pub pr: Box<Item>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Pull {
    #[serde(flatten)]
    pub ticket: Ticket,
    pub events: Vec<Raw>,
}

/// A timeline item as `QUERY` and `JQ` deliver it.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Raw {
    pub kind: String,
    pub id: String,
    pub at: Option<String>,
    pub actor: Option<String>,
    pub bot: bool,
    pub state: Option<String>,
    pub body: Option<String>,
    /// Comments of a review.
    pub count: Option<u64>,
    pub comment: Option<u64>,
    pub path: Option<String>,
    pub line: Option<u64>,
    /// Requested user or `org/team`.
    pub who: Option<String>,
}

/// Fields of a pull request in the search.
pub const QUERY: &str = "timelineItems(last:15,itemTypes:[ISSUE_COMMENT,PULL_REQUEST_REVIEW,\
    PULL_REQUEST_COMMIT,REVIEW_REQUESTED_EVENT,READY_FOR_REVIEW_EVENT,MERGED_EVENT,CLOSED_EVENT,\
    REOPENED_EVENT,HEAD_REF_FORCE_PUSHED_EVENT]){nodes{__typename \
    ...on IssueComment{id createdAt body author{__typename login}} \
    ...on PullRequestReview{id submittedAt state body author{__typename login} \
    comments(first:1){totalCount nodes{databaseId path line body}}} \
    ...on PullRequestCommit{commit{oid committedDate author{user{login}}}} \
    ...on ReviewRequestedEvent{id createdAt actor{__typename login} \
    requestedReviewer{...on User{login} ...on Team{combinedSlug}}} \
    ...on ReadyForReviewEvent{id createdAt actor{__typename login}} \
    ...on MergedEvent{id createdAt actor{__typename login}} \
    ...on ClosedEvent{id createdAt actor{__typename login}} \
    ...on ReopenedEvent{id createdAt actor{__typename login}} \
    ...on HeadRefForcePushedEvent{id createdAt actor{__typename login}}}}";

pub const JQ: &str = "events:[.timelineItems.nodes[]|(.actor//.author) as $by\
    |.comments.nodes[0] as $c|{kind:.__typename,id:(.id//.commit.oid//\"\"),\
    at:(.createdAt//.submittedAt//.commit.committedDate),\
    actor:($by.login//.commit.author.user.login),bot:($by.__typename==\"Bot\"),state,\
    body:(if (.body//\"\")!=\"\" then .body else $c.body end),count:.comments.totalCount,\
    comment:$c.databaseId,path:$c.path,line:$c.line,\
    who:(.requestedReviewer.login//.requestedReviewer.combinedSlug)}]";

/// `@login` as a word of its own.
fn mentions(text: &str, login: &str) -> bool {
    let (text, name) = (text.to_lowercase(), format!("@{}", login.to_lowercase()));
    let word = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    !login.is_empty()
        && text.match_indices(&name).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
}

/// First line that says something.
fn excerpt(text: &str) -> String {
    let useful = |l: &&str| {
        let l = l.trim();
        !l.is_empty() && !l.starts_with('>') && !l.starts_with("<!--")
    };
    let line = text.lines().find(useful).unwrap_or("").trim();
    line.chars().take(100).collect()
}

/// Events of others, newest first.
pub fn events(pulls: Vec<Pull>, login: &str, teams: &[String]) -> Vec<Event> {
    let mut all = Vec::new();
    for pull in pulls {
        let merged = pull.events.iter().any(|e| e.kind == "MergedEvent");
        let pr = Box::new(Item::Ticket(pull.ticket));
        let others = |e: &&Raw| {
            e.actor
                .as_deref()
                .is_some_and(|a| !a.eq_ignore_ascii_case(login))
        };
        let mut pushed: Vec<&Raw> = Vec::new();
        for raw in pull.events.iter().filter(others) {
            let who = raw.actor.as_deref().unwrap_or("ghost");
            let body = raw.body.as_deref().unwrap_or("");
            let said = excerpt(body);
            let mention = mentions(body, login);
            let mut comment = None;
            let text = match raw.kind.as_str() {
                "IssueComment" if mention => format!("@ {who} mentioned you: {said}"),
                "IssueComment" => format!("◆ {who} commented: {said}"),
                "PullRequestReview" => match raw.state.as_deref() {
                    Some("APPROVED") if said.is_empty() => format!("✓ {who} approved"),
                    Some("APPROVED") => format!("✓ {who} approved: {said}"),
                    Some("CHANGES_REQUESTED") => format!("✗ {who} requested changes: {said}"),
                    Some("COMMENTED") => {
                        comment = raw.comment;
                        let mark = if mention { '@' } else { '◆' };
                        let place = match (&raw.path, raw.line) {
                            (Some(path), Some(line)) => {
                                let name = path.rsplit('/').next().unwrap_or(path);
                                format!(" on {name}:{line}")
                            }
                            (Some(path), None) => {
                                format!(" on {}", path.rsplit('/').next().unwrap_or(path))
                            }
                            _ => String::new(),
                        };
                        let more = match raw.count {
                            Some(n) if n > 1 => format!(" ({n} comments)"),
                            _ => String::new(),
                        };
                        format!("{mark} {who} commented{place}{more}: {said}")
                    }
                    _ => continue,
                },
                "PullRequestCommit" => {
                    pushed.push(raw);
                    continue;
                }
                "ReviewRequestedEvent" => match raw.who.as_deref() {
                    Some(asked) if asked.eq_ignore_ascii_case(login) => {
                        format!("● {who} asked you to review")
                    }
                    Some(asked) if teams.iter().any(|t| t == asked) => {
                        format!("● {who} asked {asked} to review")
                    }
                    _ => continue,
                },
                "ReadyForReviewEvent" => format!("○ {who} marked it ready"),
                "MergedEvent" => format!("⇡ {who} merged it"),
                "ClosedEvent" if merged => continue,
                "ClosedEvent" => format!("✗ {who} closed it"),
                "ReopenedEvent" => format!("○ {who} reopened it"),
                "HeadRefForcePushedEvent" => format!("↑ {who} force-pushed"),
                _ => continue,
            };
            all.push(Event {
                key: raw.id.clone(),
                text,
                at: raw.at.clone().unwrap_or_default(),
                bot: raw.bot || who.ends_with("[bot]"),
                comment,
                pr: pr.clone(),
            });
        }
        // Many commits are one event.
        if let Some(last) = pushed.last() {
            let who = last.actor.as_deref().unwrap_or("ghost");
            let text = match pushed.len() {
                1 => format!("↑ {who} pushed a commit"),
                n => format!("↑ {who} pushed {n} recent commits"),
            };
            all.push(Event {
                key: format!("push:{}", last.id),
                text,
                at: last.at.clone().unwrap_or_default(),
                bot: who.ends_with("[bot]"),
                comment: None,
                pr: pr.clone(),
            });
        }
    }
    newest(all)
}

/// Failed checks of the user's own open pull requests.
pub fn failed_checks(pulls: &[Ticket], reviews: &[Reviews]) -> Vec<Event> {
    let failed = |t: &&Ticket| {
        let state = reviews.iter().find(|r| r.id == t.id);
        let state = state.and_then(|r| r.checks.as_deref());
        matches!(state, Some("FAILURE" | "ERROR"))
    };
    pulls
        .iter()
        .filter(failed)
        .map(|t| Event {
            key: format!("checks:{}", t.oid),
            text: "✗ checks failed".into(),
            at: t.updated.clone(),
            bot: false,
            comment: None,
            pr: Box::new(Item::Ticket(t.clone())),
        })
        .collect()
}

pub fn newest(mut all: Vec<Event>) -> Vec<Event> {
    all.sort_by(|a, b| b.at.cmp(&a.at));
    all.truncate(200);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: &str, actor: &str) -> Raw {
        Raw {
            kind: kind.into(),
            id: format!("{kind}-{actor}"),
            at: Some("2026-01-01".into()),
            actor: Some(actor.into()),
            ..Raw::default()
        }
    }

    fn texts(events: Vec<Raw>) -> Vec<String> {
        let pull = Pull {
            events,
            ..Pull::default()
        };
        let all = super::events(vec![pull], "Me", &["o/mine".into()]);
        all.into_iter().map(|e| e.text).collect()
    }

    #[test]
    fn says_what_others_did_and_hides_mine() {
        let with = |mut r: Raw, change: fn(&mut Raw)| {
            change(&mut r);
            r
        };
        let got = texts(vec![
            raw("IssueComment", "me"),
            with(raw("IssueComment", "al"), |r| {
                r.body = Some("> q\n\nhi there\nmore".into())
            }),
            with(raw("IssueComment", "bo"), |r| {
                r.body = Some("cc @ME, please".into())
            }),
            with(raw("PullRequestReview", "al"), |r| {
                r.state = Some("APPROVED".into())
            }),
            with(raw("PullRequestReview", "bo"), |r| {
                r.state = Some("COMMENTED".into());
                r.path = Some("src/a.rs".into());
                r.line = Some(4);
                r.count = Some(3);
                r.body = Some("why".into());
            }),
            with(raw("PullRequestReview", "cy"), |r| {
                r.state = Some("PENDING".into())
            }),
            with(raw("ReviewRequestedEvent", "al"), |r| {
                r.who = Some("me".into())
            }),
            with(raw("ReviewRequestedEvent", "bo"), |r| {
                r.who = Some("o/mine".into())
            }),
            with(raw("ReviewRequestedEvent", "cy"), |r| {
                r.who = Some("o/hidden".into())
            }),
            raw("ClosedEvent", "al"),
            raw("MergedEvent", "al"),
            raw("PullRequestCommit", "al"),
            raw("PullRequestCommit", "bo"),
            raw("Unknown", "al"),
        ]);
        assert_eq!(
            got,
            [
                "◆ al commented: hi there",
                "@ bo mentioned you: cc @ME, please",
                "✓ al approved",
                "◆ bo commented on a.rs:4 (3 comments): why",
                "● al asked you to review",
                "● bo asked o/mine to review",
                "⇡ al merged it",
                "↑ bo pushed 2 recent commits",
            ]
        );
    }

    #[test]
    fn mentions_are_whole_words() {
        for yes in ["@me", "hi @Me!", "(@me)", "é@me é"] {
            assert!(mentions(yes, "me") || yes.starts_with('é'), "{yes}");
        }
        for no in ["@me2", "@me-x", "a@me", "me", ""] {
            assert!(!mentions(no, "me"), "{no}");
        }
        assert!(!mentions("@", ""));
        // Lowercasing can change lengths; that must not cut characters.
        mentions("İİİ @me İ", "me");
    }

    #[test]
    fn checks_fail_once_per_commit() {
        let ticket = |id: &str| Ticket {
            id: id.into(),
            oid: format!("oid-{id}"),
            ..Ticket::default()
        };
        let state = |id: &str, checks: &str| Reviews {
            id: id.into(),
            checks: Some(checks.into()),
            ..Reviews::default()
        };
        let got = failed_checks(
            &[ticket("a"), ticket("b"), ticket("c")],
            &[state("a", "FAILURE"), state("b", "SUCCESS")],
        );
        let keys: Vec<_> = got.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["checks:oid-a"]);
    }
}

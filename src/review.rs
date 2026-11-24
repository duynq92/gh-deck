//! Discussions on lines of code: loading, posting, replying and resolving.

use crate::ai::{Finding, Review};
use crate::gh::{self, Cmd, GhError, Guard, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Comment {
    pub id: Option<u64>,
    pub author: Option<String>,
    pub at: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Thread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
    pub path: String,
    /// `None` when the line is gone, or the thread is about the whole file.
    pub line: Option<u32>,
    /// `LEFT` is the old text, `RIGHT` the new one.
    pub side: String,
    pub can_reply: bool,
    pub can_toggle: bool,
    pub total: usize,
    pub comments: Vec<Comment>,
}

/// Everything about a pull request's discussions.
#[derive(Debug, Default)]
pub struct Talk {
    pub oid: String,
    pub merge: String,
    pub threads: Vec<Thread>,
}

impl Talk {
    /// Lines for the facts at the top.
    pub fn summary(&self) -> String {
        let merge = match self.merge.as_str() {
            "CLEAN" | "HAS_HOOKS" => "ready",
            "DIRTY" => "✗ conflicts with the base branch",
            "BLOCKED" => "✗ blocked by branch rules",
            "BEHIND" => "✗ behind the base branch",
            "UNSTABLE" => "● ready, but some checks are not passing",
            "DRAFT" => "● draft",
            _ => "not known yet",
        };
        let open = self.threads.iter().filter(|t| !t.resolved).count();
        let done = self.threads.len() - open;
        let mut text = format!("Merge: {merge}");
        if !self.threads.is_empty() {
            text += &format!("\nDiscussions: {open} unresolved, {done} resolved");
        }
        text
    }
}

pub fn split(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .ok_or_else(|| GhError(format!("invalid repository {repo:?}")))
}

pub fn load(repo: &str, number: u64) -> Result<Talk> {
    #[derive(Deserialize)]
    struct Out {
        oid: String,
        merge: String,
        cursor: Option<String>,
        threads: Vec<Thread>,
    }
    let (owner, name) = split(repo)?;
    let mut talk = Talk::default();
    let mut after: Option<String> = None;
    for _ in 0..5 {
        let mut args: Vec<String> = [
            "api",
            "graphql",
            "-f",
            "query=query($o:String!,$n:String!,$number:Int!,$after:String){\
             repository(owner:$o,name:$n){pullRequest(number:$number){headRefOid \
             mergeStateStatus reviewThreads(first:100,after:$after){\
             pageInfo{hasNextPage endCursor} nodes{id isResolved isOutdated path line diffSide \
             subjectType viewerCanReply viewerCanResolve viewerCanUnresolve \
             comments(first:50){totalCount nodes{databaseId author{login} createdAt body}}}}}}}",
            "--jq",
            ".data.repository.pullRequest|{oid:.headRefOid,merge:.mergeStateStatus,\
             cursor:(.reviewThreads.pageInfo|if .hasNextPage then .endCursor else null end),\
             threads:[.reviewThreads.nodes[]|{id,resolved:.isResolved,outdated:.isOutdated,path,\
             line:(if .subjectType==\"FILE\" or .isOutdated then null else .line end),\
             side:.diffSide,can_reply:.viewerCanReply,\
             can_toggle:(if .isResolved then .viewerCanUnresolve else .viewerCanResolve end),\
             total:.comments.totalCount,comments:[.comments.nodes[]|{id:.databaseId,\
             author:.author.login,at:.createdAt,body}]}]}",
        ]
        .map(String::from)
        .to_vec();
        args.extend(["-f".into(), format!("o={owner}")]);
        args.extend(["-f".into(), format!("n={name}")]);
        args.extend(["-F".into(), format!("number={number}")]);
        if let Some(after) = after {
            args.extend(["-f".into(), format!("after={after}")]);
        }
        let text = gh::run(&args, None, gh::JSON)?;
        let out: Out = serde_json::from_str(&text)
            .map_err(|e| GhError(format!("unexpected gh response: {e}")))?;
        talk.oid = out.oid;
        talk.merge = out.merge;
        talk.threads.extend(out.threads);
        after = out.cursor;
        if after.is_none() {
            break;
        }
    }
    Ok(talk)
}

/// Lines a new comment is about, as GitHub's API names them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub path: String,
    pub line: u32,
    pub side: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub start_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub start_side: Option<String>,
}

impl Place {
    pub fn label(&self) -> String {
        let old = if self.side == "LEFT" { " (old)" } else { "" };
        match self.start_line {
            Some(start) => format!("{}:{start}-{}{old}", self.path, self.line),
            None => format!("{}:{}{old}", self.path, self.line),
        }
    }
}

/// The API request of a new comment, without its text.
#[derive(Debug, Serialize, Deserialize)]
pub struct NewComment {
    pub commit_id: String,
    #[serde(flatten)]
    pub place: Place,
}

fn send(label: String, path: String, body: serde_json::Value) -> Cmd {
    let args = [
        "api",
        "--method",
        "POST",
        &path,
        "--input",
        "-",
        "--jq",
        ".html_url",
    ];
    Cmd {
        label,
        args: args.map(String::from).to_vec(),
        stdin: Some(body.to_string()),
        reload: true,
        ..Cmd::default()
    }
}

/// Accepted findings, ready to be posted as one review.
#[derive(Debug, Serialize, Deserialize)]
pub struct Batch {
    oid: String,
    tool: String,
    comments: Vec<serde_json::Value>,
    /// Findings without a line go into the text of the review.
    rest: Vec<String>,
}

impl Batch {
    pub fn new(review: &Review) -> Self {
        let (on, off): (Vec<&Finding>, Vec<&Finding>) = review
            .findings
            .iter()
            .filter(|f| f.accepted)
            .partition(|f| f.line.is_some());
        Batch {
            oid: review.oid.clone(),
            tool: review.tool.clone(),
            comments: on
                .iter()
                .map(|f| {
                    serde_json::json!({"path": f.path, "line": f.line, "side": f.side,
                        "body": f.text()})
                })
                .collect(),
            rest: off
                .iter()
                .map(|f| format!("{}: {}", f.path, f.text()))
                .collect(),
        }
    }

    pub fn notes(&self) -> Vec<(bool, String)> {
        let mut notes = vec![(true, "a comment review: neither approves nor blocks".into())];
        let (on, off) = (self.comments.len(), self.rest.len());
        if on > 0 {
            notes.push((true, format!("{on} on lines of code")));
        }
        if off > 0 {
            notes.push((true, format!("{off} in the text of the review")));
        }
        notes
    }
}

/// `batch` is a serialized `Batch`.
pub fn post(
    repo: &str,
    number: u64,
    batch: &str,
    summary: &str,
    mention: bool,
) -> std::result::Result<Cmd, String> {
    let batch: Batch = serde_json::from_str(batch).map_err(|e| e.to_string())?;
    let count = batch.comments.len() + batch.rest.len();
    let mut parts: Vec<String> = vec![summary.trim().to_string()];
    parts.extend(batch.rest);
    if mention {
        parts.push(format!(
            "_AI-assisted review ({}); findings were accepted one by one before posting._",
            batch.tool
        ));
    }
    parts.retain(|p| !p.is_empty());
    if parts.is_empty() {
        parts.push("Review comments".into());
    }
    let body = serde_json::json!({"commit_id": batch.oid, "event": "COMMENT",
        "body": parts.join("\n\n"), "comments": batch.comments});
    let mut cmd = send(
        format!("Post {count} findings to {repo}#{number}"),
        format!("repos/{repo}/pulls/{number}/reviews"),
        body,
    );
    // Not on commits that were never reviewed.
    let args = [
        "api",
        &format!("repos/{repo}/pulls/{number}"),
        "--jq",
        ".head.sha|tojson",
    ];
    cmd.guard = Some(Guard {
        args: args.map(String::from).to_vec(),
        expected: batch.oid,
    });
    Ok(cmd)
}

/// `new` is a serialized `NewComment`.
pub fn comment(repo: &str, number: u64, new: &str, text: &str) -> std::result::Result<Cmd, String> {
    let new: NewComment = serde_json::from_str(new).map_err(|e| e.to_string())?;
    let label = format!("Comment on {}", new.place.label());
    let mut body = serde_json::to_value(&new).map_err(|e| e.to_string())?;
    body["body"] = text.into();
    Ok(send(
        label,
        format!("repos/{repo}/pulls/{number}/comments"),
        body,
    ))
}

pub fn reply(
    repo: &str,
    number: u64,
    comment: &str,
    text: &str,
) -> std::result::Result<Cmd, String> {
    let id: u64 = comment
        .parse()
        .map_err(|_| "this discussion cannot be replied to".to_string())?;
    Ok(send(
        format!("Reply in {repo}#{number}"),
        format!("repos/{repo}/pulls/{number}/comments/{id}/replies"),
        serde_json::json!({ "body": text }),
    ))
}

pub fn toggle(thread: &Thread) -> Cmd {
    let (verb, mutation) = if thread.resolved {
        ("Reopen", "unresolveReviewThread")
    } else {
        ("Resolve", "resolveReviewThread")
    };
    let args = vec![
        "api".into(),
        "graphql".into(),
        "-f".into(),
        format!(
            "query=mutation($id:ID!){{{mutation}(input:{{threadId:$id}}){{clientMutationId}}}}"
        ),
        "-f".into(),
        format!("id={}", thread.id),
        "--silent".into(),
    ];
    Cmd {
        label: format!("{verb} the discussion on {}", thread.path),
        args,
        reload: true,
        ..Cmd::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_is_sent_as_json() {
        let new = NewComment {
            commit_id: "abc".into(),
            place: Place {
                path: "a b.rs".into(),
                line: 7,
                side: "RIGHT".into(),
                start_line: Some(5),
                start_side: Some("LEFT".into()),
            },
        };
        let new = serde_json::to_string(&new).unwrap();
        let cmd = comment("o/r", 3, &new, "say \"hi\"\n--x").unwrap();
        assert_eq!(cmd.args[3], "repos/o/r/pulls/3/comments");
        let sent: serde_json::Value = serde_json::from_str(&cmd.stdin.unwrap()).unwrap();
        let want = serde_json::json!({"commit_id":"abc","path":"a b.rs","line":7,"side":"RIGHT",
            "start_line":5,"start_side":"LEFT","body":"say \"hi\"\n--x"});
        assert_eq!(sent, want);
        assert!(cmd.reload);
    }

    #[test]
    fn single_line_has_no_start() {
        let place = Place {
            path: "a".into(),
            line: 1,
            side: "LEFT".into(),
            start_line: None,
            start_side: None,
        };
        assert_eq!(place.label(), "a:1 (old)");
        let json = serde_json::to_string(&place).unwrap();
        assert!(!json.contains("start"), "{json}");
    }

    #[test]
    fn replies_and_resolving() {
        let cmd = reply("o/r", 3, "42", "ok").unwrap();
        assert_eq!(cmd.args[3], "repos/o/r/pulls/3/comments/42/replies");
        assert!(reply("o/r", 3, "", "ok").is_err());
        let mut thread = Thread {
            id: "T".into(),
            ..Thread::default()
        };
        assert!(toggle(&thread).args[3].contains("{resolveReviewThread("));
        thread.resolved = true;
        assert!(toggle(&thread).args[3].contains("{unresolveReviewThread("));
    }

    #[test]
    fn findings_are_posted_as_one_comment_review() {
        let finding = |line: Option<u32>, accepted: bool| Finding {
            path: "a".into(),
            line,
            side: "LEFT".into(),
            severity: "nit".into(),
            title: "t \"q\"".into(),
            body: "b".into(),
            accepted,
        };
        let review = Review {
            oid: "abc".into(),
            tool: "X".into(),
            findings: vec![
                finding(Some(3), true),
                finding(None, true),
                finding(Some(4), false),
            ],
            ..Review::default()
        };
        let batch = Batch::new(&review);
        assert_eq!(batch.notes().len(), 3);
        let batch = serde_json::to_string(&batch).unwrap();
        let cmd = post("o/r", 3, &batch, " hi ", true).unwrap();
        assert_eq!(cmd.args[3], "repos/o/r/pulls/3/reviews");
        let sent: serde_json::Value = serde_json::from_str(&cmd.stdin.unwrap()).unwrap();
        assert_eq!(sent["event"], "COMMENT");
        assert_eq!(sent["commit_id"], "abc");
        let text = "**t \"q\"** (nit)\n\nb";
        let one = serde_json::json!([{"path":"a","line":3,"side":"LEFT","body":text}]);
        assert_eq!(sent["comments"], one);
        let body = sent["body"].as_str().unwrap();
        assert!(
            body.starts_with(&format!("hi\n\na: {text}\n\n_AI-assisted")),
            "{body}"
        );
        assert_eq!(cmd.guard.unwrap().expected, "abc");
        let empty = serde_json::to_string(&Batch::new(&Review::default())).unwrap();
        let sent = post("o/r", 3, &empty, "", false).unwrap().stdin.unwrap();
        assert!(sent.contains("Review comments"));
    }
}

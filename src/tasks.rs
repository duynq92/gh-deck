//! Task list checkboxes (`- [ ] text`) in Markdown.

#[derive(Debug, PartialEq)]
pub struct Task {
    /// Byte offset of the mark between the brackets.
    pub mark: usize,
    pub done: bool,
    pub text: String,
}

/// Offset just after the list marker, if the line starts with one.
fn after_marker(line: &str) -> Option<usize> {
    let body = line.trim_start_matches([' ', '\t', '>']);
    let start = line.len() - body.len();
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    let marker = match body.as_bytes().get(digits)? {
        b'-' | b'*' | b'+' if digits == 0 => 1,
        b'.' | b')' if digits > 0 => digits + 1,
        _ => return None,
    };
    let rest = &body[marker..];
    let text = rest.trim_start_matches([' ', '\t']);
    (text.len() < rest.len()).then(|| start + body.len() - text.len())
}

/// Checkboxes GitHub would render: not inside code blocks or HTML comments.
pub fn find(body: &str) -> Vec<Task> {
    let mut tasks = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut comment = false;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start_matches([' ', '\t', '>']);
        let char = trimmed.bytes().next().unwrap_or(b' ');
        let run = trimmed.bytes().take_while(|b| *b == char).count();
        if let Some((open, len)) = fence {
            if char == open && run >= len && trimmed[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if comment {
            comment = !line.contains("-->");
            continue;
        }
        if matches!(char, b'`' | b'~') && run >= 3 {
            fence = Some((char, run));
            continue;
        }
        if let Some(open) = line.rfind("<!--") {
            comment = !line[open..].contains("-->");
        }
        let Some(at) = after_marker(line) else {
            continue;
        };
        let (done, text) = match line.as_bytes().get(at..at + 4) {
            Some([b'[', mark @ (b' ' | b'x' | b'X'), b']', b' ' | b'\t']) => {
                (*mark != b' ', line[at + 4..].trim())
            }
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        let depth = line.len() - line.trim_start_matches([' ', '\t']).len();
        tasks.push(Task {
            mark: start + at + 1,
            done,
            text: format!("{}{text}", " ".repeat(depth.min(8))),
        });
    }
    tasks
}

/// Set every checkbox; everything else stays byte for byte.
pub fn apply(body: &str, ticked: &[usize]) -> String {
    let mut out = body.to_string();
    for task in find(body) {
        let want = ticked.contains(&task.mark);
        if want != task.done {
            let mark = if want { "x" } else { " " };
            out.replace_range(task.mark..task.mark + 1, mark);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "# Việc\r\n- [ ] one\r\n  * [X] hai ệ\r\n1. [x] three\r\n\
        ```\r\n- [ ] code\r\n```\r\n<!-- - [ ] hidden\r\n- [ ] hidden\r\n-->\r\n\
        > - [ ] quoted\r\n- [] no\r\n- [ ]no\r\n-[ ] no\r\ntext [ ] no\r\n";

    #[test]
    fn finds_rendered_checkboxes() {
        let got: Vec<_> = find(BODY).into_iter().map(|t| (t.done, t.text)).collect();
        let want = [
            (false, "one"),
            (true, "  hai ệ"),
            (true, "three"),
            (false, "quoted"),
        ];
        assert_eq!(got, want.map(|(d, t)| (d, t.to_string())));
    }

    #[test]
    fn changes_only_the_marks() {
        let marks: Vec<usize> = find(BODY).iter().map(|t| t.mark).collect();
        let out = apply(BODY, &[marks[0], marks[1]]);
        assert_eq!(
            out,
            BODY.replace("- [ ] one", "- [x] one")
                .replace("1. [x]", "1. [ ]")
        );
        assert_eq!(apply(BODY, &[marks[1], marks[2]]), BODY);
    }
}

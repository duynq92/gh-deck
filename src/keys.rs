//! The single source of truth for shortcuts: dispatch, on-screen labels and
//! the help screen are all generated from these tables.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Prs,
    Issues,
    Releases,
    Runs,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Prs, Tab::Issues, Tab::Releases, Tab::Runs];

    pub fn tiny(self) -> &'static str {
        match self {
            Tab::Prs => "PR",
            Tab::Issues => "Iss",
            Tab::Releases => "Rel",
            Tab::Runs => "Act",
        }
    }

    pub fn short(self) -> &'static str {
        if self == Tab::Prs {
            "PRs"
        } else {
            self.title()
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Prs => "Pull Requests",
            Tab::Issues => "Issues",
            Tab::Releases => "Releases",
            Tab::Runs => "Actions",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    // Global
    Org,
    Repo,
    Filter,
    Exclude,
    State,
    Find,
    More,
    Refresh,
    Help,
    Message,
    Quit,
    // Item
    Info,
    Web,
    Diff,
    NextFile,
    PrevFile,
    Checks,
    Approve,
    RequestChanges,
    Comment,
    Merge,
    Toggle,
    Checkout,
    Reviewers,
    Ready,
    New,
    Labels,
    Assignees,
    Edit,
    Publish,
    Download,
    Delete,
    Log,
    FailedLog,
    Rerun,
    RerunFailed,
    Cancel,
    Trigger,
}

pub struct Key {
    pub key: char,
    pub label: &'static str,
    pub act: Act,
}

pub struct Group {
    pub name: &'static str,
    pub keys: &'static [Key],
}

const fn k(key: char, label: &'static str, act: Act) -> Key {
    Key { key, label, act }
}

/// Shown in the top-right corner together with their current value.
pub const SCOPE: Group = Group {
    name: "Show",
    keys: &[
        k('o', "Org", Act::Org),
        k('p', "Repo", Act::Repo),
        k('f', "Filter", Act::Filter),
        k('s', "State", Act::State),
    ],
};

pub const GLOBAL: Group = Group {
    name: "General",
    keys: &[
        k('/', "Find", Act::Find),
        k('>', "More", Act::More),
        k('R', "Refresh", Act::Refresh),
        k('?', "Help", Act::Help),
        k('!', "Message", Act::Message),
        k('q', "Quit", Act::Quit),
    ],
};

const PRS: &[Group] = &[
    Group {
        name: "View",
        keys: &[
            k('i', "Info", Act::Info),
            k('d', "Diff", Act::Diff),
            k('n', "Next file", Act::NextFile),
            k('N', "Prev file", Act::PrevFile),
            k('h', "Checks", Act::Checks),
            k('w', "Web", Act::Web),
        ],
    },
    Group {
        name: "Review",
        keys: &[
            k('a', "Approve", Act::Approve),
            k('x', "Request changes", Act::RequestChanges),
            k('c', "Comment", Act::Comment),
        ],
    },
    Group {
        name: "Manage",
        keys: &[
            k('v', "Reviewers", Act::Reviewers),
            k('r', "Mark ready", Act::Ready),
            k('m', "Merge", Act::Merge),
            k('X', "Close/Reopen", Act::Toggle),
            k('b', "Checkout", Act::Checkout),
        ],
    },
    Group {
        name: "Teams",
        keys: &[k('t', "Excluded teams", Act::Exclude)],
    },
];

const ISSUES: &[Group] = &[
    Group {
        name: "View",
        keys: &[k('i', "Info", Act::Info), k('w', "Web", Act::Web)],
    },
    Group {
        name: "Edit",
        keys: &[
            k('n', "New", Act::New),
            k('c', "Comment", Act::Comment),
            k('l', "Labels", Act::Labels),
            k('a', "Assignees", Act::Assignees),
        ],
    },
    Group {
        name: "Manage",
        keys: &[k('X', "Close/Reopen", Act::Toggle)],
    },
];

const RELEASES: &[Group] = &[
    Group {
        name: "View",
        keys: &[
            k('i', "Info", Act::Info),
            k('w', "Web", Act::Web),
            k('D', "Download", Act::Download),
        ],
    },
    Group {
        name: "Edit",
        keys: &[
            k('n', "New", Act::New),
            k('e', "Edit", Act::Edit),
            k('P', "Publish", Act::Publish),
            k('X', "Delete", Act::Delete),
        ],
    },
];

const RUNS: &[Group] = &[
    Group {
        name: "View",
        keys: &[
            k('i', "Jobs", Act::Info),
            k('l', "Log", Act::Log),
            k('L', "Failed log", Act::FailedLog),
            k('w', "Web", Act::Web),
        ],
    },
    Group {
        name: "Run",
        keys: &[
            k('r', "Rerun all", Act::Rerun),
            k('F', "Rerun failed", Act::RerunFailed),
            k('x', "Cancel", Act::Cancel),
            k('t', "Trigger workflow", Act::Trigger),
        ],
    },
];

pub fn groups(tab: Tab) -> &'static [Group] {
    match tab {
        Tab::Prs => PRS,
        Tab::Issues => ISSUES,
        Tab::Releases => RELEASES,
        Tab::Runs => RUNS,
    }
}

pub fn lookup(tab: Tab, key: char) -> Option<Act> {
    groups(tab)
        .iter()
        .chain([&SCOPE, &GLOBAL])
        .flat_map(|g| g.keys)
        .find(|k| k.key == key)
        .map(|k| k.act)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn shortcuts_never_collide() {
        for tab in Tab::ALL {
            let mut seen: HashSet<char> = "1234jkgG".chars().collect();
            for key in groups(tab)
                .iter()
                .chain([&SCOPE, &GLOBAL])
                .flat_map(|g| g.keys)
            {
                assert!(seen.insert(key.key), "{tab:?}: duplicate {:?}", key.key);
            }
        }
    }
}

//! Saved settings: teams to leave out of "my teams", and what was shown last.

use crate::ai::Settings;
use crate::api::{self, Choice, valid_repo};
use crate::keys::Tab;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// `$XDG_CONFIG_HOME/ghdeck/NAME`, or below `~/.config`.
pub fn path(name: &str) -> Option<PathBuf> {
    let dir = |name| env::var_os(name).map(PathBuf::from);
    let base = dir("XDG_CONFIG_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| dir("HOME").map(|home| home.join(".config")))?;
    Some(base.join("ghdeck").join(name))
}

/// One `org/team` per line; a missing file means none.
pub fn load(path: &Path) -> io::Result<Vec<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let teams: Vec<String> = lines.map(String::from).collect();
    match teams.iter().find(|t| !valid_repo(t)) {
        Some(bad) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{bad:?} is not org/team"),
        )),
        None => Ok(teams),
    }
}

pub fn save(path: &Path, teams: &[String]) -> io::Result<()> {
    let text: String = teams.iter().map(|t| format!("{t}\n")).collect();
    write(path, &text)
}

/// What was shown when the app was last used.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    pub orgs: Vec<Choice>,
    pub repos: Vec<Choice>,
    /// Filters and states of every tab.
    pub tabs: Vec<(Vec<Choice>, Vec<Choice>)>,
    pub raw: bool,
}

impl View {
    /// Drop what would not make a valid query.
    fn checked(mut self) -> Self {
        self.orgs.retain(|o| valid_repo(&format!("{}/x", o.1)));
        self.repos.retain(|r| valid_repo(&r.1));
        self.tabs.truncate(Tab::ALL.len());
        for (tab, (filters, states)) in Tab::ALL.into_iter().zip(&mut self.tabs) {
            let known = api::filters(tab, &[]);
            let team = |value: &str| {
                let team = value.strip_prefix("team-review-requested:");
                tab == Tab::Prs && team.is_some_and(valid_repo)
            };
            filters.retain(|f| team(&f.1) || known.iter().any(|k| k.1 == f.1));
            let known = api::states(tab);
            states.retain(|s| known.contains(s));
        }
        self
    }
}

/// `None` when nothing was saved yet.
pub fn load_view(path: &Path) -> io::Result<Option<View>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let view: View = serde_json::from_str(&text)?;
    Ok(Some(view.checked()))
}

pub fn save_view(path: &Path, view: &View) -> io::Result<()> {
    write(path, &serde_json::to_string_pretty(view)?)
}

/// Keys of the events that were seen, oldest first.
pub fn load_seen(path: &Path) -> io::Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

pub fn save_seen(path: &Path, seen: &[String]) -> io::Result<()> {
    write(path, &serde_json::to_string(seen)?)
}

/// Settings of the AI review; a missing file means none.
pub fn load_ai(path: &Path) -> io::Result<Settings> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(e),
    };
    let settings: Settings = serde_json::from_str(&text)?;
    let bad = |e| io::Error::new(io::ErrorKind::InvalidData, e);
    settings.check().map_err(bad)?;
    Ok(settings)
}

pub fn save_ai(path: &Path, settings: &Settings) -> io::Result<()> {
    write(path, &serde_json::to_string_pretty(settings)?)
}

/// Replaces the file in one step, so a crash cannot leave half of it.
fn write(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let new = path.with_extension("new");
    fs::write(&new, text)?;
    fs::rename(&new, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_rejects_garbage() {
        let dir = env::temp_dir().join(format!("ghdeck-test-{}", std::process::id()));
        let path = dir.join("sub").join("excluded-teams");
        assert_eq!(load(&path).unwrap(), Vec::<String>::new());
        let teams = vec!["o/a".to_string(), "o/b-c".to_string()];
        save(&path, &teams).unwrap();
        save(&path, &teams).unwrap();
        assert_eq!(load(&path).unwrap(), teams);
        fs::write(&path, "o/a OR is:open\n").unwrap();
        assert!(load(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn view_survives_and_is_checked() {
        let choice = |v: &str| (v.to_string(), v.to_string());
        let dir = env::temp_dir().join(format!("ghdeck-view-{}", std::process::id()));
        let path = dir.join("view.json");
        assert_eq!(load_view(&path).unwrap(), None);
        let mut view = View {
            orgs: vec![choice("o"), choice("a b OR is:open")],
            repos: vec![choice("o/r"), choice("o/r is:closed")],
            tabs: Tab::ALL
                .map(|t| (api::default_filters(t), api::default_states(t)))
                .to_vec(),
            raw: true,
        };
        view.tabs[0].0.push(choice("team-review-requested:o/t"));
        view.tabs[0].0.push(choice("is:evil"));
        view.tabs[1].1.push(choice("is:evil"));
        save_view(&path, &view).unwrap();
        let got = load_view(&path).unwrap().unwrap();
        assert_eq!((got.orgs.len(), got.repos.len()), (1, 1));
        let kept = api::default_filters(Tab::Prs).len() + 1;
        assert_eq!((got.tabs[0].0.len(), got.tabs[1].1.len()), (kept, 1));
        assert!(got.raw);
        fs::write(&path, "{").unwrap();
        assert!(load_view(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ai_settings_survive() {
        let dir = env::temp_dir().join(format!("ghdeck-ai-{}", std::process::id()));
        let path = dir.join("ai.json");
        assert_eq!(load_ai(&path).unwrap(), Settings::default());
        let settings = Settings {
            tool: Some(crate::ai::Tool::Claude),
            blocked: vec!["o".into()],
            ..Settings::default()
        };
        save_ai(&path, &settings).unwrap();
        assert_eq!(load_ai(&path).unwrap(), settings);
        fs::write(&path, "{\"model\":\"--x\"}").unwrap();
        assert!(load_ai(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}

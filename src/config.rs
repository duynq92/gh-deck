//! The one saved setting: teams to leave out of "my teams".

use crate::api::valid_repo;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// `$XDG_CONFIG_HOME/ghdeck/excluded-teams`, or below `~/.config`.
pub fn path() -> Option<PathBuf> {
    let dir = |name| env::var_os(name).map(PathBuf::from);
    let base = dir("XDG_CONFIG_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| dir("HOME").map(|home| home.join(".config")))?;
    Some(base.join("ghdeck").join("excluded-teams"))
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

/// Replaces the file in one step, so a crash cannot leave half of it.
pub fn save(path: &Path, teams: &[String]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let new = path.with_extension("new");
    let text: String = teams.iter().map(|t| format!("{t}\n")).collect();
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
}

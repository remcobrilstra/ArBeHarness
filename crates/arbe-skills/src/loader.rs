use std::fs;
use std::path::Path;

use crate::error::SkillError;
use crate::manifest::parse_manifest;
use crate::{SkillManifest, SkillScope};

/// What loading a directory found: the skills that parsed, and a
/// problem for each file that didn't (so one bad file is reported, not
/// fatal to the rest).
#[derive(Debug, Default)]
pub struct Loaded {
    pub skills: Vec<SkillManifest>,
    pub problems: Vec<SkillError>,
}

/// Loads every `*.md` skill directly in `dir` (not subdirectories). A
/// missing directory is not an error — an empty scope is a normal
/// starting state. Files are read in name order, so results are stable.
pub fn load_dir(dir: &Path, scope: SkillScope) -> Loaded {
    let mut loaded = Loaded::default();
    if !dir.exists() {
        return loaded;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            loaded.problems.push(SkillError::Io {
                path: dir.display().to_string(),
                source,
            });
            return loaded;
        }
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();
    for path in paths {
        let parsed = fs::read_to_string(&path)
            .map_err(|source| SkillError::Io {
                path: path.display().to_string(),
                source,
            })
            .and_then(|text| parse_manifest(&text, scope, &path.display().to_string()));
        match parsed {
            Ok(skill) => loaded.skills.push(skill),
            Err(problem) => loaded.problems.push(problem),
        }
    }
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_yields_no_skills() {
        let dir =
            std::env::temp_dir().join(format!("arbe-skills-missing-{}", uuid::Uuid::new_v4()));
        let loaded = load_dir(&dir, SkillScope::Global);
        assert!(loaded.skills.is_empty());
        assert!(loaded.problems.is_empty());
    }

    #[test]
    fn loads_every_markdown_manifest_in_a_directory() {
        let dir = std::env::temp_dir().join(format!("arbe-skills-load-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("a.md"),
            "---\nname: a\ndescription: skill a\n---\nbody a",
        )
        .unwrap();
        fs::write(
            dir.join("b.md"),
            "---\nname: b\ndescription: skill b\n---\nbody b",
        )
        .unwrap();
        fs::write(dir.join("ignore.txt"), "not a skill").unwrap();

        let manifests = load_dir(&dir, SkillScope::ProjectLocal).skills;

        assert_eq!(manifests.len(), 2);
        assert_eq!(manifests[0].name, "a");
        assert_eq!(manifests[1].name, "b");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_file_is_reported_and_the_rest_still_load() {
        let dir = std::env::temp_dir().join(format!("arbe-skills-bad-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("good.md"),
            "---\nname: good\ndescription: ok\n---\nbody",
        )
        .unwrap();
        fs::write(
            dir.join("bad.md"),
            "---\nname: missing-description\n---\nbody",
        )
        .unwrap();

        let loaded = load_dir(&dir, SkillScope::Global);
        assert_eq!(loaded.skills.len(), 1);
        assert_eq!(loaded.skills[0].name, "good");
        assert_eq!(loaded.problems.len(), 1);
        assert!(loaded.problems[0].to_string().contains("bad.md"));

        fs::remove_dir_all(&dir).ok();
    }
}

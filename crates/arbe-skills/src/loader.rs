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

/// The file that makes a folder a skill (the `SKILL.md` format other
/// agents share), with the files it refers to next to it.
pub const SKILL_FILE: &str = "SKILL.md";

/// Loads the skills directly in `dir`: every `*.md` file, and every
/// folder holding a [`SKILL_FILE`] (deeper folders aren't searched). A
/// missing directory is not an error — an empty scope is a normal
/// starting state. Entries are read in name order, so results are stable.
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
        .filter_map(|p| {
            if p.is_dir() {
                let file = p.join(SKILL_FILE);
                file.is_file().then_some((file, Some(p)))
            } else {
                (p.extension().and_then(|e| e.to_str()) == Some("md")).then_some((p, None))
            }
        })
        .collect();
    paths.sort();
    for (path, folder) in paths {
        let parsed = fs::read_to_string(&path)
            .map_err(|source| SkillError::Io {
                path: path.display().to_string(),
                source,
            })
            .and_then(|text| parse_manifest(&text, scope, &path.display().to_string()));
        match parsed {
            Ok(skill) => loaded.skills.push(SkillManifest {
                dir: folder,
                ..skill
            }),
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
    fn a_folder_with_a_skill_file_is_a_skill_and_remembers_its_folder() {
        let dir = std::env::temp_dir().join(format!("arbe-skills-dir-{}", uuid::Uuid::new_v4()));
        let folder = dir.join("pdf");
        fs::create_dir_all(folder.join("scripts")).unwrap();
        fs::write(
            folder.join(SKILL_FILE),
            "---\nname: pdf\ndescription: PDFs\n---\nRun scripts/fill.py",
        )
        .unwrap();
        fs::write(folder.join("scripts/fill.py"), "").unwrap();
        // A folder without a skill file is not a skill, and not an error.
        fs::create_dir_all(dir.join("notes")).unwrap();
        fs::write(
            dir.join("single.md"),
            "---\nname: single\ndescription: s\n---\nb",
        )
        .unwrap();

        let loaded = load_dir(&dir, SkillScope::ProjectLocal);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let names: Vec<_> = loaded.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["pdf", "single"]);
        assert_eq!(loaded.skills[0].dir.as_deref(), Some(folder.as_path()));
        assert_eq!(loaded.skills[1].dir, None);

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

use std::fs;
use std::path::Path;

use crate::error::SkillError;
use crate::manifest::parse_manifest;
use crate::{SkillManifest, SkillScope};

/// Loads every `*.md` manifest directly inside `dir` (non-recursive). A
/// missing directory is not an error — an empty scope is a normal starting
/// state (mirrors `arbe_storage::memory_files` treating a missing memory
/// file as `None`, not a failure).
pub fn load_dir(dir: &Path, scope: SkillScope) -> Result<Vec<SkillManifest>, SkillError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut manifests = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| SkillError::Io {
        path: dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SkillError::Io {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|source| SkillError::Io {
            path: path.display().to_string(),
            source,
        })?;
        manifests.push(parse_manifest(&text, scope, &path.display().to_string())?);
    }
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_yields_no_skills() {
        let dir =
            std::env::temp_dir().join(format!("arbe-skills-missing-{}", uuid::Uuid::new_v4()));
        let manifests = load_dir(&dir, SkillScope::Global).unwrap();
        assert!(manifests.is_empty());
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

        let mut manifests = load_dir(&dir, SkillScope::ProjectLocal).unwrap();
        manifests.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(manifests.len(), 2);
        assert_eq!(manifests[0].name, "a");
        assert_eq!(manifests[1].name, "b");

        fs::remove_dir_all(&dir).ok();
    }
}

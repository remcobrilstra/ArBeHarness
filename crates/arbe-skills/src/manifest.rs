use crate::error::SkillError;
use crate::{SkillManifest, SkillScope};

/// Parses a skill manifest of the form:
///
/// ```text
/// ---
/// name: my-skill
/// description: does a thing
/// tags: a, b, c
/// ---
/// Instructions body, everything after the closing `---` line.
/// ```
///
/// (harness spec FR-6: "Load skills from markdown manifests.") `path` is
/// only used to produce a useful error message.
pub fn parse_manifest(
    text: &str,
    scope: SkillScope,
    path: &str,
) -> Result<SkillManifest, SkillError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // tolerate a BOM
    let rest = text
        .strip_prefix("---")
        .ok_or_else(|| SkillError::Malformed {
            path: path.to_string(),
            reason: "manifest must start with a `---` frontmatter block".to_string(),
        })?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);

    let Some(end) = rest.find("\n---") else {
        return Err(SkillError::Malformed {
            path: path.to_string(),
            reason: "frontmatter block is not closed with a second `---` line".to_string(),
        });
    };
    let frontmatter = &rest[..end];
    let body = rest[end + "\n---".len()..].trim_start_matches(['\r', '\n']);

    let mut name = None;
    let mut description = None;
    let mut tags = Vec::new();
    for line in frontmatter.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => name = Some(value.to_string()),
            "description" => description = Some(value.to_string()),
            "tags" => {
                tags = value
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            }
            _ => {}
        }
    }

    let name = name.ok_or_else(|| SkillError::Malformed {
        path: path.to_string(),
        reason: "frontmatter is missing required `name` field".to_string(),
    })?;
    let description = description.ok_or_else(|| SkillError::Malformed {
        path: path.to_string(),
        reason: "frontmatter is missing required `description` field".to_string(),
    })?;

    Ok(SkillManifest {
        name,
        description,
        scope,
        instructions: body.to_string(),
        tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_manifest() {
        let text = "---\nname: rust-helper\ndescription: helps with Rust\ntags: rust, cargo\n---\nAlways run cargo fmt.\n";
        let manifest = parse_manifest(text, SkillScope::Global, "test.md").unwrap();
        assert_eq!(manifest.name, "rust-helper");
        assert_eq!(manifest.description, "helps with Rust");
        assert_eq!(manifest.tags, vec!["rust", "cargo"]);
        assert_eq!(manifest.instructions, "Always run cargo fmt.\n");
        assert_eq!(manifest.scope, SkillScope::Global);
    }

    #[test]
    fn missing_frontmatter_is_malformed() {
        let err = parse_manifest("just some text", SkillScope::Global, "test.md").unwrap_err();
        assert!(matches!(err, SkillError::Malformed { .. }));
    }

    #[test]
    fn unclosed_frontmatter_is_malformed() {
        let err = parse_manifest(
            "---\nname: x\ndescription: y\n",
            SkillScope::Global,
            "test.md",
        )
        .unwrap_err();
        assert!(matches!(err, SkillError::Malformed { .. }));
    }

    #[test]
    fn missing_required_field_is_malformed() {
        let err =
            parse_manifest("---\nname: x\n---\nbody", SkillScope::Global, "test.md").unwrap_err();
        assert!(matches!(err, SkillError::Malformed { .. }));
    }

    #[test]
    fn windows_line_endings_are_accepted() {
        let manifest = parse_manifest(
            "---\r\nname: x\r\ndescription: y\r\n---\r\nbody\r\n",
            SkillScope::Global,
            "test.md",
        )
        .unwrap();
        assert_eq!(manifest.name, "x");
        assert_eq!(manifest.description, "y");
        assert_eq!(manifest.instructions, "body\r\n");
    }

    #[test]
    fn tags_are_optional() {
        let manifest = parse_manifest(
            "---\nname: x\ndescription: y\n---\nbody",
            SkillScope::ProjectLocal,
            "test.md",
        )
        .unwrap();
        assert!(manifest.tags.is_empty());
    }
}

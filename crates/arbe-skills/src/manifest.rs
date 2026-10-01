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
    for (key, value) in top_level_fields(frontmatter) {
        match key {
            "name" => name = Some(value),
            "description" => description = Some(value),
            "tags" => {
                tags = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
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
        dir: None,
    })
}

/// The frontmatter's top-level `key: value` fields, read as the small part
/// of YAML that skill files use (including files written for other agents'
/// `SKILL.md` format): quoted values are unquoted, a `|` or `>` block value
/// is read from the indented lines below it, and indented lines that belong
/// to a nested field (`metadata:` and the like) are skipped.
fn top_level_fields(frontmatter: &str) -> Vec<(&str, String)> {
    let lines: Vec<&str> = frontmatter.lines().collect();
    let mut fields = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        i += 1;
        if line.is_empty() || line.starts_with([' ', '\t', '#']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let value = if let Some(style) = value.chars().next().filter(|c| matches!(c, '|' | '>')) {
            let mut block = Vec::new();
            while i < lines.len() {
                let next = lines[i].trim_end();
                if !next.is_empty() && !next.starts_with([' ', '\t']) {
                    break;
                }
                block.push(next.trim());
                i += 1;
            }
            let separator = if style == '|' { "\n" } else { " " };
            block
                .split(|l| l.is_empty())
                .map(|paragraph| paragraph.join(separator))
                .filter(|paragraph| !paragraph.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            unquote(value).to_string()
        };
        fields.push((key.trim(), value));
    }
    fields
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_yaml_that_other_agents_skill_files_use() {
        let text = "---\n\
                    name: \"pdf-tools\"\n\
                    description: >\n  Extracts text from PDFs\n  and fills forms.\n\n  Use for any .pdf file.\n\
                    license: Apache-2.0\n\
                    allowed-tools: [read_file]\n\
                    metadata:\n  name: not-the-skill-name\n  version: '1.0'\n\
                    tags: [pdf, forms]\n\
                    ---\n\
                    Steps.\n";
        let manifest = parse_manifest(text, SkillScope::Global, "SKILL.md").unwrap();
        assert_eq!(manifest.name, "pdf-tools");
        assert_eq!(
            manifest.description,
            "Extracts text from PDFs and fills forms.\nUse for any .pdf file."
        );
        assert_eq!(manifest.tags, vec!["pdf", "forms"]);
        assert_eq!(manifest.instructions, "Steps.\n");
    }

    #[test]
    fn a_literal_block_keeps_its_lines() {
        let text = "---\nname: x\ndescription: |-\n  line one\n  line two\n---\nbody";
        let manifest = parse_manifest(text, SkillScope::Global, "x.md").unwrap();
        assert_eq!(manifest.description, "line one\nline two");
    }

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

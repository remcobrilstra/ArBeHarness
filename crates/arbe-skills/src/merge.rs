use crate::SkillManifest;

/// Merges the project's skills with the global ones, keeping the project's
/// when names collide (harness spec FR-6 resolution order; the spec's third,
/// session-local scope isn't implemented). Order of the output otherwise
/// follows first appearance (project, then global).
pub fn merge_skills(
    project_local: Vec<SkillManifest>,
    global: Vec<SkillManifest>,
) -> Vec<SkillManifest> {
    let mut merged: Vec<SkillManifest> = Vec::new();
    for manifest in project_local.into_iter().chain(global) {
        if merged.iter().any(|m| m.name == manifest.name) {
            continue;
        }
        merged.push(manifest);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkillScope;

    fn skill(name: &str, scope: SkillScope, instructions: &str) -> SkillManifest {
        SkillManifest {
            name: name.to_string(),
            description: "d".to_string(),
            scope,
            instructions: instructions.to_string(),
            tags: vec![],
        }
    }

    #[test]
    fn project_wins_over_global_on_name_collision() {
        let project = vec![skill("shared", SkillScope::ProjectLocal, "project version")];
        let global = vec![skill("shared", SkillScope::Global, "global version")];

        let merged = merge_skills(project, global);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].instructions, "project version");
    }

    #[test]
    fn non_colliding_skills_from_both_scopes_are_all_kept() {
        let project = vec![skill("p", SkillScope::ProjectLocal, "p")];
        let global = vec![skill("g", SkillScope::Global, "g")];

        let merged = merge_skills(project, global);
        assert_eq!(merged.len(), 2);
    }
}

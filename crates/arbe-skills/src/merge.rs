use crate::SkillManifest;

/// Merges skills from all three scopes, keeping session-local over
/// project-local over global when names collide (harness spec FR-6
/// resolution order). Order of the output otherwise follows first
/// appearance (session, then project, then global).
pub fn merge_skills(
    session_local: Vec<SkillManifest>,
    project_local: Vec<SkillManifest>,
    global: Vec<SkillManifest>,
) -> Vec<SkillManifest> {
    let mut merged: Vec<SkillManifest> = Vec::new();
    for manifest in session_local.into_iter().chain(project_local).chain(global) {
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
    fn session_local_wins_over_project_and_global_on_name_collision() {
        let session = vec![skill("shared", SkillScope::SessionLocal, "session version")];
        let project = vec![skill("shared", SkillScope::ProjectLocal, "project version")];
        let global = vec![skill("shared", SkillScope::Global, "global version")];

        let merged = merge_skills(session, project, global);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].instructions, "session version");
    }

    #[test]
    fn non_colliding_skills_from_every_scope_are_all_kept() {
        let session = vec![skill("s", SkillScope::SessionLocal, "s")];
        let project = vec![skill("p", SkillScope::ProjectLocal, "p")];
        let global = vec![skill("g", SkillScope::Global, "g")];

        let merged = merge_skills(session, project, global);
        assert_eq!(merged.len(), 3);
    }
}

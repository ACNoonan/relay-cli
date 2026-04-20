//! Process-wide registry of loaded skills, indexed by name.
//!
//! Discovery scopes:
//!
//! 1. **Global**: `$XDG_CONFIG_HOME/relay/skills/*.md`, falling back to
//!    `$HOME/.config/relay/skills/*.md`.
//! 2. **Project**: `<harness_dir>/skills/*.md` (e.g. `.agent-harness/skills/`).
//!
//! Project-scope wins on cross-scope name collisions; the shadowed global is
//! reported as an info diagnostic so users can debug "why didn't my global
//! skill run?"

use camino::{Utf8Path, Utf8PathBuf};

use super::loader::{load_skills_from_dir, Skill, SkillError, SkillScope};

/// In-memory collection of loaded skills + per-file load errors.
///
/// Construct via [`SkillRegistry::load`]. The registry is immutable after
/// construction; rebuild it (cheap — file IO is O(skills)) if you want to
/// pick up edits without restarting chat.
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
    errors: Vec<SkillError>,
}

impl SkillRegistry {
    /// Discover and parse all skills from both scopes. Project-scope skills
    /// override globals on name collision (with an info-level error pushed so
    /// users can spot the shadow in `/skills` output).
    ///
    /// `harness_dir` is the project's harness root (typically
    /// `<cwd>/.agent-harness`). Pass `None` to skip the project scope (e.g.
    /// when the chat is running outside an initialised harness).
    pub fn load(harness_dir: Option<&Utf8Path>) -> Self {
        let mut errors: Vec<SkillError> = Vec::new();

        let global_dir = global_skills_dir();
        let (mut globals, global_errors) = match global_dir {
            Some(p) => load_skills_from_dir(&p, SkillScope::Global),
            None => (Vec::new(), Vec::new()),
        };
        errors.extend(global_errors);

        let (project, project_errors) = match harness_dir {
            Some(d) => {
                let project_dir = d.join("skills");
                load_skills_from_dir(&project_dir, SkillScope::Project)
            }
            None => (Vec::new(), Vec::new()),
        };
        errors.extend(project_errors);

        // Project shadows global. Walk project names, drop matching globals,
        // and emit a diagnostic so the shadow is visible in `/skills`.
        for skill in &project {
            if let Some(idx) = globals.iter().position(|g| g.name == skill.name) {
                let shadowed = globals.remove(idx);
                errors.push(SkillError {
                    path: shadowed.source_path.clone(),
                    message: format!(
                        "shadowed by project skill at {} (project scope wins)",
                        skill.source_path
                    ),
                });
            }
        }

        let mut skills = globals;
        skills.extend(project);
        // Stable order for `/skills` output: by name.
        skills.sort_by(|a, b| a.name.cmp(&b.name));

        Self { skills, errors }
    }

    /// All loaded skills, sorted by name.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// All collected errors (per-file diagnostics + cross-scope shadow notes).
    pub fn errors(&self) -> &[SkillError] {
        &self.errors
    }

    /// Find a skill by exact name (case-insensitive on the input — names
    /// themselves are forced to lowercase by the loader).
    pub fn find(&self, name: &str) -> Option<&Skill> {
        let needle = name.to_ascii_lowercase();
        self.skills.iter().find(|s| s.name == needle)
    }

    /// `(name, description)` pairs for the autocomplete popup. Cloned so the
    /// caller can pass ownership through the editor without holding a registry
    /// borrow across `&mut UiState`.
    pub fn names_with_descriptions(&self) -> Vec<(String, String)> {
        self.skills
            .iter()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect()
    }

    /// Construct a registry directly from already-built skills + errors.
    /// Test-only: production paths must go through [`SkillRegistry::load`]
    /// so scope precedence + per-file diagnostics are applied uniformly.
    #[cfg(test)]
    pub fn from_parts(skills: Vec<Skill>, errors: Vec<SkillError>) -> Self {
        Self { skills, errors }
    }
}

/// Resolve `~/.config/relay/skills/`, honouring `XDG_CONFIG_HOME`. Returns
/// `None` when neither env var nor `HOME` is set (very rare; e.g. some CI
/// environments) — in that case we just skip global skills silently.
fn global_skills_dir() -> Option<Utf8PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    let path = base.join("relay").join("skills");
    Utf8PathBuf::try_from(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn write_skill(dir: &Utf8Path, file: &str, name: &str, desc: &str, rotation: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(file),
            format!("---\nname: {name}\ndescription: {desc}\nrotation: {rotation}\n---\n"),
        )
        .unwrap();
    }

    #[test]
    fn project_overrides_global() {
        let tmp = tempfile::tempdir().unwrap();
        let xdg = Utf8PathBuf::try_from(tmp.path().join("xdg")).unwrap();
        let harness = Utf8PathBuf::try_from(tmp.path().join("harness")).unwrap();

        let global = xdg.join("relay").join("skills");
        let project = harness.join("skills");

        write_skill(&global, "shared.md", "shared", "global version", "[claude]");
        write_skill(&global, "only_global.md", "global-only", "g", "[gpt]");
        write_skill(
            &project,
            "shared.md",
            "shared",
            "project version",
            "[codex]",
        );
        write_skill(&project, "only_project.md", "project-only", "p", "[claude]");

        // Sandbox env so the global resolver picks our temp dir.
        let prev_xdg = env::var_os("XDG_CONFIG_HOME");
        let prev_home = env::var_os("HOME");
        env::set_var("XDG_CONFIG_HOME", xdg.as_std_path());
        env::remove_var("HOME");

        let reg = SkillRegistry::load(Some(harness.as_path()));

        // Restore env.
        match prev_xdg {
            Some(v) => env::set_var("XDG_CONFIG_HOME", v),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Some(h) = prev_home {
            env::set_var("HOME", h);
        }

        let names: Vec<&str> = reg.skills().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["global-only", "project-only", "shared"]);

        let shared = reg.find("shared").expect("shared exists");
        assert_eq!(shared.scope, SkillScope::Project);
        assert_eq!(shared.description, "project version");

        // Shadow note emitted.
        assert!(
            reg.errors()
                .iter()
                .any(|e| e.message.contains("shadowed by project skill")),
            "expected shadow diagnostic; got {:?}",
            reg.errors()
        );
    }

    #[test]
    fn missing_dirs_yield_empty_registry() {
        let prev_xdg = env::var_os("XDG_CONFIG_HOME");
        let prev_home = env::var_os("HOME");
        // Point both at a guaranteed-nonexistent path.
        env::set_var("XDG_CONFIG_HOME", "/no/such/relay/config");
        env::remove_var("HOME");

        let reg = SkillRegistry::load(None);

        match prev_xdg {
            Some(v) => env::set_var("XDG_CONFIG_HOME", v),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Some(h) = prev_home {
            env::set_var("HOME", h);
        }

        assert!(reg.skills().is_empty());
        assert!(reg.errors().is_empty());
    }
}

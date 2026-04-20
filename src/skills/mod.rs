//! User-invocable handoff recipes loaded from markdown files.
//!
//! Tier 3 #12 of `PI_MONO_LEARNINGS.md`, deliberately reframed for relay's
//! product surface:
//!
//! * Pi's "skills" are model-invocable system prompts (the LLM decides when to
//!   inject them). Relay does not run an agent loop — there is no model to
//!   make that decision — so the relay-equivalent is a **user-invocable
//!   recipe**: a markdown file that defines a multi-agent rotation with
//!   optional per-agent guidance, exposed as a slash command.
//!
//! Structure:
//! * [`loader`] — frontmatter parsing + on-disk discovery.
//! * [`registry`] — collected skills indexed by name, with project-overrides-
//!   global precedence, plus per-file diagnostic errors so a single malformed
//!   skill never breaks chat startup.
//!
//! See `assets/skills/security-review.md` for the canonical file format.

pub mod loader;
pub mod registry;

pub use loader::{
    load_skills_from_dir, parse_skill_file, Skill, SkillError, SkillFrontmatter, SkillScope,
};
pub use registry::SkillRegistry;

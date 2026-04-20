//! Schema migration runner for relay's persisted state.
//!
//! Operates on `serde_json::Value` so schema changes may add, remove, or
//! rename fields the typed struct no longer matches. Callers read a raw JSON
//! value from disk, run it through [`MigrationRegistry::migrate_to`], and
//! only then attempt typed deserialization.
//!
//! # Shape
//!
//! Each migration implements [`Migration`] and declares its `from_version`
//! and `to_version` (typically `from + 1`). A [`MigrationRegistry`] holds a
//! set of migrations keyed by `from_version`, then walks from the document's
//! current version to the target, applying one step at a time.
//!
//! Migrations are pure transformations and must be idempotent under
//! re-application from the same `from_version`.
//!
//! # Errors
//!
//! - A gap in the chain (document at `vN` but no migration from `vN`)
//!   returns a "missing migration" error naming the step.
//! - A document whose version is *greater* than the caller's target
//!   (e.g. a v7 file seen by a binary that only knows up to v5) returns a
//!   clear "future schema version" error. Silent pass-through would corrupt
//!   data on the next save.
//! - A migration function returning `Err` is wrapped with its `describe()`
//!   text so the caller can trace which step failed.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// A single schema migration step. Moves a raw JSON document from
/// `from_version` to `to_version` (usually one higher).
#[allow(clippy::wrong_self_convention)] // `from_version` / `to_version` read as nouns here
pub trait Migration: Send + Sync {
    fn from_version(&self) -> u32;
    fn to_version(&self) -> u32;
    /// Short human-readable description used in reports and error contexts.
    fn describe(&self) -> &'static str;
    fn migrate(&self, raw: &mut Value) -> Result<()>;
}

/// Holds an ordered set of [`Migration`]s indexed by `from_version` and
/// runs them against a raw JSON document.
#[derive(Default)]
pub struct MigrationRegistry {
    migrations: Vec<Box<dyn Migration>>,
}

/// Summary of what a [`MigrationRegistry::migrate_to`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub starting_version: u32,
    pub final_version: u32,
    pub steps_applied: Vec<&'static str>,
}

impl MigrationReport {
    pub fn upgraded(&self) -> bool {
        self.final_version > self.starting_version
    }
}

impl MigrationRegistry {
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    /// Register a migration. Panics on duplicate `from_version` so chain
    /// conflicts are caught at construction time, not at first load.
    pub fn register<M: Migration + 'static>(&mut self, m: M) {
        let from = m.from_version();
        if self
            .migrations
            .iter()
            .any(|existing| existing.from_version() == from)
        {
            panic!(
                "duplicate migration registered for from_version = {from} ({})",
                m.describe()
            );
        }
        self.migrations.push(Box::new(m));
        self.migrations.sort_by_key(|mig| mig.from_version());
    }

    /// Run migrations in order until `raw`'s `schema_version` reaches `target`.
    ///
    /// The current version is read from the `schema_version` field on `raw`,
    /// defaulting to `1` if absent (legacy v1 docs pre-dated the field).
    /// After each step we read `schema_version` again from the mutated doc and
    /// verify it matches the step's `to_version` — otherwise we bail, so a
    /// broken migration can't silently leave the doc in an inconsistent state.
    pub fn migrate_to(&self, raw: &mut Value, target: u32) -> Result<MigrationReport> {
        let starting_version = current_schema_version(raw);
        if starting_version > target {
            bail!(
                "future schema version {starting_version} exceeds target {target}; \
                 this binary is too old to read the document. Upgrade relay."
            );
        }

        let mut steps_applied: Vec<&'static str> = Vec::new();
        let mut current = starting_version;

        while current < target {
            let Some(step) = self.find_step(current) else {
                bail!(
                    "missing migration from schema version {current} to {target}; \
                     no registered step starts at from_version: {current}"
                );
            };
            let expected_to = step.to_version();
            if expected_to <= current {
                bail!(
                    "migration '{}' declares to_version {} that is not greater than from_version {}",
                    step.describe(),
                    expected_to,
                    current
                );
            }

            step.migrate(raw).with_context(|| {
                format!(
                    "applying migration '{}' (v{} -> v{})",
                    step.describe(),
                    current,
                    expected_to
                )
            })?;

            // The migration is responsible for bumping schema_version on the
            // doc. We verify, rather than blindly overwriting, to catch bugs
            // in the migration body.
            set_schema_version_if_missing(raw, expected_to);
            let after = current_schema_version(raw);
            if after != expected_to {
                bail!(
                    "migration '{}' left schema_version = {after}, expected {expected_to}",
                    step.describe()
                );
            }

            steps_applied.push(step.describe());
            current = after;
        }

        Ok(MigrationReport {
            starting_version,
            final_version: current,
            steps_applied,
        })
    }

    fn find_step(&self, from: u32) -> Option<&dyn Migration> {
        self.migrations
            .iter()
            .find(|m| m.from_version() == from)
            .map(|b| b.as_ref())
    }
}

/// Read `schema_version` off a raw JSON document, defaulting to `1` when the
/// field is missing (pre-versioned legacy docs).
fn current_schema_version(raw: &Value) -> u32 {
    raw.as_object()
        .and_then(|o| o.get("schema_version"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(1)
}

/// If `schema_version` is absent (legacy v1), set it so the subsequent check
/// in [`MigrationRegistry::migrate_to`] can validate the step ran. Does
/// nothing if the migration already set the field.
fn set_schema_version_if_missing(raw: &mut Value, version: u32) {
    if let Value::Object(map) = raw {
        if !map.contains_key("schema_version") {
            map.insert("schema_version".into(), Value::from(version));
        }
    }
}

// -----------------------------------------------------------------------------
// Conversation-specific migrations
// -----------------------------------------------------------------------------

/// Build the registry for `conversation.json`. Ordered oldest → newest.
///
/// Each migration is registered with its `from_version`; the registry itself
/// keeps them sorted. Add new migrations here when bumping
/// `CONVERSATION_SCHEMA_VERSION` in [`crate::bridge::conversation`].
pub fn conversation_registry() -> MigrationRegistry {
    let mut reg = MigrationRegistry::new();
    reg.register(V1ToV2Migration);
    reg
}

/// v1 → v2: introduced `schema_version` and `Turn.summarized_turn_count`.
///
/// Pre-existing turns are not summaries, so the serde default of `None` on
/// `summarized_turn_count` already handles the new field on deserialize.
/// We only need to stamp `schema_version = 2` on the doc so saves round-trip
/// cleanly without relying on the legacy default.
///
/// This replicates the behaviour of the now-removed
/// `Conversation::upgrade_in_place`, which also did nothing beyond bumping
/// the version marker.
pub struct V1ToV2Migration;

impl Migration for V1ToV2Migration {
    fn from_version(&self) -> u32 {
        1
    }

    fn to_version(&self) -> u32 {
        2
    }

    fn describe(&self) -> &'static str {
        "conversation v1 -> v2: add schema_version marker"
    }

    fn migrate(&self, raw: &mut Value) -> Result<()> {
        let obj = raw
            .as_object_mut()
            .ok_or_else(|| anyhow!("conversation JSON root is not an object"))?;
        obj.insert("schema_version".into(), Value::from(2u32));
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Helper: a no-op migration that only bumps the version field. Useful
    /// for exercising chain composition without tying tests to real schema
    /// details.
    struct NoopMigration {
        from: u32,
        to: u32,
        label: &'static str,
    }

    impl Migration for NoopMigration {
        fn from_version(&self) -> u32 {
            self.from
        }
        fn to_version(&self) -> u32 {
            self.to
        }
        fn describe(&self) -> &'static str {
            self.label
        }
        fn migrate(&self, raw: &mut Value) -> Result<()> {
            let to = self.to;
            raw.as_object_mut()
                .ok_or_else(|| anyhow!("root is not an object"))?
                .insert("schema_version".into(), Value::from(to));
            Ok(())
        }
    }

    struct FailingMigration;

    impl Migration for FailingMigration {
        fn from_version(&self) -> u32 {
            1
        }
        fn to_version(&self) -> u32 {
            2
        }
        fn describe(&self) -> &'static str {
            "intentionally failing step"
        }
        fn migrate(&self, _raw: &mut Value) -> Result<()> {
            Err(anyhow!("deliberate failure"))
        }
    }

    #[test]
    fn empty_registry_is_noop_when_already_at_target() {
        let reg = MigrationRegistry::new();
        let mut doc = json!({ "schema_version": 1, "payload": "hello" });
        let report = reg.migrate_to(&mut doc, 1).expect("no-op should succeed");
        assert_eq!(report.starting_version, 1);
        assert_eq!(report.final_version, 1);
        assert!(report.steps_applied.is_empty());
        assert!(!report.upgraded());
        assert_eq!(doc["payload"], json!("hello"));
    }

    #[test]
    fn v1_to_v2_upgrades_and_lists_step() {
        let mut reg = MigrationRegistry::new();
        reg.register(V1ToV2Migration);
        // Legacy v1 doc: no schema_version field.
        let mut doc = json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "turns": [],
        });
        let report = reg.migrate_to(&mut doc, 2).expect("v1 -> v2");
        assert_eq!(report.starting_version, 1);
        assert_eq!(report.final_version, 2);
        assert_eq!(report.steps_applied.len(), 1);
        assert!(report.upgraded());
        assert_eq!(doc["schema_version"], json!(2));
    }

    #[test]
    fn missing_step_in_chain_errors_with_from_version() {
        // Only v1 -> v2 registered; asking for v3 should fail at the v2 step.
        let mut reg = MigrationRegistry::new();
        reg.register(V1ToV2Migration);
        let mut doc = json!({});
        let err = reg
            .migrate_to(&mut doc, 3)
            .expect_err("chain gap must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("from_version: 2"),
            "error should name missing step's from_version: {msg}"
        );
    }

    #[test]
    fn future_version_doc_errors_clearly() {
        let mut reg = MigrationRegistry::new();
        reg.register(V1ToV2Migration);
        let mut doc = json!({ "schema_version": 5 });
        let err = reg
            .migrate_to(&mut doc, 2)
            .expect_err("future version must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("future schema version"),
            "error should mention 'future schema version': {msg}"
        );
        assert!(
            msg.contains('5'),
            "error should name the doc's version: {msg}"
        );
    }

    #[test]
    fn migration_error_propagates_with_step_description() {
        let mut reg = MigrationRegistry::new();
        reg.register(FailingMigration);
        let mut doc = json!({ "schema_version": 1 });
        let err = reg
            .migrate_to(&mut doc, 2)
            .expect_err("failing migration must propagate");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("intentionally failing step"),
            "error should name the step: {msg}"
        );
        assert!(
            msg.contains("deliberate failure"),
            "error should carry the source: {msg}"
        );
    }

    #[test]
    fn multi_step_chain_runs_in_order() {
        let mut reg = MigrationRegistry::new();
        // Register out of order to confirm the registry sorts them.
        reg.register(NoopMigration {
            from: 2,
            to: 3,
            label: "v2->v3",
        });
        reg.register(NoopMigration {
            from: 1,
            to: 2,
            label: "v1->v2",
        });
        let mut doc = json!({});
        let report = reg.migrate_to(&mut doc, 3).expect("chain");
        assert_eq!(report.starting_version, 1);
        assert_eq!(report.final_version, 3);
        assert_eq!(report.steps_applied, vec!["v1->v2", "v2->v3"]);
    }

    #[test]
    #[should_panic(expected = "duplicate migration")]
    fn duplicate_from_version_panics_at_registration() {
        let mut reg = MigrationRegistry::new();
        reg.register(NoopMigration {
            from: 1,
            to: 2,
            label: "first",
        });
        reg.register(NoopMigration {
            from: 1,
            to: 2,
            label: "second",
        });
    }

    #[test]
    fn conversation_registry_knows_v1_to_v2() {
        let reg = conversation_registry();
        let mut doc = json!({ "id": "x", "turns": [] });
        let report = reg.migrate_to(&mut doc, 2).expect("registry v1->v2");
        assert!(report.upgraded());
        assert_eq!(doc["schema_version"], json!(2));
    }
}

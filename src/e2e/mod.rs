use anyhow::Result;

use crate::storage::Storage;

/// Run E2E test commands and capture artifacts.
pub async fn run_e2e(storage: &Storage, commands: &[String]) -> Result<crate::schema::TestResult> {
    // E2E reuses the test runner with E2E-specific commands.
    crate::testing::run_tests(storage, commands, None).await
}

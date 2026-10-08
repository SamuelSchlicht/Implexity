// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Passed,
    Failed,
    NotExercised,
}

impl CheckStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::NotExercised => "not_exercised",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub required: bool,
    pub detail: String,
}

#[must_use]
pub fn classify_test_command(exit_code: i32, output: &str, collected_tests: Option<i64>) -> CheckStatus {
    let text = output.to_lowercase();
    if exit_code == 5 || text.contains("no tests ran") || text.contains("collected 0 items") {
        return CheckStatus::NotExercised;
    }
    if exit_code != 0 {
        return CheckStatus::Failed;
    }
    if collected_tests.is_some_and(|n| n <= 0) {
        return CheckStatus::NotExercised;
    }
    CheckStatus::Passed
}

#[must_use]
pub fn aggregate_gate(checks: &[CheckResult]) -> CheckStatus {
    let required: Vec<_> = checks.iter().filter(|c| c.required).collect();
    if required.iter().any(|c| c.status == CheckStatus::Failed) {
        return CheckStatus::Failed;
    }
    if required.iter().any(|c| c.status == CheckStatus::NotExercised) {
        return CheckStatus::NotExercised;
    }
    CheckStatus::Passed
}



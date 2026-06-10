use crate::error::HarvestResult;
use crate::harness::TestCase;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

/// Runs a binary with test case inputs and enforces a timeout.
pub fn run_binary_with_timeout(
    binary_path: &Path,
    test_case: &TestCase,
    timeout: Duration,
) -> HarvestResult<Output> {
    exec_runner::run_with_timeout(
        binary_path,
        &test_case.argv,
        test_case.stdin.as_deref(),
        timeout,
    )
}

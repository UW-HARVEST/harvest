use serde::Serialize;

/// Statistics for a a single test on a program
#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub filename: String,
    pub passed: bool,
    pub skipped: bool,
}

/// Statistics for running many tests on a single program
#[derive(Debug, Serialize)]
pub struct ProgramEvalStats {
    pub program_name: String,
    pub translation_success: bool,
    pub rust_build_success: bool,
    pub total_tests: usize,
    pub passed_tests: usize,
    pub skipped_tests: usize,
    pub error_message: Option<String>,
    // Store individual test results with filenames and pass/fail status
    pub test_results: Vec<TestResult>,
}

impl ProgramEvalStats {
    pub fn new(program_name: &str) -> Self {
        ProgramEvalStats {
            program_name: program_name.to_string(),
            translation_success: false,
            rust_build_success: false,
            total_tests: 0,
            passed_tests: 0,
            skipped_tests: 0,
            error_message: None,
            test_results: Vec::new(),
        }
    }

    /// Number of tests that were actually evaluated (non-skipped)
    pub fn evaluated_tests(&self) -> usize {
        self.total_tests.saturating_sub(self.skipped_tests)
    }

    /// Number of evaluated tests that failed
    pub fn failed_tests(&self) -> usize {
        self.evaluated_tests().saturating_sub(self.passed_tests)
    }

    /// Calculate success rate as a percentage
    pub fn success_rate(&self) -> f64 {
        let evaluated_tests = self.evaluated_tests();
        if evaluated_tests == 0 {
            return 0.0;
        };

        (self.passed_tests as f64 / evaluated_tests as f64) * 100.0
    }
}

/// Summary statistics across all program runs
#[derive(Debug, Serialize)]
pub struct SummaryStats {
    pub num_programs: usize,
    pub successful_translations: usize,
    pub successful_rust_builds: usize,
    pub total_tests: usize,
    pub total_skipped_tests: usize,
    pub total_passed_tests: usize,
}

impl SummaryStats {
    /// Number of tests that were actually evaluated (non-skipped)
    pub fn evaluated_tests(&self) -> usize {
        self.total_tests.saturating_sub(self.total_skipped_tests)
    }

    /// Number of evaluated tests that failed
    pub fn failed_tests(&self) -> usize {
        self.evaluated_tests()
            .saturating_sub(self.total_passed_tests)
    }

    /// Calculate overall test success rate as a percentage
    pub fn overall_success_rate(&self) -> f64 {
        let evaluated_tests = self.evaluated_tests();
        if evaluated_tests == 0 {
            0.0
        } else {
            (self.total_passed_tests as f64 / evaluated_tests as f64) * 100.0
        }
    }

    /// Calculate translation success rate as a percentage
    pub fn translation_success_rate(&self) -> f64 {
        if self.num_programs == 0 {
            0.0
        } else {
            (self.successful_translations as f64 / self.num_programs as f64) * 100.0
        }
    }

    /// Calculate Rust build success rate as a percentage
    pub fn rust_build_success_rate(&self) -> f64 {
        if self.num_programs == 0 {
            0.0
        } else {
            (self.successful_rust_builds as f64 / self.num_programs as f64) * 100.0
        }
    }

    /// Create SummaryStats from a slice of ProgramEvalStats
    pub fn from_results(results: &[ProgramEvalStats]) -> Self {
        // Calculate summary statistics
        SummaryStats {
            num_programs: results.len(),
            successful_translations: results.iter().filter(|r| r.translation_success).count(),
            successful_rust_builds: results.iter().filter(|r| r.rust_build_success).count(),
            total_tests: results.iter().map(|r| r.total_tests).sum(),
            total_skipped_tests: results.iter().map(|r| r.skipped_tests).sum(),
            total_passed_tests: results.iter().map(|r| r.passed_tests).sum(),
        }
    }
}

use std::{fs::File, path::PathBuf, process::Command};

use full_source::RawSource;
use harvest_core::{Representation, tools::Tool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum BugType {
    ARBITRARY_CODE_EXECUTION_UNDER_LOCK,
    BAD_ARG,
    BAD_ARG_LATENT,
    BAD_GENERATOR,
    BAD_GENERATOR_LATENT,
    BAD_KEY,
    BAD_KEY_LATENT,
    BAD_MAP,
    BAD_MAP_LATENT,
    BAD_RECORD,
    BAD_RECORD_LATENT,
    BAD_RETURN,
    BAD_RETURN_LATENT,
    BIABDUCTION_MEMORY_LEAK,
    BIABDUCTION_RETAIN_CYCLE,
    BLOCK_PARAMETER_NOT_NULL_CHECKED,
    BUFFER_OVERRUN_L1,
    BUFFER_OVERRUN_L2,
    BUFFER_OVERRUN_L3,
    BUFFER_OVERRUN_L4,
    BUFFER_OVERRUN_L5,
    BUFFER_OVERRUN_S2,
    BUFFER_OVERRUN_U5,
    CAPTURED_STRONG_SELF,
    CHECKERS_ALLOCATES_MEMORY,
    CHECKERS_ANNOTATION_REACHABILITY_ERROR,
    CHECKERS_CALLS_EXPENSIVE_METHOD,
    CHECKERS_EXPENSIVE_OVERRIDES_UNANNOTATED,
    CHECKERS_FRAGMENT_RETAINS_VIEW,
    CHECKERS_PRINTF_ARGS,
    CONFIG_IMPACT,
    CONFIG_IMPACT_STRICT,
    CONFIG_USAGE,
    CONSTANT_ADDRESS_DEREFERENCE,
    CONSTANT_ADDRESS_DEREFERENCE_LATENT,
    CREATE_INTENT_FROM_URI,
    CROSS_SITE_SCRIPTING,
    CXX_REF_CAPTURED_IN_BLOCK,
    DANGLING_POINTER_DEREFERENCE,
    DATALOG_FACT,
    DATA_FLOW_TO_SINK,
    DEADLOCK,
    DEAD_STORE,
    DIVIDE_BY_ZERO,
    EMPTY_VECTOR_ACCESS,
    EXECUTION_TIME_COMPLEXITY_INCREASE,
    EXECUTION_TIME_COMPLEXITY_INCREASE_UI_THREAD,
    EXECUTION_TIME_UNREACHABLE_AT_EXIT,
    EXPENSIVE_EXECUTION_TIME,
    EXPENSIVE_LOOP_INVARIANT_CALL,
    EXPOSED_INSECURE_INTENT_HANDLING,
    GUARDEDBY_VIOLATION,
    IMPURE_FUNCTION,
    INEFFICIENT_KEYSET_ITERATOR,
    INFERBO_ALLOC_IS_BIG,
    INFERBO_ALLOC_IS_NEGATIVE,
    INFERBO_ALLOC_IS_ZERO,
    INFERBO_ALLOC_MAY_BE_BIG,
    INFERBO_ALLOC_MAY_BE_NEGATIVE,
    INFINITE_EXECUTION_TIME,
    INSECURE_INTENT_HANDLING,
    INTEGER_OVERFLOW_L1,
    INTEGER_OVERFLOW_L2,
    INTEGER_OVERFLOW_L5,
    INTEGER_OVERFLOW_U5,
    INTERFACE_NOT_THREAD_SAFE,
    INVALID_SIL,
    INVARIANT_CALL,
    IPC_ON_UI_THREAD,
    JAVASCRIPT_INJECTION,
    LAB_RESOURCE_LEAK,
    LOCKLESS_VIOLATION,
    LOCK_CONSISTENCY_VIOLATION,
    LOGGING_PRIVATE_DATA,
    MEMORY_LEAK_C,
    MEMORY_LEAK_CPP,
    MISSING_REQUIRED_PROP,
    MIXED_SELF_WEAKSELF,
    MODIFIES_IMMUTABLE,
    MULTIPLE_WEAKSELF,
    MUTUAL_RECURSION_CYCLE,
    NIL_BLOCK_CALL,
    NIL_BLOCK_CALL_LATENT,
    NIL_INSERTION_INTO_COLLECTION,
    NIL_INSERTION_INTO_COLLECTION_LATENT,
    NIL_MESSAGING_TO_NON_POD,
    NIL_MESSAGING_TO_NON_POD_LATENT,
    NO_MATCHING_BRANCH_IN_TRY,
    NO_MATCHING_BRANCH_IN_TRY_LATENT,
    NO_MATCHING_CASE_CLAUSE,
    NO_MATCHING_CASE_CLAUSE_LATENT,
    NO_MATCHING_ELSE_CLAUSE,
    NO_MATCHING_ELSE_CLAUSE_LATENT,
    NO_MATCHING_FUNCTION_CLAUSE,
    NO_MATCHING_FUNCTION_CLAUSE_LATENT,
    NO_MATCH_OF_RHS,
    NO_MATCH_OF_RHS_LATENT,
    NO_TRUE_BRANCH_IN_IF,
    NO_TRUE_BRANCH_IN_IF_LATENT,
    NULLPTR_DEREFERENCE,
    NULLPTR_DEREFERENCE_IN_NULLSAFE_CLASS,
    NULLPTR_DEREFERENCE_IN_NULLSAFE_CLASS_LATENT,
    NULLPTR_DEREFERENCE_LATENT,
    NULL_ARGUMENT,
    NULL_ARGUMENT_LATENT,
    NULL_DEREFERENCE,
    OPTIONAL_EMPTY_ACCESS,
    OPTIONAL_EMPTY_ACCESS_LATENT,
    PREMATURE_NIL_TERMINATION_ARGUMENT,
    PULSE_CANNOT_INSTANTIATE_ABSTRACT_CLASS,
    PULSE_CONST_REFABLE,
    PULSE_DICT_MISSING_KEY,
    PULSE_DYNAMIC_TYPE_MISMATCH,
    PULSE_READONLY_SHARED_PTR_PARAM,
    PULSE_REFERENCE_STABILITY,
    PULSE_RESOURCE_LEAK,
    PULSE_TRANSITIVE_ACCESS,
    PULSE_UNAWAITED_AWAITABLE,
    PULSE_UNINITIALIZED_CONST,
    PULSE_UNINITIALIZED_VALUE,
    PULSE_UNNECESSARY_COPY,
    PULSE_UNNECESSARY_COPY_ASSIGNMENT,
    PULSE_UNNECESSARY_COPY_ASSIGNMENT_CONST,
    PULSE_UNNECESSARY_COPY_ASSIGNMENT_MOVABLE,
    PULSE_UNNECESSARY_COPY_INTERMEDIATE,
    PULSE_UNNECESSARY_COPY_INTERMEDIATE_CONST,
    PULSE_UNNECESSARY_COPY_MOVABLE,
    PULSE_UNNECESSARY_COPY_OPTIONAL,
    PULSE_UNNECESSARY_COPY_OPTIONAL_CONST,
    PULSE_UNNECESSARY_COPY_RETURN,
    PURE_FUNCTION,
    QUANDARY_TAINT_ERROR,
    REGEX_OP_ON_UI_THREAD,
    RESOURCE_LEAK,
    RETAIN_CYCLE,
    RETAIN_CYCLE_NO_WEAK_INFO,
    SCOPE_LEAKAGE,
    SENSITIVE_DATA_FLOW,
    SHELL_INJECTION,
    SHELL_INJECTION_RISK,
    SQL_INJECTION,
    SQL_INJECTION_RISK,
    STACK_VARIABLE_ADDRESS_ESCAPE,
    STARVATION,
    STATIC_INITIALIZATION_ORDER_FIASCO,
    STRICT_MODE_VIOLATION,
    STRONG_SELF_NOT_CHECKED,
    TAINT_ERROR,
    THREAD_SAFETY_VIOLATION,
    TOPL_ERROR,
    TOPL_ERROR_LATENT,
    UNTRUSTED_BUFFER_ACCESS,
    UNTRUSTED_DESERIALIZATION,
    UNTRUSTED_DESERIALIZATION_RISK,
    UNTRUSTED_ENVIRONMENT_CHANGE_RISK,
    UNTRUSTED_FILE,
    UNTRUSTED_FILE_RISK,
    UNTRUSTED_HEAP_ALLOCATION,
    UNTRUSTED_INTENT_CREATION,
    UNTRUSTED_URL_RISK,
    UNTRUSTED_VARIABLE_LENGTH_ARRAY,
    USER_CONTROLLED_SQL_RISK,
    USE_AFTER_DELETE,
    USE_AFTER_DELETE_LATENT,
    USE_AFTER_FREE,
    USE_AFTER_FREE_LATENT,
    USE_AFTER_LIFETIME,
    USE_AFTER_LIFETIME_LATENT,
    VECTOR_INVALIDATION,
    VECTOR_INVALIDATION_LATENT,
    WEAK_SELF_IN_NO_ESCAPE_BLOCK,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Severity {
    Error,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct InferResult {
    bug_type: BugType,
    severity: Severity,
    line: usize,
    column: usize,
    file: PathBuf,
    qualifier: String,
}

#[derive(Deserialize, Serialize)]
pub struct InferResults(Vec<InferResult>);

impl std::fmt::Display for InferResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::ser::to_string_pretty(self).map_err(|_| std::fmt::Error)?
        )
    }
}

impl Representation for InferResults {
    fn name(&self) -> &'static str {
        "infer_results"
    }
}

pub struct InferStaticAnalyze;

impl Tool for InferStaticAnalyze {
    fn name(&self) -> &'static str {
        "infer_static_analyze"
    }

    fn run(
        self: Box<Self>,
        context: harvest_core::tools::RunContext,
        inputs: Vec<harvest_core::Id>,
    ) -> Result<Box<dyn harvest_core::Representation>, Box<dyn std::error::Error>> {
        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;

        let working_dir = harvest_core::fs::temp_working_dir()?;
        raw_source.materialize(working_dir.as_ref())?;

        let status = Command::new("infer")
            .current_dir(working_dir.as_ref())
            .arg("run")
            .arg("--")
            .arg("make")
            .spawn()?.wait()?;

        if !status.success() {
            return Err("Infer failed".into());
        }

        println!("Success!");

        let results: Vec<InferResult> = serde_json::de::from_reader(File::open(
            working_dir.as_ref().join("infer-out").join("report.json"),
        )?)?;

        println!("{:?}", results);

        Ok(Box::new(InferResults(results)))
    }
}

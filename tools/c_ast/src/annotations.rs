use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::TopLevelEntity;
use crate::utils::{function_name, is_header_file, is_static_function};

/// Annotation metadata attached to extracted top-level entities.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EntityAnnotations {
    pub public: bool,
}

/// Marks function definitions as public when:
/// - Their name matches a declaration in a header
/// - They are not static
/// Ultimately this a hueristic, however the MITLL benchmarks don't always link against their headers, so it is necessary for now.
pub fn annotate_visibility(app_functions: &mut [TopLevelEntity], app_func_sigs: &[TopLevelEntity]) {
    let mut declared_in_headers: HashSet<String> = HashSet::new();

    for decl in app_func_sigs {
        if !is_header_file(&decl.span.file) {
            continue;
        }

        if let Some(name) = function_name(decl) {
            declared_in_headers.insert(name.to_string());
        }
    }

    for def in app_functions {
        if is_static_function(def) {
            continue;
        }

        let Some(name) = function_name(def) else {
            continue;
        };

        if declared_in_headers.contains(name) {
            def.annotations.public = true;
        }
    }
}

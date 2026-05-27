use super::{FnSig, is_string_type};
use std::collections::HashMap;

/// Map from unqualified struct name → (compound-literal name, fields).
/// The compound-literal name is the typedef name when one exists (e.g. `Point`),
/// or `struct Tag` for a plain struct tag.
pub(super) type StructMap = HashMap<String, (String, Vec<(String, String)>)>;

pub(super) struct TestVector {
    pub(super) function: String,
    pub(super) args: Vec<String>,
}

/// Returns C expression strings representing interesting boundary values for
/// a C type. Returns an empty vec for unknown/unsupported types.
pub(super) fn boundary_values(ty: &str, structs: &StructMap) -> Vec<String> {
    boundary_values_inner(ty, structs, 0)
}

fn boundary_values_inner(ty: &str, structs: &StructMap, depth: usize) -> Vec<String> {
    if depth > 4 {
        return vec![];
    }
    let ty = ty.trim();

    // String types (must come before generic pointer check)
    if is_string_type(ty) {
        return vec!["NULL".to_string(), r#""""#.to_string(), r#""hello""#.to_string()];
    }

    // Pointer types
    if ty.ends_with('*') {
        let inner = ty[..ty.len() - 1].trim();
        let inner_base = inner.trim_start_matches("const").trim();

        if inner_base == "void" {
            return vec!["NULL".to_string()];
        }

        // Pointer to known struct
        let struct_key = inner_base.trim_start_matches("struct").trim();
        if let Some((compound_name, fields)) = structs.get(struct_key) {
            let lit = struct_compound_literal(compound_name, fields, structs, depth + 1);
            return vec![format!("&{lit}"), "NULL".to_string()];
        }

        // Pointer to known scalar — use compound literal to take address
        let inner_vals = boundary_values_inner(inner_base, structs, depth + 1);
        if !inner_vals.is_empty() {
            let neutral = &inner_vals[0];
            return vec![format!("&({inner_base}){{{neutral}}}"), "NULL".to_string()];
        }

        return vec!["NULL".to_string()];
    }

    // Struct types (by value)
    let struct_key = ty.trim_start_matches("struct").trim();
    if let Some((compound_name, fields)) =
        structs.get(struct_key).or_else(|| structs.get(ty))
    {
        return vec![struct_compound_literal(compound_name, fields, structs, depth + 1)];
    }

    // Primitive types
    match ty {
        "int" | "signed int" | "signed" => &["0", "1", "-1", "INT_MAX", "INT_MIN"] as &[&str],
        "unsigned int" | "unsigned" => &["0", "1", "UINT_MAX"],
        "long" | "signed long" | "long int" => &["0", "1", "-1", "LONG_MAX", "LONG_MIN"],
        "unsigned long" | "unsigned long int" => &["0", "1", "ULONG_MAX"],
        "long long" | "signed long long" | "long long int" => {
            &["0", "1", "-1", "LLONG_MAX", "LLONG_MIN"]
        }
        "unsigned long long" | "unsigned long long int" => &["0", "1", "ULLONG_MAX"],
        "short" | "signed short" | "short int" => &["0", "1", "-1", "SHRT_MAX", "SHRT_MIN"],
        "unsigned short" | "unsigned short int" => &["0", "1", "USHRT_MAX"],
        "char" => &["0", "'a'", "CHAR_MAX", "CHAR_MIN"],
        "signed char" => &["0", "1", "-1", "SCHAR_MAX", "SCHAR_MIN"],
        "unsigned char" => &["0", "'a'", "UCHAR_MAX"],
        "float" => &["0.0f", "1.0f", "-1.0f"],
        "double" => &["0.0", "1.0", "-1.0"],
        "long double" => &["0.0L", "1.0L", "-1.0L"],
        "size_t" => &["0", "1", "SIZE_MAX"],
        "ssize_t" | "ptrdiff_t" | "intptr_t" => &["0", "1", "-1"],
        "uintptr_t" => &["0", "1"],
        "bool" | "_Bool" => &["0", "1"],
        "int8_t" | "int_least8_t" | "int_fast8_t" => &["0", "1", "-1", "INT8_MAX", "INT8_MIN"],
        "int16_t" | "int_least16_t" | "int_fast16_t" => {
            &["0", "1", "-1", "INT16_MAX", "INT16_MIN"]
        }
        "int32_t" | "int_least32_t" | "int_fast32_t" => {
            &["0", "1", "-1", "INT32_MAX", "INT32_MIN"]
        }
        "int64_t" | "int_least64_t" | "int_fast64_t" => {
            &["0", "1", "-1", "INT64_MAX", "INT64_MIN"]
        }
        "uint8_t" | "uint_least8_t" | "uint_fast8_t" => &["0", "1", "UINT8_MAX"],
        "uint16_t" | "uint_least16_t" | "uint_fast16_t" => &["0", "1", "UINT16_MAX"],
        "uint32_t" | "uint_least32_t" | "uint_fast32_t" => &["0", "1", "UINT32_MAX"],
        "uint64_t" | "uint_least64_t" | "uint_fast64_t" => &["0", "1", "UINT64_MAX"],
        _ => return vec![],
    }
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Build a C99 compound literal for a struct, using the neutral (first) boundary
/// value for each field. Fields with unresolvable types are omitted (C zero-inits them).
fn struct_compound_literal(
    compound_name: &str,
    fields: &[(String, String)],
    structs: &StructMap,
    depth: usize,
) -> String {
    let inits: Vec<String> = fields
        .iter()
        .filter_map(|(fname, ftype)| {
            boundary_values_inner(ftype, structs, depth)
                .into_iter()
                .next()
                .map(|val| format!(".{fname} = {val}"))
        })
        .collect();
    if inits.is_empty() {
        format!("({compound_name}){{0}}")
    } else {
        format!("({compound_name}){{{}}}", inits.join(", "))
    }
}

/// Generate OVAT test vectors for all functions whose parameter types are fully resolvable.
pub(super) fn generate_test_vectors(
    sigs: &HashMap<String, FnSig>,
    structs: &StructMap,
) -> Vec<TestVector> {
    let mut fn_names: Vec<&String> = sigs.keys().collect();
    fn_names.sort();

    let mut tests = Vec::new();

    for fn_name in fn_names {
        let sig = &sigs[fn_name];

        if sig.param_types.is_empty() {
            tests.push(TestVector { function: fn_name.clone(), args: vec![] });
            continue;
        }

        let value_sets: Vec<Vec<String>> = sig
            .param_types
            .iter()
            .map(|ty| boundary_values(ty, structs))
            .collect();

        // Skip functions with unresolvable parameter types
        if value_sets.iter().any(|v| v.is_empty()) {
            continue;
        }

        let neutral: Vec<String> = value_sets.iter().map(|v| v[0].clone()).collect();

        // Baseline: all parameters at their neutral value
        tests.push(TestVector { function: fn_name.clone(), args: neutral.clone() });

        // OVAT: vary each parameter through its non-neutral values
        for (i, values) in value_sets.iter().enumerate() {
            for val in values.iter().skip(1) {
                let mut args = neutral.clone();
                args[i] = val.clone();
                tests.push(TestVector { function: fn_name.clone(), args });
            }
        }
    }

    tests
}

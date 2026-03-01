use crate::error::HarvestResult;
use harvest_core::fs::RawDir;
use harvest_core::HarvestIR;
use std::path::PathBuf;

use full_source::{CargoPackage, RawSource};
use try_cargo_build::CargoBuildResult;

/// Extract a single CargoPackage representation from the IR.
/// Returns an error if there are 0 or multiple CargoPackage representations.
#[allow(dead_code)]
pub fn raw_cargo_package(ir: &HarvestIR) -> HarvestResult<&RawDir> {
    let cargo_packages: Vec<&RawDir> = ir
        .get_by_representation::<CargoPackage>()
        .map(|(_, r)| &r.dir)
        .collect();

    match cargo_packages.len() {
        0 => Err("No CargoPackage representation found in IR".into()),
        1 => Ok(cargo_packages[0]),
        n => Err(format!(
            "Found {} CargoPackage representations, expected at most 1",
            n
        )
        .into()),
    }
}

/// Extract the most recently produced CargoPackage from the IR.
/// When multiple exist (e.g. `ModularTranslationLlm` followed by `ModularFixLlm`), returns the
/// one with the highest ID, which reflects the latest refinement pass.
/// Returns an error only if no CargoPackage is present at all.
pub fn latest_cargo_package(ir: &HarvestIR) -> HarvestResult<&RawDir> {
    // BTreeMap iterates in ascending ID order, so `.last()` is the most recently added entry.
    match ir.get_by_representation::<CargoPackage>().last() {
        None => Err("No CargoPackage representation found in IR".into()),
        Some((_, r)) => Ok(&r.dir),
    }
}

/// Extract a single RawSource representation from the IR.
/// Returns an error if there are 0 or multiple RawSource representations.
pub fn raw_source(ir: &HarvestIR) -> HarvestResult<&RawDir> {
    let raw_sources: Vec<&RawDir> = ir
        .get_by_representation::<RawSource>()
        .map(|(_, r)| &r.dir)
        .collect();

    match raw_sources.len() {
        0 => Err("No RawSource representation found in IR".into()),
        1 => Ok(raw_sources[0]),
        n => Err(format!("Found {} RawSource representations, expected at most 1", n).into()),
    }
}

/// Extract cargo build results from the IR.
/// Returns the build artifacts or an error if no results or multiple results are found.
pub fn cargo_build_result(ir: &HarvestIR) -> Result<Vec<PathBuf>, String> {
    let build_results: Vec<Result<Vec<PathBuf>, String>> = ir
        .get_by_representation::<CargoBuildResult>()
        .map(|(_, r)| r.result.clone())
        .collect();

    match build_results.len() {
        0 => Err("No artifacts built".into()),
        1 => build_results[0].clone(),
        n => Err(format!("Found {} build results, expected at most 1", n)),
    }
}

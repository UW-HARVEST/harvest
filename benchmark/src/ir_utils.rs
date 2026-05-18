use crate::error::HarvestResult;
use harvest_core::fs::RawDir;
use harvest_core::HarvestIR;

use full_source::{CargoPackage, RawSource};
use try_cargo_build::CargoBuildResult;
use write_output::WriteOutputResult;

/// Extract the final CargoPackage representation from the IR.
///
/// When multiple tools in a pipeline each produce a `CargoPackage` (e.g. `translate_agentic`
/// followed by `verify_fix_agentic`), the IR holds one per tool. Because `HarvestIR` is backed by
/// a `BTreeMap<Id, ...>` and `Id`s are monotonically increasing, the last entry is always the most
/// recently produced.
pub fn raw_cargo_package(ir: &HarvestIR) -> HarvestResult<&RawDir> {
    ir.get_by_representation::<CargoPackage>()
        .last()
        .map(|(_, r)| &r.dir)
        .ok_or_else(|| "No CargoPackage representation found in IR".into())
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

/// Extract the final CargoBuildResult from the IR.
///
/// The repair loop produces one `CargoBuildResult` per build attempt. Because `HarvestIR` is
/// backed by a `BTreeMap<Id, ...>` and `Id`s are monotonically increasing, the last entry is
/// always the most recently produced.
pub fn cargo_build_result(ir: &HarvestIR) -> Result<&CargoBuildResult, String> {
    ir.get_by_representation::<CargoBuildResult>()
        .last()
        .map(|(_, r)| r)
        .ok_or_else(|| "No CargoBuildResult found in IR".into())
}

/// Extract the WriteOutputResult from the IR.
pub fn write_output_result(ir: &HarvestIR) -> Result<&WriteOutputResult, String> {
    ir.get_by_representation::<WriteOutputResult>()
        .last()
        .map(|(_, r)| r)
        .ok_or_else(|| "No WriteOutputResult found in IR".into())
}

/// Extract all CargoPackage representations from the IR, in order.
/// Each repair pass produces a new CargoPackage, so this returns one entry per pass.
pub fn all_cargo_packages(ir: &HarvestIR) -> Vec<&RawDir> {
    ir.get_by_representation::<CargoPackage>()
        .map(|(_, r)| &r.dir)
        .collect()
}

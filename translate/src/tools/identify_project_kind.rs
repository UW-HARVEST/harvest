use std::fmt::Display;

use harvest_core::Representation;

use super::{MightWriteContext, MightWriteOutcome, RunContext, Tool, load_raw_source::RawSource};

pub enum ProjectKind {
    Library,
    Executable,
}

impl Display for ProjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectKind::Library => write!(f, "Library"),
            ProjectKind::Executable => write!(f, "Executable"),
        }
    }
}

impl Representation for ProjectKind {
    fn name(&self) -> &'static str {
        "KindAndName"
    }
}

pub struct IdentifyProjectKind;

impl Tool for IdentifyProjectKind {
    fn name(&self) -> &'static str {
        "identify_project_kind"
    }

    fn might_write(&mut self, context: MightWriteContext) -> MightWriteOutcome {
        // We need a raw_source to be available, but we won't write any existing IDs.
        match context.ir.get_by_representation::<RawSource>().next() {
            None => MightWriteOutcome::TryAgain,
            Some(_) => MightWriteOutcome::Runnable([].into()),
        }
    }

    fn run(self: Box<Self>, context: RunContext) -> Result<(), Box<dyn std::error::Error>> {
        for (_, repr) in context.ir_snapshot.get_by_representation::<RawSource>() {
            if let Ok(cmakelists) = repr.dir.get_file("CMakeLists.txt") {
                if String::from_utf8_lossy(cmakelists)
                    .lines()
                    .any(|line| line.starts_with("add_executable("))
                {
                    context
                        .ir_edit
                        .add_representation(Box::new(ProjectKind::Executable));
                } else if String::from_utf8_lossy(cmakelists)
                    .lines()
                    .any(|line| line.starts_with("add_library("))
                {
                    context
                        .ir_edit
                        .add_representation(Box::new(ProjectKind::Library));
                }
            }
        }
        Ok(())
    }
}

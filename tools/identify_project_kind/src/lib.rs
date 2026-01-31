use std::fmt::Display;

use harvest_core::Representation;

use full_source::RawSource;
use harvest_core::Id;
use harvest_core::tools::{RunContext, Tool};

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
        "kind_and_name"
    }
}

pub struct IdentifyProjectKind;

impl Tool for IdentifyProjectKind {
    fn name(&self) -> &'static str {
        "identify_project_kind"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Get RawSource representation (the first and only arg of identify_project_kind)
        let repr = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;
        if let Ok(cmakelists) = repr.dir.get_file("CMakeLists.txt") {
            if String::from_utf8_lossy(cmakelists)
                .lines()
                .any(|line| line.starts_with("add_executable("))
            {
                return Ok(Box::new(ProjectKind::Executable));
            } else if String::from_utf8_lossy(cmakelists)
                .lines()
                .any(|line| line.starts_with("add_library("))
            {
                return Ok(Box::new(ProjectKind::Library));
            }
        }

        Err("Could not identify project kind from CMakeLists.txt (or could not find it)".into())
    }
}

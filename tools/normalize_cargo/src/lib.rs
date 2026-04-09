//! Checks if a generated Rust project builds by materializing
//! it to a tempdir and running `cargo build --release`.
use full_source::CargoPackage;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use toml_edit::{DocumentMut, Item, Table, Value};

pub struct NormalizeCargo;

impl Tool for NormalizeCargo {
    fn name(&self) -> &'static str {
        "normalize_cargo"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Get cargo package representation (the first and only arg of try_cargo_build)
        let mut cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("No CargoPackage representation found in IR")?
            .clone();
        let raw_cargo = cargo_package.dir.get_file_mut("Cargo.toml")?;
        let mut cargo: DocumentMut = str::from_utf8(raw_cargo)?.parse()?;
        if cargo.get("workspace").is_none() {
            cargo.insert("workspace", Item::Table(Table::new()));
        }
        if let Some(pkg) = cargo.get_mut("package").and_then(|p| p.as_table_mut()) {
            pkg.insert("name", Item::Value(Value::from("driver")));
        }

        raw_cargo.clear();
        raw_cargo.append(&mut cargo.to_string().as_bytes().to_vec());

        Ok(Box::new(cargo_package))
    }
}

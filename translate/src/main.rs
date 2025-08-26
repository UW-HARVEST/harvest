mod cli;

use clap::Parser as _;
use cli::Args;
use harvest_ir::{HarvestIR, Id, Representation};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let harvest_ir = HarvestIR::from_raw_source(args.in_performer)?;
    println!("{harvest_ir}");
    if let Some(Representation::RawSource(rd)) = harvest_ir.get(&Id(0)) {
        let c2r_cast = harvest_ir::c2rust_c_ast::C2RustCAst::populate_from(rd);
        //println!("{:?}", c2r_cast);
        c2r_cast.unwrap().tree_crawl();
    } else {
        panic!("WTF?");
    }
    Ok(())
}

mod cli;

use clap::Parser as _;
use cli::Args;
use harvest_ir::HarvestIR;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let name = args.in_performer.to_string_lossy();
    let harvest_ir = HarvestIR::from_raw_source(&args.in_performer)?;
    if let Some(_repr) = harvest_ir::c_ast::CAst::run_stage(harvest_ir) {
        println!("{name} Done");
        let c2r_cast = harvest_ir::c2rust_c_ast::CAst::populate_from(rd);
        //println!("{:?}", c2r_cast);
        c2r_cast.unwrap().tree_crawl();
    } else {
        panic!("{name} Only reachable if the setup was wrong or casting code is incorrect");
    }
    Ok(())
}

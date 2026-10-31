use clap::Parser;
use harvest_translate::cli::{Args, initialize};
use harvest_translate::transpile;
use harvest_translate::util::set_user_only_umask;
use std::sync::Arc;

fn main() {
    if let Err(e) = run() {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    set_user_only_umask();
    let args: Arc<_> = Args::parse().into();
    let Some(config) = initialize(args) else {
        return Ok(()); // An early-exit argument was passed.
    };
    // Note: output-directory preparation happens inside `transpile`, which
    // knows whether this is a resume-verify run (and must therefore preserve
    // the existing `<output>/translated` crate rather than erase it).
    let ir = transpile(config.into())?;
    println!("{}", ir);
    Ok(())
}

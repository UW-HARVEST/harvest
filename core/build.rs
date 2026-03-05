use std::process::{Command, Output};

fn parse_output(output: Option<Output>) -> Option<String> {
    Some(String::from_utf8(output?.stdout).ok()?.trim().to_string())
}

fn git_sha() -> String {
    parse_output(
        Command::new("git")
            .args(["describe", "--always", "--abbrev=0", "--dirty=*"])
            .output()
            .ok(),
    )
    .unwrap_or("unknown".to_string())
}

fn git_date() -> String {
    parse_output(
        Command::new("git")
            .args([
                "show",
                "-s",
                "--format=%cd",
                "--date=format:%Y-%m-%d",
                "HEAD",
            ])
            .output()
            .ok(),
    )
    .unwrap_or("unknown".to_string())
}

fn main() {
    // Re-run if git HEAD changes (covers new commits, branch switches)
    println!("cargo:rerun-if-changed=.git/HEAD");
    // Re-run if the current ref changes
    println!("cargo:rerun-if-changed=.git/index");
    // Log git info as env vars for inclusion in the build
    println!("cargo:rustc-env=GIT_SHA={}", git_sha());
    println!("cargo:rustc-env=GIT_DATE={}", git_date());
}

// `soundpack.rs` resolves `packs_dir()` through this, and there is no
// `crate::config` here to reach it by.
#[path = "../data_dir.rs"]
mod data_dir;
#[path = "../soundpack.rs"]
mod soundpack;
// Only so the `t!` calls inside `soundpack.rs` resolve. `init` is never
// called here, so every message stays English — which is what a command-line
// tool's output should be.
#[allow(dead_code)]
#[path = "../i18n.rs"]
mod i18n;

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(project) = args.next() else {
        eprintln!("Usage: soundpack <project-directory> <output.pspack>");
        std::process::exit(2)
    };
    let Some(output) = args.next() else {
        eprintln!("Usage: soundpack <project-directory> <output.pspack>");
        std::process::exit(2)
    };
    if args.next().is_some() {
        eprintln!("Usage: soundpack <project-directory> <output.pspack>");
        std::process::exit(2)
    }
    match soundpack::compile_and_bump(&PathBuf::from(project), &PathBuf::from(output)) {
        Ok(revision) => println!("Compiled sound pack revision {revision}."),
        Err(error) => {
            eprintln!("soundpack: {error}");
            std::process::exit(1);
        }
    }
}

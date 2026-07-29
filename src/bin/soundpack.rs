#[path = "../soundpack.rs"]
mod soundpack;

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

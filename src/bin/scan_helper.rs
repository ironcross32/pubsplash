//! pubsplash-scan: loads a single VST plugin and prints its info as JSON.
//!
//! Runs as a separate process so a plugin that crashes or hangs while loading
//! cannot take Pubsplash down with it. Activation dialogs a plugin shows
//! during load appear normally (same desktop); Pubsplash waits.
//!
//! Usage: `pubsplash-scan --vst2 <dll>` or `pubsplash-scan --vst3 <binary>`.
//! Success: JSON on stdout, exit 0. Failure: reason on stderr, exit nonzero.

#[allow(dead_code)]
#[path = "../vst/types.rs"]
mod types;
#[path = "../vst/vst2.rs"]
mod vst2;
#[path = "../vst/vst3.rs"]
mod vst3;

use std::io::Write;
use std::path::Path;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
};
use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

/// Flushes stdio and kills this process without running DLL_PROCESS_DETACH.
/// A normal exit notifies every loaded DLL, and plugins routinely crash in
/// that teardown path (static destructors, orphaned worker threads) — which
/// used to turn a fully successful probe into exit code 0xc0000005.
fn exit_hard(code: u32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), code);
    }
    unreachable!("TerminateProcess on the current process does not return");
}

fn main() {
    // Keep Windows from popping error boxes when a plugin crashes this
    // process; the parent reports the failure instead.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        // Some plugins use COM while loading and fault on an uninitialized
        // apartment.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let args: Vec<String> = std::env::args().collect();
    let (flag, path) = match (args.get(1), args.get(2)) {
        (Some(flag), Some(path)) => (flag.as_str(), Path::new(path)),
        _ => {
            eprintln!("usage: pubsplash-scan --vst2 <dll> | --vst3 <binary>");
            std::process::exit(2);
        }
    };
    let result = match flag {
        "--vst2" => vst2::scan(path),
        "--vst3" => vst3::scan(path),
        _ => {
            eprintln!("unknown flag {flag}");
            std::process::exit(2);
        }
    };
    match result {
        Ok(out) => {
            println!("{}", serde_json::to_string(&out).expect("serializable"));
            exit_hard(0);
        }
        Err(reason) => {
            eprintln!("{reason}");
            exit_hard(1);
        }
    }
}

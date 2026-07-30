//! Turns a hard crash into something readable.
//!
//! Pubsplash hosts third-party VST2 and VST3 code in-process, and when that
//! code faults the process dies with `STATUS_ACCESS_VIOLATION` (0xC0000005) —
//! a *hardware* fault, not a Rust panic. There is no unwind, no panic hook, no
//! message and no backtrace, so `logging::install_panic_hook` never sees it and
//! the log file simply stops mid-sentence. That is exactly the situation where
//! the one fact worth having is *whose code faulted*.
//!
//! So this installs a top-level exception filter that writes, before the
//! process goes away:
//!
//! - the exception code and the faulting address;
//! - the **module that address belongs to**, and the offset within it — which
//!   names the plugin DLL, `vst3-host`, or us, and is what turns "it crashes
//!   when I delete an effect" into a fixable bug;
//! - for an access violation, whether it was a read or a write and of what;
//! - a minidump next to the logs, for anyone who wants to open it in a debugger.
//!
//! The filter runs on the faulting thread with a possibly-corrupt heap, so it
//! does as little as it can get away with and guards against re-entering itself.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_STACK_OVERFLOW,
    GENERIC_WRITE, HANDLE, HMODULE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
};
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_EXECUTE_HANDLER, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
    MiniDumpWithIndirectlyReferencedMemory, MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::core::PCWSTR;

/// Set for the whole life of the handler so a fault *inside* it (a corrupt
/// heap makes that entirely possible) does not recurse forever.
static HANDLING: AtomicBool = AtomicBool::new(false);

/// Installs the top-level exception filter. Call once, as early as possible
/// after logging is up.
pub fn install() {
    unsafe {
        SetUnhandledExceptionFilter(Some(handler));
    }
}

unsafe extern "system" fn handler(info: *const EXCEPTION_POINTERS) -> i32 {
    if HANDLING.swap(true, Ordering::SeqCst) {
        return EXCEPTION_EXECUTE_HANDLER;
    }
    report(info);
    // Written after the report, so a failure to produce the dump can never cost
    // us the one line that actually names the culprit.
    let dump = write_minidump(info);

    log::error!("Pubsplash is terminating because of the fault above");
    if let Some(path) = dump {
        log::error!("Crash dump written to {}", path.display());
    }
    // Buffered on a background writer that will not get another chance to run.
    log::logger().flush();
    EXCEPTION_EXECUTE_HANDLER
}

/// Logs what happened and, crucially, in whose code.
fn report(info: *const EXCEPTION_POINTERS) {
    let record = unsafe { info.as_ref() }.and_then(|i| unsafe { i.ExceptionRecord.as_ref() });
    let Some(record) = record else {
        log::error!("Unhandled exception with no exception record");
        return;
    };

    let code = record.ExceptionCode.0 as u32;
    let address = record.ExceptionAddress as usize;
    let (module, offset) = module_for(address);

    log::error!(
        "FATAL: unhandled exception {code:#010x} ({}) at {address:#018x} on thread {} \
         — in {module}+{offset:#x}",
        exception_name(code),
        unsafe { GetCurrentThreadId() },
    );
    if let Some(detail) = access_violation_detail(code, record.NumberParameters, unsafe {
        std::ptr::addr_of!(record.ExceptionInformation).read_unaligned()
    }) {
        log::error!("FATAL: {detail}");
    }
}

/// The friendly name for the codes we are realistically going to see.
fn exception_name(code: u32) -> &'static str {
    match code {
        c if c == EXCEPTION_ACCESS_VIOLATION.0 as u32 => "access violation",
        c if c == EXCEPTION_STACK_OVERFLOW.0 as u32 => "stack overflow",
        c if c == EXCEPTION_ILLEGAL_INSTRUCTION.0 as u32 => "illegal instruction",
        0xC000_0374 => "heap corruption",
        0xC000_0409 => "stack buffer overrun",
        0xE06D_7363 => "C++ exception",
        _ => "unknown",
    }
}

/// An access violation carries what it was doing and to what address, which
/// separates "called through a freed vtable" from "wrote past a buffer".
fn access_violation_detail(code: u32, count: u32, params: [usize; 15]) -> Option<String> {
    if code != EXCEPTION_ACCESS_VIOLATION.0 as u32 || count < 2 {
        return None;
    }
    let what = match params[0] {
        0 => "read from",
        1 => "wrote to",
        8 => "executed",
        _ => "touched",
    };
    let target = params[1];
    let (module, offset) = module_for(target);
    Some(format!(
        "the faulting instruction {what} {target:#018x} ({module}+{offset:#x})"
    ))
}

/// The module owning `address`, and the offset into it. This is the whole point
/// of the handler: it is the difference between "Pubsplash crashed" and
/// "WaveShell1-VST3 12.7_x64.vst3 crashed".
fn module_for(address: usize) -> (String, usize) {
    if address == 0 {
        return ("<null>".to_string(), 0);
    }
    let mut module = HMODULE::default();
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(address as *const u16),
            &mut module,
        )
    };
    if found.is_err() || module.is_invalid() {
        // No mapped module owns it — the classic shape of a call through a
        // pointer into a DLL that has just been unloaded.
        return ("<no mapped module>".to_string(), address);
    }
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    let name = if len == 0 {
        format!("{:#x}", module.0 as usize)
    } else {
        String::from_utf16_lossy(&buf[..len])
    };
    (name, address.wrapping_sub(module.0 as usize))
}

/// Writes a minidump beside the log files. Best effort — a failure here is
/// logged and otherwise ignored.
fn write_minidump(info: *const EXCEPTION_POINTERS) -> Option<PathBuf> {
    let dir = crate::config::config_dir().join("crashes");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("Could not create the crash dump directory: {e}");
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("pubsplash-{stamp}.dmp"));

    let wide: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().chain([0]).collect()
    };
    let file = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    let file: HANDLE = match file {
        Ok(handle) if handle != INVALID_HANDLE_VALUE => handle,
        _ => {
            log::error!("Could not create {}", path.display());
            return None;
        }
    };

    let mut exception = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: unsafe { GetCurrentThreadId() },
        ExceptionPointers: info as *mut EXCEPTION_POINTERS,
        ClientPointers: false.into(),
    };
    let result = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file,
            MiniDumpWithIndirectlyReferencedMemory,
            Some(&mut exception as *const _),
            None,
            None,
        )
    };
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(file);
    }
    match result {
        Ok(()) => Some(path),
        Err(e) => {
            log::error!("Could not write the crash dump: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_inside_our_own_image_resolves_to_this_executable() {
        let (module, offset) = module_for(module_for as *const () as usize);
        assert!(
            module.to_ascii_lowercase().contains("pubsplash"),
            "expected our own image, got {module}"
        );
        assert!(offset > 0, "an offset into the image, not the base");
    }

    #[test]
    fn an_unmapped_address_is_reported_as_unmapped() {
        let (module, _) = module_for(1);
        assert_eq!(module, "<no mapped module>");
        assert_eq!(module_for(0).0, "<null>");
    }

    #[test]
    fn an_access_violation_says_what_it_was_doing() {
        let mut params = [0usize; 15];
        params[0] = 1;
        params[1] = 0xdead_beef;
        let detail =
            access_violation_detail(EXCEPTION_ACCESS_VIOLATION.0 as u32, 2, params).unwrap();
        assert!(detail.contains("wrote to"), "{detail}");
        assert!(detail.contains("deadbeef"), "{detail}");
        // Anything that is not an access violation carries no such parameters.
        assert!(access_violation_detail(0xC000_0374, 2, params).is_none());
    }
}

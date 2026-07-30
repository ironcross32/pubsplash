//! Keeps a plugin's DLL mapped for the life of the process.
//!
//! Removing a plugin from a chain destroys the instance, and destroying an
//! instance ends in `FreeLibrary` — in `Vst2Plugin::drop` for VST2, and inside
//! `vst3-host`'s `WindowsModule::drop` for VST3. When that call takes the
//! module's last reference the image is unmapped, and *anything* the plugin
//! left running dies with the process: its own worker threads, a `SetTimer`
//! callback, a COM apartment stub, a thread-pool item, an unregistered window
//! class's `WndProc`. The result is a call into unmapped memory —
//! `STATUS_ACCESS_VIOLATION` with a faulting address that belongs to no module
//! at all (which is exactly what `crash.rs` is built to report).
//!
//! Plenty of real plugins do this. Shell modules that host many effects behind
//! one DLL are the worst offenders, and there is nothing a host can do about it
//! from the outside — the plugin is asking to be unloaded and is not ready.
//!
//! So we take one extra, permanent reference per distinct plugin binary before
//! the first instance is created, and never release it. Every later
//! load/unload pair stays balanced against that floor, so the refcount can rise
//! and fall but never reaches zero and the image is never unmapped. The leak is
//! the point: one mapped image per plugin the user has actually used, for the
//! session.
//!
//! This does not stop a VST3 module's `ExitDll` from being called (that is
//! `vst3-host`'s business and it has no per-module refcount), only the unmap
//! that follows it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows::core::{PCWSTR, s};

/// Binaries already pinned, so repeated instantiation of the same plugin is one
/// extra reference rather than one per instance.
static PINNED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Binaries whose `IPluginFactory` is pinned. Separate from [`PINNED`] because
/// the two happen at different moments: the image before the plugin loads, the
/// factory only once it has.
static PINNED_FACTORIES: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// `GetPluginFactory`, the one required export of a VST3 module.
type GetPluginFactory = unsafe extern "system" fn() -> *mut std::ffi::c_void;

/// Records `binary` in `set`, returning whether this is the first time.
fn first_time(set: &Mutex<Option<HashSet<PathBuf>>>, binary: &Path) -> bool {
    let mut guard = set.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashSet::new)
        .insert(binary.to_path_buf())
}

/// Resolves `path` to the DLL that will actually be loaded and takes a
/// permanent reference to it, returning the resolved binary. Idempotent per
/// binary — the second instance of a plugin adds no further references.
///
/// Best effort, and deliberately infallible from the caller's point of view: if
/// the pin does not take, we are simply back to the old behaviour, which is
/// worth a warning but is no reason to refuse to load the plugin. A failure is
/// not retried, because the next attempt would fail the same way and say so
/// again on every instantiation.
pub fn pin(path: &str) -> PathBuf {
    let binary = resolve(Path::new(path));

    if !first_time(&PINNED, &binary) {
        return binary;
    }

    let wide: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        binary.as_os_str().encode_wide().chain([0]).collect()
    };
    // SAFETY: `wide` is a NUL-terminated path that outlives the call. The
    // returned handle is deliberately dropped without `FreeLibrary` — that is
    // the entire mechanism.
    match unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) } {
        Ok(_) => log::debug!("Pinned plugin module {}", binary.display()),
        Err(e) => log::warn!(
            "Could not pin the plugin module {} ({e}); it may be unmapped when the plugin is \
             removed",
            binary.display()
        ),
    }
    binary
}

/// Whether `path`'s binary has been pinned. Test and diagnostic use only.
#[cfg(test)]
fn is_pinned(binary: &Path) -> bool {
    PINNED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|seen| seen.contains(binary))
}

/// Takes a permanent `IPluginFactory` reference on an already-loaded VST3
/// module, so the factory outlives every instance created from it.
///
/// `vst3-host` treats the factory as a local: it gets one in `PluginImpl::load`,
/// creates the component, and releases it before `load` returns. For an
/// ordinary plugin that is harmless. For a **shell** — one module publishing
/// many plugins, of which Waves' `WaveShell1-VST3` is the canonical example —
/// it is not: the factory is the object that owns the shell's per-process
/// registry (WaveShell drags in `WavesLicenseEngine.dll` and
/// `InnerProcessDictionary_x64.dll`, and loads the selected plugin's own DLL
/// underneath itself). Dropping the last factory reference while a component
/// created from it is still alive leaves that registry torn down, and the
/// component's own `terminate()` then walks it and dereferences `-1`. Every
/// real host keeps one module and one factory per DLL for the process; this is
/// the smallest way to get the same guarantee without owning the loader.
///
/// Call **after** a successful load: the module has to be mapped and its
/// optional `InitDll` already run, both of which the crate does.
pub fn pin_factory(path: &str) {
    let binary = resolve(Path::new(path));
    if !first_time(&PINNED_FACTORIES, &binary) {
        return;
    }
    let wide: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        binary.as_os_str().encode_wide().chain([0]).collect()
    };
    // SAFETY: the module is already loaded (the caller just instantiated a
    // plugin from it), so this only looks it up; `GetPluginFactory` is the
    // module's required export and takes no arguments. The reference it returns
    // is deliberately never released — that is the whole point.
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR(wide.as_ptr())) else {
            log::warn!(
                "Could not find the loaded module for {} to pin its factory",
                binary.display()
            );
            return;
        };
        let Some(entry) = GetProcAddress(module, s!("GetPluginFactory")) else {
            log::warn!("{} exports no GetPluginFactory", binary.display());
            return;
        };
        let entry: GetPluginFactory = std::mem::transmute(entry);
        if entry().is_null() {
            log::warn!("GetPluginFactory returned null for {}", binary.display());
        } else {
            log::debug!("Pinned the plugin factory for {}", binary.display());
        }
    }
}

/// The file `LoadLibrary` will be given. A modern VST3 is a bundle *directory*
/// (`Foo.vst3/Contents/x86_64-win/Foo.vst3`), and pinning the directory would
/// pin nothing; `vst3-host` resolves it the same way before loading, so we
/// borrow its resolver rather than keeping a second copy of the layout rules.
/// A VST2 path is already the DLL.
fn resolve(path: &Path) -> PathBuf {
    if path.is_file() {
        return path.to_path_buf();
    }
    vst3_host::discovery::get_vst3_binary_path(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_dll_path_resolves_to_itself() {
        // Any real file will do; the rule under test is "a file is its own
        // binary", which is what every VST2 path hits.
        let exe = std::env::current_exe().expect("current exe");
        assert_eq!(resolve(&exe), exe);
    }

    #[test]
    fn pinning_is_idempotent_and_records_the_resolved_binary() {
        // A real, loadable module: pinned once, and the same binary comes back
        // however many times it is asked for.
        let exe = std::env::current_exe().expect("current exe");
        let path = exe.to_string_lossy().to_string();
        assert_eq!(pin(&path), exe);
        assert_eq!(pin(&path), exe);
        assert!(is_pinned(&exe));
    }

    #[test]
    fn a_path_that_cannot_be_loaded_is_not_retried() {
        // The failure path must not panic, must still report the resolved
        // binary, and must not ask Windows again on every instantiation.
        let missing = Path::new("Z:\\no\\such\\plugin.vst3");
        let resolved = pin(&missing.to_string_lossy());
        assert!(is_pinned(&resolved));
        assert_eq!(pin(&missing.to_string_lossy()), resolved);
    }
}

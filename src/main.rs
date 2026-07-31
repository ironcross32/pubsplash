#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod b64;
mod config;
mod crash;
mod fx;
mod json_store;
mod keybind;
mod logging;
mod net;
mod secret;
mod soundpack;
mod source_name;
mod state;
mod tts;
mod ui;
mod vst;

use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    // Logging must come up before config so config recovery can log.
    // Held (not leaked) for the lifetime of the app: log writes are buffered
    // and flushed on a background thread so the audio engine never blocks on
    // one, and dropping the handle at the end of `main` is what flushes the
    // tail of the file.
    let mut log_handle = logging::init("info");
    let config = config::load();
    if let Some(handle) = log_handle.as_mut() {
        handle.set_level(&config.logging.level);
    }
    logging::install_panic_hook();
    // The panic hook only catches Rust panics. Hosted plugins fault in C++,
    // which kills the process without unwinding, so it takes a Win32 exception
    // filter to leave any trace at all of who did it.
    crash::install();
    log::info!("Pubsplash {} starting", env!("CARGO_PKG_VERSION"));

    let plugin_cache = vst::load_cache();
    log::info!("Plugin cache: {} plugins known", plugin_cache.plugins.len());
    let chain_library = fx::load_library();

    let engine = audio::AudioEngine::start();
    let net = net::NetHandle::start();
    let speaker = tts::speaker::Speaker::start(engine.external_feeds.clone());
    tts::prewarm_voices();
    // Fired before the UI is built so the cue overlaps plugin instantiation and
    // window construction rather than trailing them. The chosen pack is loaded
    // on the same thread and before the cue, so the first sound the user hears
    // is already theirs; a pack that has been deleted or corrupted since it was
    // chosen logs and leaves the built-in one active rather than opening a
    // dialog over a window that does not exist yet.
    {
        let pack = config.sounds.pack.clone();
        let play_startup = config.sounds.play_startup;
        std::thread::Builder::new()
            .name("soundpack-load".into())
            .spawn(move || {
                if !pack.is_empty() {
                    match soundpack::load_installed(&pack) {
                        Ok(loaded) => soundpack::set_active(Some(loaded)),
                        Err(e) => log::warn!("Could not load the sound pack {pack}: {e}"),
                    }
                }
                if play_startup {
                    if let Err(e) =
                        audio::cue::play_sound_kind_blocking(soundpack::SoundKind::Startup)
                    {
                        log::warn!("Could not play the startup sound: {e}");
                    }
                }
            })
            .ok();
    }
    let _ = wxdragon::main(move |_| {
        let (apps_tx, apps_rx) = crossbeam_channel::unbounded();
        let (usage_tx, usage_rx) = crossbeam_channel::unbounded();
        let (tts_catalog_tx, tts_catalog_rx) = crossbeam_channel::unbounded();
        tts::catalog::start_refresh(config.speech.clone(), tts_catalog_tx);
        ui::run_when_ready(move || {
            while let Ok(refresh) = tts_catalog_rx.try_recv() {
                match refresh.result {
                    Ok(catalog) => {
                        tts::catalog::commit_engine(refresh.engine, catalog);
                    }
                    Err(error) => log::warn!(
                        "Could not refresh the {} TTS catalog: {error}",
                        tts::engines::display_name(refresh.engine)
                    ),
                }
            }
            false
        });
        let app = Rc::new(ui::App {
            config: RefCell::new(config.clone()),
            run: RefCell::new(ui::Runtime::default()),
            engine,
            net,
            speaker,
            widgets: RefCell::new(None),
            connect_ui: RefCell::new(None),
            plugins: RefCell::new(plugin_cache.clone()),
            scan: RefCell::new(None),
            fx: RefCell::new(ui::FxRuntime::default()),
            orphaned_plugins: RefCell::new(Vec::new()),
            chain_library: RefCell::new(chain_library.clone()),
            open_editors: RefCell::new(Vec::new()),
            shutting_down: std::cell::Cell::new(false),
            config_dirty: std::cell::Cell::new(false),
            pumping: std::cell::Cell::new(false),
            apps_tx,
            apps_rx,
            apps_pending: std::cell::Cell::new(false),
            usage_tx,
            usage_rx,
            pump_timer: RefCell::new(None),
            fast_timer: RefCell::new(None),
            shutdown_cue: RefCell::new(None),
        });
        ui::build(app);
    });

    log::info!("Pubsplash exiting");
    if let Some(handle) = &log_handle {
        handle.flush();
    }
    drop(log_handle);
}

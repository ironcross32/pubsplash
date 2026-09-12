#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod b64;
mod config;
mod crash;
mod data_dir;
mod fx;
mod i18n;
mod instance;
mod json_store;
mod keybind;
mod logging;
mod mastodon;
mod net;
mod schedule;
mod secret;
mod soundpack;
mod source_name;
mod state;
mod tts;
mod ui;
mod update;
mod vst;

use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    // First of everything, and above logging in particular: flexi_logger appends
    // to the live log file and rotates it by size, so a copy that is about to be
    // turned away must not open it and pull it out from under the copy that is
    // running. The cost is that a rejection leaves no log line, only the message
    // box. It also has to be well above `update::cleanup_leftovers` below, which
    // deletes the staging directory an in-flight `pubsplash-update.exe` is
    // running from. Bound to a name so the claim lives as long as `main` does.
    let _instance = match instance::acquire() {
        instance::Acquired::Held(guard) => guard,
        instance::Acquired::AlreadyRunning => instance::reject_and_exit(),
    };

    // Logging must come up before config so config recovery can log. The handle
    // lives in a process-global inside `logging` for the lifetime of the app,
    // since the Preferences window reaches it too; `logging::shutdown` at the
    // end of `main` is what flushes the tail of the file, because log writes
    // are buffered and written on a background thread so the audio engine never
    // blocks on one.
    logging::init("info");
    // Before the config is read, so a portable copy that was last run by a build
    // which kept its settings in %LOCALAPPDATA% finds them where it now looks.
    // After logging, so the one time it happens is visible in the log.
    if let Some(from) = data_dir::migrate_from_legacy() {
        log::info!(
            "Brought existing settings across from {} into {}",
            from.display(),
            data_dir::root().display()
        );
    }
    let config = config::load();
    // Before the first `t!` anywhere, which in practice means before any window
    // exists — but also before the sound pack and engine threads below, since a
    // failure in either reports itself through a translated string. Logging is
    // already up, so the language actually chosen is visible in the log; nothing
    // written to the log is itself translated.
    i18n::init(Some(config.interface.language.as_str()));
    // Before anything can play: the startup cue below is spawned from this
    // function, and a monitoring thread opens the device the moment a strip
    // asks for it. Both read this setting rather than being handed it.
    audio::render::set_output_device(config.audio.output_device_id.clone());
    logging::set_level(&config.logging.level);
    logging::install_panic_hook();
    // The panic hook only catches Rust panics. Hosted plugins fault in C++,
    // which kills the process without unwinding, so it takes a Win32 exception
    // filter to leave any trace at all of who did it.
    crash::install();
    log::info!("Pubsplash {} starting", env!("CARGO_PKG_VERSION"));
    // Clears the staging area, and with it the helper copy from a previous
    // update -- a running process cannot delete its own image, so that one
    // always outlives the update it applied.
    update::cleanup_leftovers();

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
                if play_startup
                    && let Err(e) =
                        audio::cue::play_sound_kind_blocking(soundpack::SoundKind::Startup)
                {
                    log::warn!("Could not play the startup sound: {e}");
                }
            })
            .ok();
    }
    let _ = wxdragon::main(move |_| {
        let (apps_tx, apps_rx) = crossbeam_channel::unbounded();
        let (usage_tx, usage_rx) = crossbeam_channel::unbounded();
        let (mastodon_tx, mastodon_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
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
            cues: Default::default(),
            open_editors: RefCell::new(Vec::new()),
            shutting_down: std::cell::Cell::new(false),
            config_dirty: std::cell::Cell::new(false),
            pumping: std::cell::Cell::new(false),
            scan_pumping: std::cell::Cell::new(false),
            apps_tx,
            apps_rx,
            apps_pending: std::cell::Cell::new(false),
            usage_tx,
            usage_rx,
            mastodon_tx,
            mastodon_rx,
            update_tx,
            update_rx,
            update_state: RefCell::new(Default::default()),
            pump_timer: RefCell::new(None),
            fast_timer: RefCell::new(None),
            shutdown_cue: RefCell::new(None),
        });
        ui::build(app);
    });

    log::info!("Pubsplash exiting");
    logging::shutdown();
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod fx;
mod logging;
mod net;
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
    let log_handle = logging::init("info");
    let config = config::load();
    if let Some(mut handle) = log_handle {
        handle.set_level(&config.logging.level);
        // Keep the handle alive for the lifetime of the app.
        std::mem::forget(handle);
    }
    logging::install_panic_hook();
    log::info!("Pubsplash {} starting", env!("CARGO_PKG_VERSION"));

    let plugin_cache = vst::load_cache();
    log::info!("Plugin cache: {} plugins known", plugin_cache.plugins.len());
    let chain_library = fx::load_library();

    let engine = audio::AudioEngine::start();
    let net = net::NetHandle::start();
    let speaker = tts::sapi::Speaker::start(engine.external_feeds.clone());
    tts::prewarm_voices();
    let _ = wxdragon::main(move |_| {
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
            chain_library: RefCell::new(chain_library.clone()),
            open_editors: RefCell::new(Vec::new()),
            shutting_down: std::cell::Cell::new(false),
            pump_timer: RefCell::new(None),
        });
        ui::build(app);
    });

    log::info!("Pubsplash exiting");
}

// Embeds the application icon (and version metadata) into the Windows
// executable so Explorer, the taskbar, and Alt+Tab show it. This is separate
// from cargo-packager's `icons`, which only covers the installer and shortcuts.
fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/pubsplash.ico");
        res.compile().expect("failed to embed Windows resources");
    }
}

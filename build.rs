// Embeds Windows resources so Explorer, the taskbar, Alt+Tab, and wxWidgets all
// see the executable metadata they expect. This is separate from cargo-packager's
// `icons`, which only covers the installer and shortcuts.
fn main() {
    validate_default_sound_pack();

    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/pubsplash.ico");
        res.set_manifest(COMMON_CONTROLS_V6_MANIFEST);
        res.compile().expect("failed to embed Windows resources");
    }
}

const COMMON_CONTROLS_V6_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="1.0.0.0" processorArchitecture="*" name="Pubsplash" type="win32"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn validate_default_sound_pack() {
    const PATH: &str = "assets/sounds/default/default.pspack";
    println!("cargo:rerun-if-changed={PATH}");

    let bytes = std::fs::read(PATH).expect("default sound pack must exist");
    assert!(
        bytes.len() >= 42,
        "default sound pack is too short to be a Pubsplash sound pack"
    );
    assert!(
        bytes.starts_with(b"PSSP"),
        "default sound pack does not have the Pubsplash sound-pack header"
    );
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    assert_eq!(
        version, 1,
        "default sound pack uses unsupported format version {version}"
    );
}

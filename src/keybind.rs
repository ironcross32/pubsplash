//! User-configurable keyboard shortcuts: the data model, the action catalogue,
//! and the pure formatting/validation rules behind them.
//!
//! Chords are stored as **Windows virtual-key codes**, not wx key codes. Both
//! ends of the feature live in the Win32 low-level keyboard hook — capture in the
//! Add-binding dialog and dispatch while the app runs (see [`crate::ui::keybinds`])
//! — so VK end to end avoids a translation table entirely, and is what lets the
//! capture box see CTRL+letter, ESC and the arrows at all: those never survive
//! wx's `IsDialogMessage` pre-processing.
//!
//! Everything here is pure and unit-tested. The UI half is `ui/keybinds_ui.rs`
//! and the runtime half is `ui/keybinds.rs`.

use serde::{Deserialize, Serialize};

use crate::config::Config;

// --- chords ----------------------------------------------------------------

/// One key combination. `vk == 0` means "no key", which is how an unassigned
/// binding is spelled while the Add-binding dialog is open; such a chord never
/// reaches the config (see [`KeybindsConfig::fix_up`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Chord {
    pub vk: u32,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    pub fn new(vk: u32, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self {
            vk,
            ctrl,
            alt,
            shift,
        }
    }

    /// A bare key with no modifiers, the common case for the defaults.
    pub fn plain(vk: u32) -> Self {
        Self::new(vk, false, false, false)
    }

    pub fn is_empty(&self) -> bool {
        self.vk == 0
    }

    pub fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// Whether this chord is a bare printable character — `K`, `7`, `;` — with
    /// no modifier held.
    ///
    /// Such a chord may not be made **global**: it would swallow that character
    /// in every application on the system, including the one the user is typing
    /// into. Function keys and anything with CTRL/ALT/SHIFT are fine.
    pub fn is_plain_character(&self) -> bool {
        !self.has_modifier() && produces_character(self.vk)
    }

    /// `"CTRL+SHIFT+F9"`. Modifier order is fixed so two equal chords always
    /// render the same string.
    pub fn label(&self) -> String {
        if self.is_empty() {
            return "None".to_string();
        }
        let mut out = String::new();
        for (held, name) in [
            (self.ctrl, "CTRL"),
            (self.alt, "ALT"),
            (self.shift, "SHIFT"),
        ] {
            if held {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&vk_name(self.vk));
        out
    }
}

/// Whether `vk` types a character, and so must not be grabbed system-wide on its
/// own. Deliberately generous: the OEM keys vary by layout, and treating an
/// unknown one as a character only costs the user a modifier.
fn produces_character(vk: u32) -> bool {
    matches!(vk,
        0x20                        // space
        | 0x30..=0x39               // 0-9
        | 0x41..=0x5A               // A-Z
        | 0x60..=0x6F               // numpad digits and operators
        | 0xBA..=0xC0               // OEM_1 .. OEM_3
        | 0xDB..=0xDF               // OEM_4 .. OEM_8
        | 0xE2                      // OEM_102
    )
}

/// The display name of a virtual-key code.
pub fn vk_name(vk: u32) -> String {
    if let 0x30..=0x39 | 0x41..=0x5A = vk {
        // Digits and letters are their own ASCII.
        return ((vk as u8) as char).to_string();
    }
    if (0x70..=0x87).contains(&vk) {
        // F1 .. F24
        return format!("F{}", vk - 0x6F);
    }
    if (0x60..=0x69).contains(&vk) {
        return format!("Numpad {}", vk - 0x60);
    }
    let name = match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0C => "Clear",
        0x0D => "Enter",
        0x13 => "Pause",
        0x14 => "Caps Lock",
        0x1B => "Escape",
        0x20 => "Space",
        0x21 => "Page Up",
        0x22 => "Page Down",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left Arrow",
        0x26 => "Up Arrow",
        0x27 => "Right Arrow",
        0x28 => "Down Arrow",
        0x2C => "Print Screen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x5D => "Applications",
        0x6A => "Numpad Multiply",
        0x6B => "Numpad Plus",
        0x6D => "Numpad Minus",
        0x6E => "Numpad Period",
        0x6F => "Numpad Divide",
        0x90 => "Num Lock",
        0x91 => "Scroll Lock",
        0xA6 => "Browser Back",
        0xA7 => "Browser Forward",
        0xAD => "Volume Mute",
        0xAE => "Volume Down",
        0xAF => "Volume Up",
        0xB0 => "Next Track",
        0xB1 => "Previous Track",
        0xB2 => "Stop Media",
        0xB3 => "Play/Pause",
        0xBA => "Semicolon",
        0xBB => "Equals",
        0xBC => "Comma",
        0xBD => "Minus",
        0xBE => "Period",
        0xBF => "Slash",
        0xC0 => "Backtick",
        0xDB => "Left Bracket",
        0xDC => "Backslash",
        0xDD => "Right Bracket",
        0xDE => "Quote",
        _ => return format!("Key {vk}"),
    };
    name.to_string()
}

// --- actions ---------------------------------------------------------------

/// What a binding does. Variants carrying a name resolve it at fire time, so a
/// binding survives the scene or bus being renamed away and back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum BindAction {
    ToggleStream,
    ToggleRecording,
    NextScene,
    PreviousScene,
    SwitchScene { scene: String },
    ToggleMonitorMaster,
    ToggleMonitorSource { source: String },
    ToggleMonitorBus { bus: String },
    ToggleMuteMaster,
    ToggleMuteSource { source: String },
    ToggleMuteBus { bus: String },
}

/// Which extra dropdown the Add-binding dialog must show for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Specifier {
    None,
    Scene,
    Source,
    Bus,
}

impl Specifier {
    /// The label on the specifier dropdown, and its accessible name.
    pub fn label(&self) -> &'static str {
        match self {
            Specifier::None => "",
            Specifier::Scene => "Scene",
            Specifier::Source => "Source",
            Specifier::Bus => "Bus",
        }
    }
}

/// The Add-binding dialog's first dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Streaming,
    Scenes,
    Monitoring,
    Muting,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Streaming,
        Category::Scenes,
        Category::Monitoring,
        Category::Muting,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Category::Streaming => "Streaming",
            Category::Scenes => "Scenes",
            Category::Monitoring => "Monitoring",
            Category::Muting => "Muting",
        }
    }

    /// The actions in this category, as templates: any name a variant carries is
    /// empty, to be filled from the specifier dropdown. This is what fills the
    /// dialog's second dropdown.
    pub fn actions(&self) -> Vec<BindAction> {
        match self {
            Category::Streaming => vec![BindAction::ToggleStream, BindAction::ToggleRecording],
            Category::Scenes => vec![
                BindAction::NextScene,
                BindAction::PreviousScene,
                BindAction::SwitchScene {
                    scene: String::new(),
                },
            ],
            Category::Monitoring => vec![
                BindAction::ToggleMonitorMaster,
                BindAction::ToggleMonitorSource {
                    source: String::new(),
                },
                BindAction::ToggleMonitorBus { bus: String::new() },
            ],
            Category::Muting => vec![
                BindAction::ToggleMuteMaster,
                BindAction::ToggleMuteSource {
                    source: String::new(),
                },
                BindAction::ToggleMuteBus { bus: String::new() },
            ],
        }
    }
}

impl BindAction {
    pub fn category(&self) -> Category {
        match self {
            BindAction::ToggleStream | BindAction::ToggleRecording => Category::Streaming,
            BindAction::NextScene | BindAction::PreviousScene | BindAction::SwitchScene { .. } => {
                Category::Scenes
            }
            BindAction::ToggleMonitorMaster
            | BindAction::ToggleMonitorSource { .. }
            | BindAction::ToggleMonitorBus { .. } => Category::Monitoring,
            BindAction::ToggleMuteMaster
            | BindAction::ToggleMuteSource { .. }
            | BindAction::ToggleMuteBus { .. } => Category::Muting,
        }
    }

    pub fn specifier_kind(&self) -> Specifier {
        match self {
            BindAction::SwitchScene { .. } => Specifier::Scene,
            BindAction::ToggleMonitorSource { .. } | BindAction::ToggleMuteSource { .. } => {
                Specifier::Source
            }
            BindAction::ToggleMonitorBus { .. } | BindAction::ToggleMuteBus { .. } => Specifier::Bus,
            _ => Specifier::None,
        }
    }

    /// The name this action's specifier currently holds, if it has one.
    pub fn specifier(&self) -> Option<&str> {
        match self {
            BindAction::SwitchScene { scene } => Some(scene),
            BindAction::ToggleMonitorSource { source } | BindAction::ToggleMuteSource { source } => {
                Some(source)
            }
            BindAction::ToggleMonitorBus { bus } | BindAction::ToggleMuteBus { bus } => Some(bus),
            _ => None,
        }
    }

    /// Returns this action with its specifier replaced. Actions that take none
    /// are returned unchanged, so the dialog can call this unconditionally.
    pub fn with_specifier(&self, name: &str) -> BindAction {
        let name = name.to_string();
        match self {
            BindAction::SwitchScene { .. } => BindAction::SwitchScene { scene: name },
            BindAction::ToggleMonitorSource { .. } => BindAction::ToggleMonitorSource {
                source: name,
            },
            BindAction::ToggleMuteSource { .. } => BindAction::ToggleMuteSource { source: name },
            BindAction::ToggleMonitorBus { .. } => BindAction::ToggleMonitorBus { bus: name },
            BindAction::ToggleMuteBus { .. } => BindAction::ToggleMuteBus { bus: name },
            other => other.clone(),
        }
    }

    /// The name shown in the second dropdown: no specifier filled in, since the
    /// specifier has a dropdown of its own right below it.
    pub fn template_label(&self) -> &'static str {
        match self {
            BindAction::ToggleStream => "Start or stop streaming",
            BindAction::ToggleRecording => "Start or stop recording",
            BindAction::NextScene => "Switch to the next scene",
            BindAction::PreviousScene => "Switch to the previous scene",
            BindAction::SwitchScene { .. } => "Switch to a specific scene",
            BindAction::ToggleMonitorMaster => "Monitor master",
            BindAction::ToggleMonitorSource { .. } => "Monitor a source",
            BindAction::ToggleMonitorBus { .. } => "Monitor a bus",
            BindAction::ToggleMuteMaster => "Mute or unmute master",
            BindAction::ToggleMuteSource { .. } => "Mute or unmute a source",
            BindAction::ToggleMuteBus { .. } => "Mute or unmute a bus",
        }
    }

    /// The name shown in the Keybinds list, with the specifier spelled out.
    pub fn label(&self) -> String {
        match self {
            BindAction::SwitchScene { scene } => format!("Switch to the scene {scene}"),
            BindAction::ToggleMonitorSource { source } => format!("Monitor {source}"),
            BindAction::ToggleMonitorBus { bus } => format!("Monitor the bus {bus}"),
            BindAction::ToggleMuteSource { source } => format!("Mute or unmute {source}"),
            BindAction::ToggleMuteBus { bus } => format!("Mute or unmute the bus {bus}"),
            other => other.template_label().to_string(),
        }
    }
}

// --- the catalogue ---------------------------------------------------------

/// The names a specifier dropdown can offer, and the raw material for the
/// Keybinds list. Collected from the config in one place so the list and the
/// dialog can never disagree about what exists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    pub scenes: Vec<String>,
    pub sources: Vec<String>,
    pub buses: Vec<String>,
}

impl Targets {
    /// `sources` is the deduped union of source identity names across **all**
    /// scenes, not just the active one.
    ///
    /// `SourceConfig.name` is an identity key ("Microphone 2"), unique only
    /// within its scene — the user-facing names come from `crate::source_name`.
    /// A source binding stores that identity and resolves it against the active
    /// scene when it fires; taking the union here is what makes a source that
    /// lives in some other scene bindable at all.
    pub fn from_config(config: &Config) -> Self {
        let mut sources: Vec<String> = Vec::new();
        for scene in &config.scenes.scenes {
            for source in &scene.sources {
                if !sources.iter().any(|s| s == &source.name) {
                    sources.push(source.name.clone());
                }
            }
        }
        Self {
            scenes: config.scenes.scenes.iter().map(|s| s.name.clone()).collect(),
            sources,
            buses: config.buses.buses.iter().map(|b| b.name.clone()).collect(),
        }
    }

    /// Whether the scene, source or bus an action names still exists. Always
    /// true for actions that name nothing.
    pub fn has(&self, action: &BindAction) -> bool {
        match action.specifier() {
            None => true,
            Some(name) => self
                .for_specifier(action.specifier_kind())
                .iter()
                .any(|n| n == name),
        }
    }

    pub fn for_specifier(&self, kind: Specifier) -> &[String] {
        match kind {
            Specifier::None => &[],
            Specifier::Scene => &self.scenes,
            Specifier::Source => &self.sources,
            Specifier::Bus => &self.buses,
        }
    }
}

/// Every action that can be bound, in the order the Keybinds list shows them.
///
/// The list is a catalogue, not a view of the config: an action with no chord
/// still gets a row, reading "Unassigned". Actions that take a specifier expand
/// to one row per target.
pub fn catalogue(targets: &Targets) -> Vec<BindAction> {
    let mut out = Vec::new();
    for category in Category::ALL {
        for template in category.actions() {
            match template.specifier_kind() {
                Specifier::None => out.push(template),
                kind => {
                    for name in targets.for_specifier(kind) {
                        out.push(template.with_specifier(name));
                    }
                }
            }
        }
    }
    out
}

// --- persisted config ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyBind {
    pub action: BindAction,
    pub key: Chord,
    /// Whether the chord is grabbed system-wide rather than only while Pubsplash
    /// is in the foreground.
    pub global: bool,
}

impl Default for KeyBind {
    fn default() -> Self {
        Self {
            action: BindAction::ToggleStream,
            key: Chord::default(),
            global: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindsConfig {
    pub binds: Vec<KeyBind>,
}

/// F9 and F10 out of the box. App-scoped rather than global: both are common
/// shortcuts elsewhere, and grabbing them system-wide by default would take them
/// away from every other application. Either can be made global in the tab.
impl Default for KeybindsConfig {
    fn default() -> Self {
        Self {
            binds: vec![
                KeyBind {
                    action: BindAction::ToggleStream,
                    key: Chord::plain(VK_F9),
                    global: false,
                },
                KeyBind {
                    action: BindAction::ToggleRecording,
                    key: Chord::plain(VK_F10),
                    global: false,
                },
            ],
        }
    }
}

pub const VK_F1: u32 = 0x70;
pub const VK_F6: u32 = 0x75;
pub const VK_F9: u32 = 0x78;
pub const VK_F10: u32 = 0x79;

impl KeybindsConfig {
    /// Post-load repair: drops keyless binds and keeps only the first bind on any
    /// given chord.
    ///
    /// Deliberately does **not** drop binds naming a scene, source or bus that is
    /// currently absent. Those names resolve when the binding fires, so dropping
    /// them here would silently lose a binding while a scene is renamed, a source
    /// is being moved between scenes, or a capture device is unplugged.
    pub fn fix_up(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.binds
            .retain(|b| !b.key.is_empty() && seen.insert(b.key));
    }

    pub fn find(&self, action: &BindAction) -> Option<&KeyBind> {
        self.binds.iter().find(|b| &b.action == action)
    }

    /// The action already using `chord`, ignoring `except` (the action being
    /// edited, which is allowed to keep its own chord).
    pub fn conflict(&self, chord: Chord, except: Option<&BindAction>) -> Option<&BindAction> {
        self.binds
            .iter()
            .find(|b| b.key == chord && Some(&b.action) != except)
            .map(|b| &b.action)
    }

    /// Assigns `chord` to `action`, replacing any chord that action already had.
    /// An empty chord unbinds it instead.
    pub fn set(&mut self, action: BindAction, chord: Chord, global: bool) {
        self.binds.retain(|b| b.action != action);
        if chord.is_empty() {
            return;
        }
        self.binds.push(KeyBind {
            action,
            key: chord,
            global,
        });
    }

    pub fn remove(&mut self, action: &BindAction) {
        self.binds.retain(|b| &b.action != action);
    }
}

/// The Keybinds list row for one catalogue entry.
pub fn row_label(action: &BindAction, bind: Option<&KeyBind>) -> String {
    match bind {
        None => format!("{}: Unassigned", action.label()),
        Some(bind) if bind.global => format!("{}: {}, global", action.label(), bind.key.label()),
        Some(bind) => format!("{}: {}", action.label(), bind.key.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_labels_put_modifiers_in_a_fixed_order() {
        assert_eq!(Chord::plain(VK_F9).label(), "F9");
        assert_eq!(Chord::new(b'M' as u32, true, false, false).label(), "CTRL+M");
        assert_eq!(
            Chord::new(VK_F9, true, true, true).label(),
            "CTRL+ALT+SHIFT+F9"
        );
        assert_eq!(Chord::new(0x2E, false, true, false).label(), "ALT+Delete");
        assert_eq!(Chord::default().label(), "None");
    }

    #[test]
    fn function_and_numpad_keys_are_named() {
        assert_eq!(vk_name(0x70), "F1");
        assert_eq!(vk_name(0x7B), "F12");
        assert_eq!(vk_name(0x62), "Numpad 2");
        assert_eq!(vk_name(0x26), "Up Arrow");
        assert_eq!(vk_name(b'7' as u32), "7");
    }

    // The rule the Add-binding dialog enforces: a bare character key may not be
    // grabbed system-wide.
    #[test]
    fn plain_characters_are_recognised_only_without_modifiers() {
        assert!(Chord::plain(b'K' as u32).is_plain_character());
        assert!(Chord::plain(b'7' as u32).is_plain_character());
        assert!(Chord::plain(0xBA).is_plain_character()); // semicolon
        assert!(!Chord::new(b'K' as u32, true, false, false).is_plain_character());
        assert!(!Chord::new(b'K' as u32, false, false, true).is_plain_character());
        assert!(!Chord::plain(VK_F9).is_plain_character());
        assert!(!Chord::plain(0x26).is_plain_character()); // up arrow
    }

    #[test]
    fn every_action_reports_the_specifier_its_variant_carries() {
        assert_eq!(BindAction::ToggleStream.specifier_kind(), Specifier::None);
        assert_eq!(
            BindAction::SwitchScene {
                scene: "Live".into()
            }
            .specifier_kind(),
            Specifier::Scene
        );
        assert_eq!(
            BindAction::ToggleMuteSource {
                source: "Microphone 1".into()
            }
            .specifier_kind(),
            Specifier::Source
        );
        assert_eq!(
            BindAction::ToggleMonitorBus { bus: "Music".into() }.specifier_kind(),
            Specifier::Bus
        );
    }

    #[test]
    fn with_specifier_leaves_specifier_free_actions_alone() {
        assert_eq!(
            BindAction::ToggleStream.with_specifier("Live"),
            BindAction::ToggleStream
        );
        assert_eq!(
            BindAction::SwitchScene {
                scene: String::new()
            }
            .with_specifier("Live"),
            BindAction::SwitchScene {
                scene: "Live".into()
            }
        );
    }

    fn targets() -> Targets {
        Targets {
            scenes: vec!["Default".into(), "Live".into()],
            sources: vec!["Microphone 1".into()],
            buses: vec!["Music".into()],
        }
    }

    #[test]
    fn the_catalogue_expands_one_row_per_target() {
        let all = catalogue(&targets());
        // Streaming 2, Scenes 2 + 2 scenes, Monitoring 1 + 1 source + 1 bus,
        // Muting the same 3.
        assert_eq!(all.len(), 2 + 2 + 2 + 3 + 3);
        assert!(all.contains(&BindAction::SwitchScene {
            scene: "Live".into()
        }));
        assert!(all.contains(&BindAction::ToggleMuteBus {
            bus: "Music".into()
        }));
        // No template with an empty specifier ever reaches the list.
        assert!(all.iter().all(|a| a.specifier() != Some("")));
    }

    #[test]
    fn a_catalogue_with_no_scenes_or_buses_still_offers_the_fixed_actions() {
        let all = catalogue(&Targets::default());
        assert_eq!(all.len(), 2 + 2 + 1 + 1);
        assert!(all.contains(&BindAction::ToggleMuteMaster));
    }

    #[test]
    fn fix_up_drops_keyless_binds_and_later_duplicates() {
        let mut config = KeybindsConfig {
            binds: vec![
                KeyBind {
                    action: BindAction::ToggleStream,
                    key: Chord::plain(VK_F9),
                    global: false,
                },
                KeyBind {
                    action: BindAction::ToggleRecording,
                    key: Chord::plain(VK_F9),
                    global: true,
                },
                KeyBind {
                    action: BindAction::NextScene,
                    key: Chord::default(),
                    global: false,
                },
            ],
        };
        config.fix_up();
        assert_eq!(config.binds.len(), 1);
        assert_eq!(config.binds[0].action, BindAction::ToggleStream);
    }

    // A bind naming something that is not there right now must survive: the name
    // resolves when the key is pressed.
    #[test]
    fn fix_up_keeps_binds_naming_an_absent_target() {
        let mut config = KeybindsConfig {
            binds: vec![KeyBind {
                action: BindAction::SwitchScene {
                    scene: "Deleted".into(),
                },
                key: Chord::plain(VK_F10),
                global: false,
            }],
        };
        config.fix_up();
        assert_eq!(config.binds.len(), 1);
    }

    #[test]
    fn conflict_ignores_the_action_being_edited() {
        let config = KeybindsConfig::default();
        assert_eq!(
            config.conflict(Chord::plain(VK_F9), None),
            Some(&BindAction::ToggleStream)
        );
        assert_eq!(
            config.conflict(Chord::plain(VK_F9), Some(&BindAction::ToggleStream)),
            None
        );
        assert_eq!(config.conflict(Chord::plain(0x41), None), None);
    }

    #[test]
    fn set_replaces_and_an_empty_chord_unbinds() {
        let mut config = KeybindsConfig::default();
        config.set(
            BindAction::ToggleStream,
            Chord::new(b'S' as u32, true, false, true),
            true,
        );
        assert_eq!(config.binds.len(), 2);
        let bind = config.find(&BindAction::ToggleStream).unwrap();
        assert!(bind.global);
        assert_eq!(bind.key.label(), "CTRL+SHIFT+S");

        config.set(BindAction::ToggleStream, Chord::default(), false);
        assert!(config.find(&BindAction::ToggleStream).is_none());
        assert_eq!(config.binds.len(), 1);
    }

    #[test]
    fn row_labels_read_the_binding_after_the_name() {
        let config = KeybindsConfig::default();
        assert_eq!(
            row_label(
                &BindAction::ToggleStream,
                config.find(&BindAction::ToggleStream)
            ),
            "Start or stop streaming: F9"
        );
        assert_eq!(
            row_label(&BindAction::ToggleMuteMaster, None),
            "Mute or unmute master: Unassigned"
        );
        let global = KeyBind {
            action: BindAction::NextScene,
            key: Chord::new(VK_F9, true, false, false),
            global: true,
        };
        assert_eq!(
            row_label(&BindAction::NextScene, Some(&global)),
            "Switch to the next scene: CTRL+F9, global"
        );
    }

    #[test]
    fn actions_round_trip_through_json() {
        let config = KeybindsConfig {
            binds: vec![KeyBind {
                action: BindAction::ToggleMuteSource {
                    source: "Microphone 1".into(),
                },
                key: Chord::new(b'M' as u32, true, false, true),
                global: true,
            }],
        };
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<KeybindsConfig>(&text).unwrap(), config);
    }
}

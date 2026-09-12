//! The "Audio" tab in Preferences: which device Pubsplash plays out of.
//!
//! One setting, but it reaches two independent output paths — the mixer's
//! monitoring tap (`audio::monitor`) and local sound cues (`audio::render`) —
//! and it is half of the rule that keeps Pubsplash's own audio out of a Desktop
//! Audio source. See `audio::device::effective_output_device_id` for the whole
//! of that rule; the short version is that a Desktop Audio source pinned to one
//! endpoint captures *everything* on it, so it may never be pinned to the
//! device chosen here.
//!
//! Saves as the user changes the control, like every other tab: the dialog has
//! no OK button, only Close.

use crate::t;
use std::rc::Rc;

use wxdragon::prelude::*;

use super::{App, show_info};
use crate::audio::device::DeviceInfo;
use crate::config::SourceKindConfig;

/// The row that means "whatever Windows currently calls the default".
fn follow_system() -> String {
    t!("Default output device (follow system)")
}

pub fn build_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let (device_group, device_box) = super::group_box(panel, &t!("Playback device"));

    fn output_device() -> String {
        t!("Output device")
    }
    let label = StaticText::builder(&device_box)
        .with_label(&output_device())
        .build();
    let choice = Choice::builder(&device_box).build();
    super::set_accessible_name(&choice, &output_device());
    super::help::tag(
        &choice,
        "dialog.preferences.audio.outputDevice",
        "Output device combo box",
    );

    let devices = crate::audio::device::render_devices();
    choice.append(&follow_system());
    for device in &devices {
        choice.append(&device.name);
    }
    // A device that has since been unplugged is not in the list, so a
    // configured id that no longer resolves falls back to showing the default
    // row. The setting itself is left alone: the device may come back, and
    // rewriting it here would quietly lose the user's choice for them.
    let preselect = app
        .config
        .borrow()
        .audio
        .output_device_id
        .as_deref()
        .and_then(|id| devices.iter().position(|d| d.id == id).map(|i| i + 1))
        .unwrap_or(0);
    choice.set_selection(preselect as u32);

    let explanation = StaticText::builder(&device_box)
        .with_label(
            &t!("Everything Pubsplash plays for you goes to this device: monitored sources, \
             text-to-speech, and sound cues. It is not what listeners hear."),
        )
        .build();

    device_group.add(&label, 0, SizerFlag::All, 4);
    device_group.add(&choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    device_group.add(&explanation, 0, SizerFlag::All, 4);

    {
        let app = app.clone();
        let devices = devices.clone();
        let dialog = *dialog;
        choice.on_selection_changed(move |_| {
            let Some(index) = choice.get_selection() else {
                return;
            };
            let device = (index as usize).checked_sub(1).and_then(|i| devices.get(i));
            app.config.borrow_mut().audio.output_device_id = device.map(|d| d.id.clone());
            app.save_config();
            app.apply_output_device();
            // Asked whichever row was picked, including the follow-system one:
            // that row resolves to a real endpoint too, and it is just as able
            // to be the one a Desktop Audio source is pinned to.
            warn_about_colliding_sources(&app, &dialog, &devices);
        });
    }

    let test = Button::builder(&device_box)
        .with_label(&t!("Play a test sound"))
        .build();
    super::set_accessible_name(&test, &t!("Play a test sound"));
    super::help::tag(
        &test,
        "dialog.preferences.audio.testSound",
        "Play a test sound button",
    );
    {
        test.on_click(move |_| {
            // On its own thread, and deliberately not awaited: `play_samples`
            // holds the device open for the length of the cue, and this is the
            // UI thread.
            std::thread::Builder::new()
                .name("output-device-test".into())
                .spawn(|| {
                    if let Err(e) = crate::audio::cue::play_sound_kind_blocking(
                        crate::soundpack::SoundKind::Startup,
                    ) {
                        log::warn!("Could not play the output device test sound: {e}");
                    }
                })
                .ok();
        });
    }
    device_group.add(&test, 0, SizerFlag::All, 4);

    sizer.add_sizer(&device_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
}

/// Names any Desktop Audio source that has just been pointed at Pubsplash's own
/// output, and says what will happen to it.
///
/// The dialog that pins a Desktop Audio source refuses this pairing outright,
/// but it can only refuse the half it is looking at: the *output* device can be
/// changed afterwards, which is exactly what has happened here. `capture::run`
/// catches it and falls back to capturing every endpoint with Pubsplash
/// excluded, so nothing leaks either way — but silently capturing more than the
/// user asked for is not something to leave in the log alone.
///
/// A modal is right here by the test in `CLAUDE.md`: it answers a deliberate
/// action the user just took and is sitting waiting on, rather than arriving
/// unbidden.
///
/// The question is asked of the *effective* device rather than of the row the
/// user picked, so that choosing "follow system" is covered: it names no
/// endpoint of its own, but it resolves to one, and that one collides just as
/// surely. `None` means the answer is unknown — say nothing rather than
/// guess, since `capture::run` treats unknown as a collision anyway and falls
/// back on its own.
fn warn_about_colliding_sources(app: &Rc<App>, dialog: &Dialog, devices: &[DeviceInfo]) {
    let Some(device_id) = crate::audio::device::effective_output_device_id() else {
        return;
    };
    let affected = colliding_sources(&app.config.borrow(), &device_id, devices);
    if affected.is_empty() {
        return;
    }
    let name = devices
        .iter()
        .find(|d| d.id == device_id)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| t!("The system default output device"));
    show_info(
        dialog,
        &t!("Output device"),
        &format!(
            "{name} is now Pubsplash's output device, and these Desktop Audio sources capture \
             it:\n\n{}\n\nCapturing it would feed Pubsplash's own audio back into the stream, \
             so they will capture every output device instead, with Pubsplash excluded. Edit \
             them to pick a different device.",
            affected.join("\n")
        ),
    );
}

/// The user-visible names of every Desktop Audio source pinned to `device_id`,
/// across every scene — a source in a scene that is not active collides just as
/// surely the moment that scene is switched to.
///
/// `render_devices` is what makes the labels readable: a pinned Desktop Audio
/// source names its endpoint, and a `NameContext` without the render devices
/// resolves every one of them to "Desktop Audio (unavailable)" — for the very
/// device the user has just chosen from a list of the ones that do exist.
fn colliding_sources(
    config: &crate::config::Config,
    device_id: &str,
    render_devices: &[DeviceInfo],
) -> Vec<String> {
    let context = crate::source_name::NameContext {
        render_devices: render_devices.to_vec(),
        ..Default::default()
    };
    let mut names = Vec::new();
    for scene in &config.scenes.scenes {
        let labels = crate::source_name::list_labels(&scene.sources, &context);
        for (source, label) in scene.sources.iter().zip(labels) {
            if matches!(
                &source.kind,
                SourceKindConfig::DesktopAudio { device_id: Some(id) } if id == device_id
            ) {
                names.push(t!(
                "{source} ({scene})",
                source = label,
                scene = scene.name
            ));
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SceneConfig, SourceConfig};

    fn scene(name: &str, sources: Vec<SourceConfig>) -> SceneConfig {
        SceneConfig {
            name: name.into(),
            is_default: false,
            sources,
        }
    }

    fn desktop(name: &str, device_id: Option<&str>) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            kind: SourceKindConfig::DesktopAudio {
                device_id: device_id.map(str::to_string),
            },
            ..Default::default()
        }
    }

    fn speakers() -> Vec<DeviceInfo> {
        vec![DeviceInfo {
            id: "{a}".into(),
            name: "Speakers (Realtek)".into(),
        }]
    }

    #[test]
    fn a_source_pinned_to_the_new_output_device_is_reported() {
        let mut config = Config::default();
        config.scenes.scenes = vec![scene("Live", vec![desktop("Desktop Audio", Some("{a}"))])];

        let names = colliding_sources(&config, "{a}", &speakers());

        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].contains("Live"), "{names:?}");
    }

    /// The label has to name the endpoint the user just picked. Without the
    /// render devices the source resolves to "(unavailable)", which is both
    /// wrong and unhelpful for finding the source again.
    #[test]
    fn the_reported_label_names_the_device() {
        let mut config = Config::default();
        config.scenes.scenes = vec![scene("Live", vec![desktop("Desktop Audio", Some("{a}"))])];

        let names = colliding_sources(&config, "{a}", &speakers());

        assert!(names[0].contains("Speakers (Realtek)"), "{names:?}");
        assert!(!names[0].contains("unavailable"), "{names:?}");
    }

    /// The all-endpoints form excludes Pubsplash by construction, so it never
    /// collides however the output device is set.
    #[test]
    fn an_unpinned_source_never_collides() {
        let mut config = Config::default();
        config.scenes.scenes = vec![scene("Live", vec![desktop("Desktop Audio", None)])];

        assert!(colliding_sources(&config, "{a}", &speakers()).is_empty());
    }

    #[test]
    fn a_source_on_another_device_does_not_collide() {
        let mut config = Config::default();
        config.scenes.scenes = vec![scene("Live", vec![desktop("Desktop Audio", Some("{b}"))])];

        assert!(colliding_sources(&config, "{a}", &speakers()).is_empty());
    }

    /// A scene that is not active still matters: switching to it is one arrow
    /// key away, and the collision would be waiting.
    #[test]
    fn every_scene_is_searched_not_just_the_active_one() {
        let mut config = Config::default();
        config.scenes.scenes = vec![
            scene("Live", vec![desktop("Desktop Audio", None)]),
            scene("Music", vec![desktop("Desktop Audio 2", Some("{a}"))]),
        ];

        let names = colliding_sources(&config, "{a}", &speakers());

        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].contains("Music"), "{names:?}");
    }
}

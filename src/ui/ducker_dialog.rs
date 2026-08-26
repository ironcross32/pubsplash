//! Settings for one auto-ducker, opened from a source's Effects tab.
//!
//! Every control here is deliberately in units a broadcaster can reason about
//! without knowing any audio engineering — a percentage of full volume, or a
//! time in milliseconds. There is no ratio, no knee and no decibel anywhere,
//! and the labels say what the setting *does* rather than what it is called in
//! a compressor manual.
//!
//! Purely a form: it edits a copy and hands it back, and the Effects page that
//! opened it decides what to do with the answer. Nothing here touches the
//! config or the engine.

use super::slider_uia::SliderAnnouncer;
use super::source_dialog::KeyChoice;
use crate::config::DuckerConfig;
use std::rc::Rc;
use wxdragon::prelude::*;

/// The row offered when a ducker is not listening to anything, and what an
/// unconfigured or orphaned key falls back to.
const NOTHING: &str = "(nothing)";

/// One slider and everything needed to keep it spoken.
struct Row {
    slider: Slider,
    announcer: Rc<SliderAnnouncer>,
}

/// Shows the dialog over `parent`, returning the edited settings, or `None` if
/// the user cancelled.
pub fn edit(parent: &Dialog, current: &DuckerConfig, keys: &[KeyChoice]) -> Option<DuckerConfig> {
    let dialog = Dialog::builder(parent, "Auto-ducker")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(520, 560)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    const LISTEN_TO: &str = "Listen to";
    let listen_label = StaticText::builder(&panel).with_label(LISTEN_TO).build();
    let listen = Choice::builder(&panel).build();
    super::set_accessible_name(&listen, LISTEN_TO);
    super::help::tag(
        &listen,
        "dialog.ducker.listenTo",
        "Source the ducker listens to",
    );
    // Row 0 is "listening to nothing", which is also where an orphaned key
    // lands: the source it named has been deleted, so there is nothing to
    // preselect and silently picking a different source would be worse.
    listen.append(NOTHING);
    for key in keys {
        listen.append(&key.label);
    }
    let preselect = keys
        .iter()
        .position(|k| k.name == current.key)
        .map(|i| i + 1)
        .unwrap_or(0);
    listen.set_selection(preselect as u32);

    let mut rows = Vec::new();
    // Builds one labelled, announcing slider. Deliberately does not tag the
    // control for help: `gen-help` scans for a literal id at the call site, so
    // the tags live beside the calls below.
    let mut slider = |label: &str, value: u32, max: u32, page: i32, unit: Unit| {
        let text = StaticText::builder(&panel).with_label(label).build();
        let control = Slider::builder(&panel)
            .with_value(value.min(max) as i32)
            .with_min_value(0)
            .with_max_value(max as i32)
            .build();
        super::set_accessible_name(&control, label);
        // `set_accessible_name` only answers MSAA; NVDA reads UIA and would
        // otherwise get the native trackbar's percentage-of-range. The
        // announcer is also what makes the value read in the user's units.
        let announcer = Rc::new(super::slider_uia::install(&control));
        let name = label.to_string();
        announcer.set_text(&name, &unit.format(value.min(max)));

        {
            let announcer = announcer.clone();
            let name = name.clone();
            control.on_slider(move |_| {
                announcer.update(&name, &unit.format(control.value().max(0) as u32));
            });
        }
        {
            // Every movement key is handled here rather than by the native
            // trackbar, whose arrow and page directions run backwards.
            let announcer = announcer.clone();
            let name = name.clone();
            control.on_key_down(move |event| {
                let Some((code, _)) = super::key_of(&event) else {
                    event.skip(true);
                    return;
                };
                let Some(value) =
                    super::slider_uia::key_step(code, control.value(), 0, max as i32, page)
                else {
                    event.skip(true);
                    return;
                };
                // wxDragon re-arms `Skip(true)` before every closure, so
                // without this the trackbar's default proc applies its
                // opposite mapping over the top of ours.
                event.skip(false);
                control.set_value(value);
                // Announced even at the end of the range, so the key always
                // produces spoken feedback.
                announcer.update(&name, &unit.format(value.max(0) as u32));
            });
        }

        sizer.add(&text, 0, SizerFlag::All, 4);
        sizer.add(&control, 0, SizerFlag::Expand | SizerFlag::All, 4);
        rows.push(Row {
            slider: control,
            announcer,
        });
        control
    };

    sizer.add(&listen_label, 0, SizerFlag::All, 4);
    sizer.add(&listen, 0, SizerFlag::Expand | SizerFlag::All, 4);

    let duck_to = slider("Ducked volume", current.duck_to, 100, 10, Unit::Percent);
    let trigger = slider("Start ducking at", current.trigger, 100, 5, Unit::Percent);
    let fade_down = slider(
        "Fade down over",
        current.fade_down_ms,
        MAX_FADE_DOWN_MS,
        50,
        Unit::Millis,
    );
    let fade_up = slider(
        "Fade back up over",
        current.fade_up_ms,
        MAX_TIME_MS,
        100,
        Unit::Millis,
    );
    let hold = slider(
        "Stay ducked for",
        current.hold_ms,
        MAX_TIME_MS,
        100,
        Unit::Millis,
    );
    super::help::tag(&duck_to, "dialog.ducker.duckTo", "Ducked volume slider");
    super::help::tag(&trigger, "dialog.ducker.trigger", "Ducking trigger slider");
    super::help::tag(
        &fade_down,
        "dialog.ducker.fadeDown",
        "Ducking fade down time slider",
    );
    super::help::tag(
        &fade_up,
        "dialog.ducker.fadeUp",
        "Ducking fade up time slider",
    );
    super::help::tag(&hold, "dialog.ducker.hold", "Ducking hold time slider");

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = super::ok_button(&panel, "OK");
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);

    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    ok.on_click(move |_| dialog.end_modal(ID_OK));
    cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));

    let confirmed = dialog.show_modal() == ID_OK;
    let edited = confirmed.then(|| DuckerConfig {
        key: listen
            .get_selection()
            .and_then(|row| (row as usize).checked_sub(1))
            .and_then(|i| keys.get(i))
            .map(|k| k.name.clone())
            .unwrap_or_default(),
        duck_to: duck_to.value().max(0) as u32,
        trigger: trigger.value().max(0) as u32,
        fade_down_ms: fade_down.value().max(0) as u32,
        fade_up_ms: fade_up.value().max(0) as u32,
        hold_ms: hold.value().max(0) as u32,
    });

    // The UIA provider registry is keyed by HWND, so it has to let go of every
    // slider before the window is destroyed and the handles can be recycled.
    for row in &rows {
        let _ = row.slider;
        row.announcer.uninstall();
    }
    dialog.destroy();
    edited
}

/// Ceiling for the fade-down slider. Separate from [`MAX_TIME_MS`] because a
/// slow *drop* defeats the point — the first word is over before the music has
/// moved — so the range stays where a useful value can be reached with a few
/// key presses.
const MAX_FADE_DOWN_MS: u32 = 500;
/// Ceiling for the recovery and hold sliders.
const MAX_TIME_MS: u32 = 3_000;

/// How a slider's value is read out.
#[derive(Clone, Copy)]
enum Unit {
    Percent,
    Millis,
}

impl Unit {
    fn format(self, value: u32) -> String {
        match self {
            // "of full volume" rather than a bare percent: the number is a
            // share of the source's own level, not of the master fader.
            Unit::Percent => format!("{value}% of full volume"),
            Unit::Millis => format!("{value} ms"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_say_what_they_are_a_percentage_of() {
        assert_eq!(Unit::Percent.format(25), "25% of full volume");
        assert_eq!(Unit::Millis.format(300), "300 ms");
    }
}

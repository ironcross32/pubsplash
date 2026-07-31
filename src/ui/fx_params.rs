//! An OSARA-style parameter dialog: a screen-reader-friendly way to adjust
//! any plugin's automation parameters without its native UI. Modelled on the
//! REAPER OSARA extension's FX parameter dialog.
//!
//! The dialog's logic lives behind the [`ParamSource`] trait so the stepping,
//! filtering, and value-formatting rules can be unit-tested against a fake.

use super::fx::{self, ChainTarget};
use super::slider_uia::{self, SliderAnnouncer};
use super::{App, WXK_END, WXK_ESCAPE, WXK_HOME, WXK_PAGEDOWN, WXK_PAGEUP, WXK_TAB};
use crate::vst::PluginInstance;
use std::rc::Rc;
use std::sync::Arc;
use wxdragon::prelude::*;

/// Updates the slider position and value text for a parameter, and announces
/// the new value to screen readers.
///
/// The formatted value (the plugin's own text, e.g. "-3.0 dB", falling back to
/// a percentage only when the plugin reports none) is pushed to the slider's
/// UIA provider ([`SliderAnnouncer`]), which exposes it as the slider's value
/// and raises a value-change event so a screen reader speaks it on each keyboard
/// step. The read-only value text mirrors it as a tab-to-read fallback.
fn announce_value(
    announcer: &SliderAnnouncer,
    slider: &Slider,
    value_text: &TextCtrl,
    name: &str,
    norm: f32,
    text: &str,
) {
    slider.set_value((norm * 1000.0).round() as i32);
    value_text.set_value(text);
    announcer.update(name, text);
}

/// Keyboard step sizes as fractions of the normalized 0..1 range (OSARA uses
/// 1/1000 for arrows, 1/50 for page keys).
const SMALL_STEP: f32 = 0.001;
const LARGE_STEP: f32 = 0.02;
/// How many extra steps to try before giving up on finding a value whose
/// formatted text differs (so discrete parameters move one detent per press).
const MAX_SNAP_TRIES: u32 = 10;

/// What the dialog needs from a plugin. Implemented for a real plugin and for
/// a test fake.
pub trait ParamSource {
    fn count(&self) -> i32;
    fn name(&self, index: i32) -> String;
    fn get(&self, index: i32) -> f32;
    fn set(&self, index: i32, value: f32);
    fn display(&self, index: i32) -> String;
    fn label(&self, index: i32) -> String;
    fn automatable(&self, index: i32) -> bool;
    /// Parses `text` in the parameter's own display units (e.g. "-6 dB") and
    /// applies it, returning whether the plugin supported the conversion. VST2 exposes this as an optional opcode; VST3 typed entry is disabled until a parse API is available.
    fn set_from_string(&self, index: i32, text: &str) -> bool;
}

impl ParamSource for Arc<PluginInstance> {
    fn count(&self) -> i32 {
        self.num_params()
    }
    fn name(&self, index: i32) -> String {
        self.param_name(index)
    }
    fn get(&self, index: i32) -> f32 {
        self.get_parameter(index)
    }
    fn set(&self, index: i32, value: f32) {
        self.set_parameter(index, value);
    }
    fn display(&self, index: i32) -> String {
        self.param_display(index)
    }
    fn label(&self, index: i32) -> String {
        self.param_label(index)
    }
    fn automatable(&self, index: i32) -> bool {
        self.can_be_automated(index)
    }
    fn set_from_string(&self, index: i32, text: &str) -> bool {
        self.string_to_parameter(index, text)
    }
}

/// What happened to a typed value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TypedValue {
    /// The plugin parsed the text; the new normalized value is attached.
    Applied(f32),
    /// The field was blank, so there was nothing to apply.
    Blank,
    /// The plugin cannot convert display text back to a value.
    Unsupported,
}

/// Applies `text` (in the parameter's display units) via the plugin's optional
/// string-to-parameter conversion.
///
/// The three outcomes are deliberately distinct: silently reverting the field
/// is indistinguishable from success to a screen-reader user, and for VST3 —
/// where no parameter supports this — that would make the field a dead control
/// that swallows everything typed into it.
pub fn apply_typed_value(src: &dyn ParamSource, index: i32, text: &str) -> TypedValue {
    let text = text.trim();
    if text.is_empty() {
        return TypedValue::Blank;
    }
    if src.set_from_string(index, text) {
        TypedValue::Applied(src.get(index))
    } else {
        TypedValue::Unsupported
    }
}

/// The human-readable value: the plugin's formatted text plus its unit, or a
/// percentage fallback when the plugin formats nothing.
pub fn formatted_value(src: &dyn ParamSource, index: i32) -> String {
    let display = src.display(index);
    if display.is_empty() {
        return format!("{}%", (src.get(index) * 100.0).round() as i32);
    }
    let label = src.label(index);
    if label.is_empty() {
        display
    } else {
        format!("{display} {label}")
    }
}

/// Whether a parameter name is meaningful (not blank or a bare number). OSARA
/// hides "unnamed" parameters by default.
pub fn is_named(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty() && !trimmed.chars().all(|c| c.is_ascii_digit())
}

/// One parameter's unchanging metadata.
pub struct ParamInfo {
    pub index: i32,
    pub name: String,
    /// Lowercased once, because the filter box compares against it on every
    /// keystroke.
    pub search_name: String,
    pub automatable: bool,
}

/// Reads every parameter's name and automatability.
///
/// Both are dispatcher round-trips into the plugin, and the filter box
/// re-filters on every keystroke — so re-reading them per keystroke cost a
/// 300-parameter plugin around 900 round-trips per character typed, in the one
/// dialog built specifically for screen-reader users. The dialog reads this
/// once when it opens and filters over the result.
///
/// The trade-off is that a plugin renaming its parameters while the dialog is
/// open shows stale names until it is reopened. That is rare, and the host
/// already ignores `AUDIO_MASTER_UPDATE_DISPLAY` (see `vst::host2`).
pub fn param_infos(src: &dyn ParamSource) -> Vec<ParamInfo> {
    (0..src.count())
        .map(|index| {
            let name = src.name(index);
            ParamInfo {
                index,
                search_name: name.to_lowercase(),
                name,
                automatable: src.automatable(index),
            }
        })
        .collect()
}

/// The parameter indices the dialog should show: automatable, name-matched,
/// and (unless `include_unnamed`) meaningfully named.
pub fn filter_infos(infos: &[ParamInfo], filter: &str, include_unnamed: bool) -> Vec<i32> {
    let filter = filter.trim().to_lowercase();
    infos
        .iter()
        .filter(|info| info.automatable)
        .filter(|info| {
            if !include_unnamed && !is_named(&info.name) {
                return false;
            }
            filter.is_empty() || info.search_name.contains(&filter)
        })
        .map(|info| info.index)
        .collect()
}

/// [`filter_infos`] straight off a plugin, reading its metadata as it goes.
/// The dialog caches via [`param_infos`] instead; this is for tests.
#[cfg(test)]
pub fn filter_params(src: &dyn ParamSource, filter: &str, include_unnamed: bool) -> Vec<i32> {
    filter_infos(&param_infos(src), filter, include_unnamed)
}

/// Applies one keyboard step and returns the new normalized value. Moves to
/// the next value whose formatted text differs from the current one: it tries
/// `delta` first (staying precise for finely-quantized parameters) and, if
/// the text is unchanged, grows the step geometrically (up to
/// [`MAX_SNAP_TRIES`] times) so a coarse discrete parameter still advances one
/// detent per keypress.
pub fn step_param(src: &dyn ParamSource, index: i32, delta: f32) -> f32 {
    let start = src.get(index);
    let start_text = formatted_value(src, index);
    let mut step = delta;
    let mut value = (start + step).clamp(0.0, 1.0);
    src.set(index, value);
    for _ in 0..MAX_SNAP_TRIES {
        if value <= 0.0 || value >= 1.0 || formatted_value(src, index) != start_text {
            break;
        }
        step *= 2.0;
        value = (start + step).clamp(0.0, 1.0);
        src.set(index, value);
    }
    value
}

/// Sets an absolute normalized value (Home/End) and returns it.
pub fn set_param(src: &dyn ParamSource, index: i32, value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    src.set(index, value);
    value
}

/// Opens the parameter dialog for a plugin instance. On close, the slot's
/// state is snapshotted back into config.
pub fn edit_parameters(
    app: &Rc<App>,
    target: ChainTarget,
    slot: usize,
    plugin: Arc<PluginInstance>,
) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    // Re-read the parameter list before showing it. A plugin may change its
    // parameter set at runtime — loading a preset in its own interface is
    // enough — and VST3 gives us no notification when it does. A stale
    // index-to-id map would have the dialog announce one parameter's name
    // while an arrow key edits a different parameter, which a screen-reader
    // user has no way to notice.
    plugin.refresh_params();
    let src: Rc<dyn ParamSource> = Rc::new(plugin);
    if src.count() == 0 {
        super::show_info(
            &frame,
            "Edit parameters",
            "This plugin exposes no automatable parameters.",
        );
        return;
    }

    let dialog = Dialog::builder(&frame, "Plugin parameters")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(460, 320)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let filter_label = StaticText::builder(&panel).with_label("Filter").build();
    let filter_box = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&filter_box, "Filter parameters");
    super::help::tag(
        &filter_box,
        "dialog.fxParams.filter",
        "Filter parameters by name box",
    );

    let param_label = StaticText::builder(&panel).with_label("Parameter").build();
    let param_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&param_choice, "Parameter");
    super::help::tag(
        &param_choice,
        "dialog.fxParams.parameter",
        "Plugin parameter chooser",
    );

    let value_label = StaticText::builder(&panel).with_label("Value").build();
    let value_slider = Slider::builder(&panel)
        .with_value(0)
        .with_min_value(0)
        .with_max_value(1000)
        .build();
    super::set_accessible_name(&value_slider, "Value");
    super::help::tag(
        &value_slider,
        "dialog.fxParams.value",
        "Selected parameter value slider",
    );
    // Give the slider a UIA provider so keyboard steps announce the plugin's
    // formatted value (see `slider_uia`). Kept alive until the dialog closes.
    let announcer = Rc::new(slider_uia::install(&value_slider));

    let value_text = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::ProcessEnter)
        .build();
    super::set_accessible_name(&value_text, "Value entry");
    super::help::tag(
        &value_text,
        "dialog.fxParams.valueText",
        "Type-in value entry for the selected parameter",
    );

    let unnamed_check = CheckBox::builder(&panel)
        .with_label("Show &unnamed parameters")
        .build();
    super::set_accessible_name(&unnamed_check, "Show unnamed parameters");
    super::help::tag(
        &unnamed_check,
        "dialog.fxParams.showUnnamed",
        "Show unnamed parameters checkbox",
    );

    // Dismiss-only: `dismiss_button` gives it the `ID_CANCEL` wx maps Escape to and
    // makes it the default item Enter fires. The per-control handlers below also
    // close on Escape, but only on the controls they are attached to; this covers
    // the rest of the dialog. Enter in `value_text` is unaffected — that control
    // has `ProcessEnter` and keeps the key to commit the typed value.
    let close = super::dismiss_button(&panel, "&Close");

    sizer.add(&filter_label, 0, SizerFlag::All, 4);
    sizer.add(&filter_box, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&param_label, 0, SizerFlag::All, 4);
    sizer.add(&param_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&value_label, 0, SizerFlag::All, 4);
    sizer.add(&value_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&value_text, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&unnamed_check, 0, SizerFlag::All, 8);
    sizer.add(&close, 0, SizerFlag::AlignRight | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // Visible parameter indices, refreshed by the filter and unnamed toggle.
    let visible: Rc<std::cell::RefCell<Vec<i32>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
    // Parameter names and automatability, read once — see `param_infos`.
    let infos = Rc::new(param_infos(src.as_ref()));

    // Updates the slider position, value text, and accessible value for the
    // currently selected parameter.
    let show_value = {
        let src = src.clone();
        let visible = visible.clone();
        let param_choice = param_choice.clone();
        let value_slider = value_slider.clone();
        let value_text = value_text.clone();
        let announcer = announcer.clone();
        move || {
            let Some(sel) = param_choice.get_selection() else {
                return;
            };
            let Some(&index) = visible.borrow().get(sel as usize) else {
                return;
            };
            let value = src.get(index);
            let text = formatted_value(src.as_ref(), index);
            announce_value(
                &announcer,
                &value_slider,
                &value_text,
                &src.name(index),
                value,
                &text,
            );
        }
    };

    let repopulate = {
        let infos = infos.clone();
        let visible = visible.clone();
        let filter_box = filter_box.clone();
        let param_choice = param_choice.clone();
        let unnamed_check = unnamed_check.clone();
        let show_value = show_value.clone();
        move || {
            let filter = filter_box.get_value();
            let indices = filter_infos(&infos, &filter, unnamed_check.get_value());
            *visible.borrow_mut() = indices.clone();
            param_choice.clear();
            for &index in &indices {
                // `param_infos` walks `0..count` in order, so position is index.
                let name = infos
                    .get(index as usize)
                    .map(|info| info.name.as_str())
                    .unwrap_or_default();
                param_choice.append(name);
            }
            if !indices.is_empty() {
                param_choice.set_selection(0);
            }
            show_value();
        }
    };
    repopulate();

    {
        let show_value = show_value.clone();
        param_choice
            .clone()
            .on_selection_changed(move |_| show_value());
    }
    {
        let repopulate = repopulate.clone();
        filter_box.clone().on_text_changed(move |_| repopulate());
    }
    {
        let repopulate = repopulate.clone();
        unnamed_check.clone().on_toggled(move |_| repopulate());
    }

    // Moves to the next/previous visible parameter (Ctrl+Tab), wrapping.
    let cycle_param = {
        let visible = visible.clone();
        let param_choice = param_choice.clone();
        let show_value = show_value.clone();
        move |forward: bool| {
            let count = visible.borrow().len();
            if count == 0 {
                return;
            }
            let current = param_choice.get_selection().unwrap_or(0) as usize;
            let next = if forward {
                (current + 1) % count
            } else {
                (current + count - 1) % count
            };
            param_choice.set_selection(next as u32);
            show_value();
        }
    };

    // The slider drives parameter values with OSARA's key mapping.
    {
        let src = src.clone();
        let visible = visible.clone();
        let param_choice = param_choice.clone();
        let value_slider = value_slider.clone();
        let value_text = value_text.clone();
        let cycle_param = cycle_param.clone();
        let dialog = dialog.clone();
        let announcer = announcer.clone();
        value_slider.clone().on_key_down(move |event| {
            let Some((code, ctrl)) = super::key_of(&event) else {
                event.skip(true);
                return;
            };
            // wxDragon re-arms `Skip(true)` before calling every closure, so
            // every key we act on needs an explicit `skip(false)`; otherwise the
            // native trackbar also handles it and drags the thumb away from the
            // value we just announced.
            if code == WXK_ESCAPE {
                event.skip(false);
                dialog.end_modal(ID_OK);
                return;
            }
            if code == WXK_TAB {
                if ctrl {
                    event.skip(false);
                    cycle_param(!event_shift(&event));
                } else {
                    event.skip(true);
                }
                return;
            }
            let Some(sel) = param_choice.get_selection() else {
                event.skip(true);
                return;
            };
            let Some(&index) = visible.borrow().get(sel as usize) else {
                event.skip(true);
                return;
            };
            let new_value = match code {
                super::WXK_UP => step_param(src.as_ref(), index, SMALL_STEP),
                super::WXK_DOWN => step_param(src.as_ref(), index, -SMALL_STEP),
                WXK_PAGEUP => step_param(src.as_ref(), index, LARGE_STEP),
                WXK_PAGEDOWN => step_param(src.as_ref(), index, -LARGE_STEP),
                WXK_HOME => set_param(src.as_ref(), index, 1.0),
                WXK_END => set_param(src.as_ref(), index, 0.0),
                _ => {
                    event.skip(true);
                    return;
                }
            };
            event.skip(false);
            let text = formatted_value(src.as_ref(), index);
            announce_value(
                &announcer,
                &value_slider,
                &value_text,
                &src.name(index),
                new_value,
                &text,
            );
        });
    }

    // The value-entry field: reads the current value (kept in sync by
    // `announce_value`), and on tab-away or Enter parses what was typed, in the
    // parameter's own display units, and applies it.
    //
    // A plugin that can't convert the text gets *said so*. `set_value` alone
    // announces nothing, so reverting the field in silence is indistinguishable
    // from the edit having worked — and for VST3 that is every parameter, since
    // `IEditController::getParamValueByString` has no wrapper in the crate.
    let apply_typed = {
        let src = src.clone();
        let visible = visible.clone();
        let param_choice = param_choice.clone();
        let value_slider = value_slider.clone();
        let value_text = value_text.clone();
        let announcer = announcer.clone();
        move || {
            let Some(sel) = param_choice.get_selection() else {
                return;
            };
            let Some(&index) = visible.borrow().get(sel as usize) else {
                return;
            };
            let current = formatted_value(src.as_ref(), index);
            match apply_typed_value(src.as_ref(), index, &value_text.get_value()) {
                TypedValue::Applied(new_value) => {
                    let text = formatted_value(src.as_ref(), index);
                    announce_value(
                        &announcer,
                        &value_slider,
                        &value_text,
                        &src.name(index),
                        new_value,
                        &text,
                    );
                }
                TypedValue::Blank => value_text.set_value(&current),
                TypedValue::Unsupported => {
                    value_text.set_value(&current);
                    announcer.update(
                        &src.name(index),
                        &format!("this plugin does not accept typed values, still {current}"),
                    );
                }
            }
        }
    };
    {
        let apply_typed = apply_typed.clone();
        value_text.clone().on_kill_focus(move |event| {
            apply_typed();
            event.skip(true);
        });
    }
    {
        let apply_typed = apply_typed.clone();
        value_text.clone().on_text_enter(move |event| {
            apply_typed();
            // Consumed, or the Return reaches the native EDIT and it beeps —
            // see the same call in `super::chat::build` for why.
            event.skip(false);
        });
    }

    // Ctrl+Tab anywhere in the dialog cycles parameters; Escape closes it.
    // (The value slider handles both itself, above.)
    let nav_keys = {
        let cycle_param = cycle_param.clone();
        let dialog = dialog.clone();
        move |event: WindowEventData| match super::key_of(&event) {
            Some((WXK_ESCAPE, _)) => dialog.end_modal(ID_OK),
            Some((WXK_TAB, true)) => cycle_param(!event_shift(&event)),
            _ => event.skip(true),
        }
    };
    {
        let nav_keys = nav_keys.clone();
        filter_box.clone().on_key_down(nav_keys);
    }
    {
        let nav_keys = nav_keys.clone();
        param_choice.clone().on_key_down(nav_keys);
    }
    {
        let nav_keys = nav_keys.clone();
        unnamed_check.clone().on_key_down(nav_keys);
    }
    {
        let nav_keys = nav_keys.clone();
        value_text.clone().on_key_down(nav_keys);
    }

    {
        let dialog = dialog.clone();
        close.on_click(move |_| dialog.end_modal(ID_OK));
    }

    dialog.show_modal();
    // Remove the slider's UIA provider while its window still exists.
    announcer.uninstall();
    dialog.destroy();

    // Persist whatever the user changed.
    fx::snapshot_slot(app, target, slot);
}

/// Whether Shift was held for a key event (for Ctrl+Shift+Tab).
fn event_shift(event: &WindowEventData) -> bool {
    if let WindowEventData::Keyboard(kb) = event {
        kb.shift_down()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeParam {
        name: String,
        value: RefCell<f32>,
        automatable: bool,
        /// Formats value into text; empty string means "no display".
        format: fn(f32) -> String,
        label: String,
        /// Parses display text into a normalized value; `None` means the plugin
        /// does not support string-to-parameter conversion.
        parse: Option<fn(&str) -> Option<f32>>,
    }

    struct FakeParams {
        params: Vec<FakeParam>,
    }

    impl ParamSource for FakeParams {
        fn count(&self) -> i32 {
            self.params.len() as i32
        }
        fn name(&self, index: i32) -> String {
            self.params[index as usize].name.clone()
        }
        fn get(&self, index: i32) -> f32 {
            *self.params[index as usize].value.borrow()
        }
        fn set(&self, index: i32, value: f32) {
            *self.params[index as usize].value.borrow_mut() = value.clamp(0.0, 1.0);
        }
        fn display(&self, index: i32) -> String {
            let p = &self.params[index as usize];
            (p.format)(*p.value.borrow())
        }
        fn label(&self, index: i32) -> String {
            self.params[index as usize].label.clone()
        }
        fn automatable(&self, index: i32) -> bool {
            self.params[index as usize].automatable
        }
        fn set_from_string(&self, index: i32, text: &str) -> bool {
            let p = &self.params[index as usize];
            match p.parse.and_then(|f| f(text)) {
                Some(v) => {
                    *p.value.borrow_mut() = v.clamp(0.0, 1.0);
                    true
                }
                None => false,
            }
        }
    }

    fn sample() -> FakeParams {
        FakeParams {
            params: vec![
                // Continuous, formatted in dB.
                FakeParam {
                    name: "Gain".into(),
                    value: RefCell::new(0.5),
                    automatable: true,
                    format: |v| format!("{:.1}", v * 24.0 - 12.0),
                    label: "dB".into(),
                    // Inverse of the dB mapping above (-12..+12 dB → 0..1).
                    parse: Some(|s| s.trim().parse::<f32>().ok().map(|db| (db + 12.0) / 24.0)),
                },
                // Discrete 4-state, text changes only at quarter boundaries.
                FakeParam {
                    name: "Mode".into(),
                    value: RefCell::new(0.0),
                    automatable: true,
                    format: |v| {
                        ["Off", "Low", "Mid", "High"][((v * 3.0).round() as usize).min(3)]
                            .to_string()
                    },
                    label: String::new(),
                    parse: None,
                },
                // Unnamed / non-automatable.
                FakeParam {
                    name: "3".into(),
                    value: RefCell::new(0.0),
                    automatable: true,
                    format: |_| String::new(),
                    label: String::new(),
                    parse: None,
                },
                FakeParam {
                    name: "Hidden".into(),
                    value: RefCell::new(0.0),
                    automatable: false,
                    format: |_| String::new(),
                    label: String::new(),
                    parse: None,
                },
            ],
        }
    }

    #[test]
    fn formatted_value_uses_display_and_label_then_percent() {
        let p = sample();
        assert_eq!(formatted_value(&p, 0), "0.0 dB");
        // Param 2 has no display text → percentage fallback.
        assert_eq!(formatted_value(&p, 2), "0%");
    }

    #[test]
    fn apply_typed_value_uses_conversion_or_reports_unsupported() {
        let p = sample();
        // Gain supports conversion: "-6" dB → 0.25 normalized, and re-displays.
        let applied = apply_typed_value(&p, 0, "-6");
        assert_eq!(applied, TypedValue::Applied(0.25));
        assert_eq!(formatted_value(&p, 0), "-6.0 dB");
        // Blank input is ignored — distinct from a refusal, so the dialog can
        // stay quiet for one and speak up for the other.
        assert_eq!(apply_typed_value(&p, 0, "   "), TypedValue::Blank);
        // Mode does not support conversion; the value is left alone.
        assert_eq!(apply_typed_value(&p, 1, "High"), TypedValue::Unsupported);
        assert_eq!(*p.params[1].value.borrow(), 0.0);
    }

    #[test]
    fn is_named_rejects_blank_and_numeric() {
        assert!(is_named("Gain"));
        assert!(!is_named(""));
        assert!(!is_named("   "));
        assert!(!is_named("42"));
    }

    #[test]
    fn filter_hides_unnamed_and_non_automatable() {
        let p = sample();
        let visible = filter_params(&p, "", false);
        // Gain and Mode only (index 2 unnamed, index 3 not automatable).
        assert_eq!(visible, vec![0, 1]);
        // Including unnamed adds index 2 but never the non-automatable one.
        assert_eq!(filter_params(&p, "", true), vec![0, 1, 2]);
        // Substring filter.
        assert_eq!(filter_params(&p, "mod", false), vec![1]);
    }

    #[test]
    fn small_step_moves_continuous_param_to_next_distinct_value() {
        let p = sample();
        let v = step_param(&p, 0, SMALL_STEP);
        // Moves up to the next value whose displayed dB differs.
        assert!(v > 0.5 && v < 0.55, "got {v}");
        assert_ne!(formatted_value(&p, 0), "0.0 dB");
    }

    #[test]
    fn step_snaps_discrete_param_by_one_detent() {
        let p = sample();
        // Mode starts at "Off" (0.0). One small up-step must reach "Low".
        assert_eq!(formatted_value(&p, 1), "Off");
        step_param(&p, 1, SMALL_STEP);
        assert_eq!(formatted_value(&p, 1), "Low");
    }

    #[test]
    fn set_param_clamps() {
        let p = sample();
        assert_eq!(set_param(&p, 0, 2.0), 1.0);
        assert_eq!(set_param(&p, 0, -1.0), 0.0);
    }
}

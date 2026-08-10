//! The per-source Sends dialog: which destinations a source feeds, and at what
//! level.
//!
//! Every destination the source could feed is on show at once — master output
//! as the pinned first row, then every bus — as a check list the space bar
//! toggles. There is no "add a send" step and no separate "send directly to
//! master" checkbox: master is a row like any other, and a send exists exactly
//! when its row is checked.
//!
//! The rows say "on" or "off" as well as carrying a check mark, and that is not
//! belt and braces. A `wxCheckListBox` on MSW is an owner-drawn `LISTBOX`: wx
//! paints the check itself, so there is no native checkbox for Windows to
//! describe and no `STATE_SYSTEM_CHECKED` for a screen reader to read — verified
//! with NVDA, which announced the row and said nothing about its state. The
//! state has to be in the text or it does not exist. Toggling therefore rebuilds
//! the list (wxdragon's `CheckListBox` has no `set_string`), restores the
//! selection, and raises the row's focus event so the row the user is standing
//! on speaks its new state.
//!
//! Each checked bus gets its own level slider, which appears when the bus is
//! checked and disappears when it is unchecked. Those sliders are all built up
//! front and only shown and hidden, never created or destroyed: a hidden window
//! is out of the Tab order and out of the accessibility tree, but its
//! `SliderAnnouncer` stays valid, which keeps the whole HWND-keyed provider
//! lifetime problem (see `home.rs::drop_mixer_strips`) out of this file.
//!
//! Edits apply live — there is no OK, only Close. That is why the engine work
//! goes through `App::sync_source_routing` and `App::set_send_level` rather
//! than `App::sync_engine_sources`: the latter carries the whole source list,
//! which respawns every capture thread in the app, and doing that on a slider
//! drag would restart the microphone mid-stream.

use super::App;
use crate::config::SendConfig;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wxdragon::prelude::*;

/// The pinned first row, standing for the master output. Matches the Buses
/// tab's own pinned row (see `buses.rs`).
const MASTER_ROW: &str = "Master output";

/// How far Page Up and Page Down move a level slider, over its 0-100 range.
const PAGE_STEP: i32 = 10;

/// A send starts at unity unless this dialog already knows a level for it.
const DEFAULT_LEVEL: u32 = 100;

/// How tall the bus list is, in pixels. Fixed, and the list is added to the
/// sizer at proportion 0, so it scrolls its own rows instead of stretching to
/// swallow the dialog or being squeezed by however many sliders are showing.
const LIST_HEIGHT: i32 = 170;

/// The dialog never fits itself narrower or shorter than this.
const MIN_WIDTH: i32 = 460;
const MIN_HEIGHT: i32 = 320;

/// The check list's rows: master first, then the buses in config order, each
/// saying whether it is on.
///
/// The state is in the text because on MSW there is nowhere else to put it that
/// a screen reader can reach — see the module docs.
fn row_labels(buses: &[String], to_master: bool, sends: &[SendConfig]) -> Vec<String> {
    let state = |on: bool| if on { "on" } else { "off" };
    std::iter::once(format!("{MASTER_ROW}, {}", state(to_master)))
        .chain(
            buses
                .iter()
                .map(|bus| format!("{bus}, {}", state(sends.iter().any(|s| &s.bus == bus)))),
        )
        .collect()
}

/// Whether the source is heard anywhere at all.
fn is_routed(to_master: bool, sends: &[SendConfig]) -> bool {
    to_master || !sends.is_empty()
}

/// What, if anything, to tell the user as the dialog closes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Notice {
    /// Nothing is checked, so nothing anywhere will carry this source.
    HeardNowhere,
    /// Nothing is checked and the source is speech that is meant to reach the
    /// stream — the broadcaster still hears it, the listeners do not.
    SpeechOffStream,
}

/// The notice the closing dialog owes, given where the source ended up.
///
/// Speech is a case of its own twice over. A TTS source is always played out of
/// the local device whatever its routing (`SourceSpec::local`), so "will not be
/// heard anywhere" would be a lie; and when its own "send speech to the stream"
/// is off, its routing is ignored outright, so an empty list is what that
/// setting *means* and there is nothing to report.
fn closing_notice(
    is_tts: bool,
    reaches_stream: bool,
    to_master: bool,
    sends: &[SendConfig],
) -> Option<Notice> {
    if is_routed(to_master, sends) || !reaches_stream {
        return None;
    }
    Some(if is_tts {
        Notice::SpeechOffStream
    } else {
        Notice::HeardNowhere
    })
}

/// Applies one row's new checked state, returning the bus it touched (`None`
/// for the master row, which has no send and no level).
///
/// `remembered` carries the level of a send across an uncheck, so toggling a
/// bus off and straight back on within one visit does not silently reset it to
/// unity.
fn apply_toggle(
    to_master: &mut bool,
    sends: &mut Vec<SendConfig>,
    buses: &[String],
    row: usize,
    checked: bool,
    remembered: &mut HashMap<String, u32>,
) -> Option<String> {
    let Some(bus) = row.checked_sub(1).and_then(|i| buses.get(i)) else {
        // Row 0, or a row that outlived its bus.
        if row == 0 {
            *to_master = checked;
        }
        return None;
    };
    let existing = sends.iter().position(|s| &s.bus == bus);
    match (checked, existing) {
        (true, None) => {
            let level = remembered.get(bus).copied().unwrap_or(DEFAULT_LEVEL);
            // Kept in bus order, so the settings file does not churn on a
            // toggle and the file reads the way the dialog looks.
            let order = |name: &String| buses.iter().position(|b| b == name);
            let at = sends
                .iter()
                .position(|s| order(&s.bus) > order(bus))
                .unwrap_or(sends.len());
            sends.insert(
                at,
                SendConfig {
                    bus: bus.clone(),
                    level,
                },
            );
        }
        (false, Some(index)) => {
            remembered.insert(bus.clone(), sends[index].level);
            sends.remove(index);
        }
        // Already in the state the user asked for.
        _ => {}
    }
    Some(bus.clone())
}

/// One bus's level control: hidden until the bus is checked.
struct LevelRow {
    bus: String,
    /// What the slider announces as, kept so every update speaks the same name.
    name: String,
    label: StaticText,
    slider: Slider,
    announcer: Rc<super::slider_uia::SliderAnnouncer>,
}

/// Writes a send's level to the config and applies it to the running mix.
fn apply_level(app: &Rc<App>, scene_index: usize, source_index: usize, bus: &str, value: u32) {
    {
        let mut config = app.config.borrow_mut();
        let Some(send) = config
            .scenes
            .scenes
            .get_mut(scene_index)
            .and_then(|s| s.sources.get_mut(source_index))
            .and_then(|s| s.sends.iter_mut().find(|s| s.bus == bus))
        else {
            return;
        };
        send.level = value;
    }
    // Both of these borrow the config, so neither may run above.
    app.set_send_level(scene_index, source_index, bus, value);
    app.save_config();
}

/// The level a send currently carries, if it exists.
fn level_of(app: &Rc<App>, scene_index: usize, source_index: usize, bus: &str) -> Option<u32> {
    let config = app.config.borrow();
    config
        .scenes
        .scenes
        .get(scene_index)?
        .sources
        .get(source_index)?
        .sends
        .iter()
        .find(|s| s.bus == bus)
        .map(|s| s.level)
}

/// Rewrites every row from the config and puts the selection back on `select`.
///
/// A rebuild rather than a relabel because wxdragon's `CheckListBox` has no
/// `set_string`, and the whole list rather than the toggled row because there is
/// no way to touch one row on its own. It is cheap — there are as many rows as
/// the user has buses — and it happens only on a deliberate toggle.
///
/// Restoring the selection is what makes the row speak its new state, and it
/// must be the *only* thing that does. `clear` drops the selection, so setting
/// it again is a real change and the list box raises its own focus and
/// selection events for the row — exactly what arrowing onto a row raises, and
/// NVDA reads it the same way. An explicit `EVENT_OBJECT_FOCUS` would be a
/// second announcement, so `native_acc` deliberately handles only
/// `WM_GETOBJECT` and leaves row events to the native list.
///
/// `freeze`/`thaw` keeps the rebuild from flickering.
fn relabel_rows(
    app: &Rc<App>,
    scene_index: usize,
    source_index: usize,
    list: &CheckListBox,
    buses: &[String],
    select: u32,
) {
    let (to_master, sends) = {
        let config = app.config.borrow();
        let Some(source) = config
            .scenes
            .scenes
            .get(scene_index)
            .and_then(|s| s.sources.get(source_index))
        else {
            return;
        };
        (source.to_master, source.sends.clone())
    };
    list.freeze();
    list.clear();
    for label in row_labels(buses, to_master, &sends) {
        list.append(&label);
    }
    list.check(0, to_master);
    for (index, bus) in buses.iter().enumerate() {
        list.check((index + 1) as u32, sends.iter().any(|s| &s.bus == bus));
    }
    list.thaw();
    // Last, and after `thaw`, so the announcement it raises describes a list
    // that is finished being rebuilt.
    if select < list.get_count() {
        list.set_selection(select, true);
    }
}

/// Resizes the dialog around whatever is currently showing, so the sliders that
/// have appeared all fit and the ones that have gone leave no hole.
///
/// The bus list has a fixed height of its own and scrolls, so this only ever
/// grows and shrinks by whole slider rows.
fn fit_dialog(dialog: &Dialog, panel: &Panel) {
    panel.layout();
    dialog.fit();
    let size = dialog.get_size();
    dialog.set_size(Size::new(
        size.width.max(MIN_WIDTH),
        size.height.max(MIN_HEIGHT),
    ));
}

/// Opens the sends dialog for one source of one scene.
pub fn edit_sends(app: &Rc<App>, scene_index: usize, source_index: usize) {
    let Some(frame) = app.widgets(|w| w.frame) else {
        return;
    };
    let (source_name, bus_names, to_master, sends) = {
        let config = app.config.borrow();
        let Some(source) = config
            .scenes
            .scenes
            .get(scene_index)
            .and_then(|s| s.sources.get(source_index))
        else {
            return;
        };
        (
            crate::source_name::strip_label(
                source,
                &app.name_context(std::slice::from_ref(source)),
            ),
            config
                .buses
                .buses
                .iter()
                .map(|b| b.name.clone())
                .collect::<Vec<_>>(),
            source.to_master,
            source.sends.clone(),
        )
    };
    // No "there are no buses yet" refusal: with master as row 0 the dialog is
    // still the place the user routes a source to it.

    let dialog = Dialog::builder(&frame, &format!("Sends for {source_name}"))
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(MIN_WIDTH, MIN_HEIGHT)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // The static text is not decoration: `native_acc` hands the list back to
    // Windows, and the native list box takes its accessible name from the
    // control immediately before it.
    let list_label = StaticText::builder(&panel).with_label("Buses").build();
    let bus_list = CheckListBox::builder(&panel)
        .with_size(Size::new(-1, LIST_HEIGHT))
        .with_choices(row_labels(&bus_names, to_master, &sends))
        .build();
    super::native_acc::install_check_list(&bus_list, "Buses");
    super::help::tag(
        &bus_list,
        "dialog.sends.busList",
        "Destinations for this source list",
    );
    bus_list.check(0, to_master);
    for (index, bus) in bus_names.iter().enumerate() {
        bus_list.check((index + 1) as u32, sends.iter().any(|s| &s.bus == bus));
    }
    // No placeholder row, and none of `ui::list`'s machinery: the master row
    // always exists, so this list can never be empty.
    bus_list.set_selection(0, true);

    let levels_label = StaticText::builder(&panel)
        .with_label("Send levels")
        .build();

    sizer.add(&list_label, 0, SizerFlag::All, 4);
    // Proportion 0: the list keeps `LIST_HEIGHT` and scrolls its own rows,
    // rather than stretching to fill the dialog or being squeezed by however
    // many sliders happen to be showing.
    sizer.add(&bus_list, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&levels_label, 0, SizerFlag::All, 4);

    // One slider per bus, in bus order, built once. Creation order is Tab
    // order, and a hidden slider is skipped by both.
    let mut rows = Vec::new();
    for bus in &bus_names {
        let name = format!("{bus} send level");
        let label = StaticText::builder(&panel).with_label(&name).build();
        let level = sends.iter().find(|s| &s.bus == bus).map(|s| s.level);
        let slider = Slider::builder(&panel)
            .with_value(level.unwrap_or(DEFAULT_LEVEL) as i32)
            .with_min_value(0)
            .with_max_value(100)
            .build();
        super::set_accessible_name(&slider, &name);
        // A native trackbar announces its own position through UIA, which is
        // what NVDA reads on Windows 10/11 — `set_accessible_name` only answers
        // MSAA. Our provider is what makes the bus name and the level spoken.
        let announcer = Rc::new(super::slider_uia::install(&slider));
        announcer.set_text(&name, &level.unwrap_or(DEFAULT_LEVEL).to_string());
        super::help::tag(
            &slider,
            "dialog.sends.level",
            "Send level slider for one bus",
        );
        let shown = level.is_some();
        label.show(shown);
        slider.show(shown);
        sizer.add(&label, 0, SizerFlag::All, 4);
        sizer.add(&slider, 0, SizerFlag::Expand | SizerFlag::All, 4);

        // Mouse drags.
        {
            let app = app.clone();
            let bus = bus.clone();
            let name = name.clone();
            let announcer = announcer.clone();
            let slider_for_handler = slider;
            slider.on_slider(move |_| {
                let value = slider_for_handler.value().clamp(0, 100) as u32;
                apply_level(&app, scene_index, source_index, &bus, value);
                announcer.update(&name, &value.to_string());
            });
        }

        // Every movement key is handled here rather than by the native
        // trackbar, whose directions are backwards; see `slider_uia::key_step`.
        {
            let app = app.clone();
            let bus = bus.clone();
            let name = name.clone();
            let announcer = announcer.clone();
            let slider_for_keys = slider;
            slider.on_key_down(move |event| {
                let Some((code, _)) = super::key_of(&event) else {
                    event.skip(true);
                    return;
                };
                let current = slider_for_keys.value();
                let Some(value) = super::slider_uia::key_step(code, current, 0, 100, PAGE_STEP)
                else {
                    event.skip(true);
                    return;
                };
                // Consuming the key matters: wxDragon re-arms `Skip(true)`
                // before every closure, and the trackbar's default proc would
                // otherwise apply its opposite mapping and fire a
                // `wxEVT_SLIDER` over the top.
                event.skip(false);
                slider_for_keys.set_value(value);
                apply_level(&app, scene_index, source_index, &bus, value as u32);
                // Announced even at the end of the range, so the key always
                // produces spoken feedback.
                announcer.update(&name, &value.to_string());
            });
        }

        rows.push(LevelRow {
            bus: bus.clone(),
            name,
            label,
            slider,
            announcer,
        });
    }
    let rows = Rc::new(rows);
    levels_label.show(rows.iter().any(|r| r.slider.is_shown()));

    let close = super::dismiss_button(&panel, "Close");
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    buttons.add(&close, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);

    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);
    fit_dialog(&dialog, &panel);

    // Levels of sends that have been switched off during this visit.
    let remembered: Rc<RefCell<HashMap<String, u32>>> = Rc::new(RefCell::new(
        sends.iter().map(|s| (s.bus.clone(), s.level)).collect(),
    ));

    {
        let app = app.clone();
        let rows = rows.clone();
        let bus_names = bus_names.clone();
        let remembered = remembered.clone();
        bus_list.on_toggled(move |event| {
            let Some(row) = event.get_selection() else {
                return;
            };
            // wxMSW does not reliably set the command event's checked flag —
            // only its int, the row — so the state is read from the control.
            let checked = bus_list.is_checked(row);
            let touched = {
                let mut config = app.config.borrow_mut();
                let Some(source) = config
                    .scenes
                    .scenes
                    .get_mut(scene_index)
                    .and_then(|s| s.sources.get_mut(source_index))
                else {
                    return;
                };
                apply_toggle(
                    &mut source.to_master,
                    &mut source.sends,
                    &bus_names,
                    row as usize,
                    checked,
                    &mut remembered.borrow_mut(),
                )
            };
            app.save_config();
            app.sync_source_routing(scene_index, source_index);
            // The row now says something different, and rewriting it is the
            // only way the new state is spoken at all.
            relabel_rows(&app, scene_index, source_index, &bus_list, &bus_names, row);
            let Some(bus) = touched else {
                return;
            };
            let Some(level_row) = rows.iter().find(|r| r.bus == bus) else {
                return;
            };
            if checked {
                let level =
                    level_of(&app, scene_index, source_index, &bus).unwrap_or(DEFAULT_LEVEL);
                level_row.slider.set_value(level as i32);
                // `set_text`, not `update`: the row the user just toggled is
                // already speaking, and an event would chase it with a
                // redundant read of a slider they have not reached yet.
                level_row
                    .announcer
                    .set_text(&level_row.name, &level.to_string());
            }
            level_row.label.show(checked);
            level_row.slider.show(checked);
            levels_label.show(rows.iter().any(|r| r.slider.is_shown()));
            fit_dialog(&dialog, &panel);
        });
    }

    {
        close.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    dialog.show_modal();
    // The provider registry is keyed by HWND, so it must let go of these
    // sliders before the windows are destroyed and the handles recycled.
    for row in rows.iter() {
        row.announcer.uninstall();
    }
    dialog.destroy();

    // Deliberately after the dialog is gone, so every way out of it — the
    // button, Escape, the title bar — is checked the same way. A modal is right
    // here: it answers something the user just did and is sitting waiting on.
    let notice = {
        let config = app.config.borrow();
        config
            .scenes
            .scenes
            .get(scene_index)
            .and_then(|s| s.sources.get(source_index))
            .and_then(|s| {
                closing_notice(
                    matches!(&s.kind, crate::config::SourceKindConfig::Tts(_)),
                    super::tts_reaches_the_stream(s),
                    s.to_master,
                    &s.sends,
                )
            })
    };
    match notice {
        Some(Notice::HeardNowhere) => super::show_info(
            &frame,
            "Sends",
            &format!(
                "{source_name} is not sending to master or to any bus, so it will not be heard \
                 anywhere. Check a destination in the Sends dialog to route it."
            ),
        ),
        Some(Notice::SpeechOffStream) => super::show_info(
            &frame,
            "Sends",
            &format!(
                "{source_name} is not sending to master or to any bus, so your listeners will \
                 not hear it. You will still hear it yourself. Check a destination in the Sends \
                 dialog to put it on the stream."
            ),
        ),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buses() -> Vec<String> {
        vec!["Music".into(), "Voice".into(), "Effects".into()]
    }

    fn send(bus: &str, level: u32) -> SendConfig {
        SendConfig {
            bus: bus.into(),
            level,
        }
    }

    #[test]
    fn master_is_the_first_row_and_every_row_says_its_state() {
        assert_eq!(
            row_labels(&buses(), true, &[send("Voice", 80)]),
            vec![
                "Master output, on",
                "Music, off",
                "Voice, on",
                "Effects, off"
            ]
        );
        assert_eq!(
            row_labels(&buses(), false, &[])[0],
            "Master output, off".to_string()
        );
    }

    #[test]
    fn row_zero_is_master_and_touches_no_send() {
        let mut to_master = false;
        let mut sends = vec![send("Music", 50)];
        let touched = apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            0,
            true,
            &mut HashMap::new(),
        );
        assert_eq!(touched, None);
        assert!(to_master);
        assert_eq!(sends.len(), 1);

        apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            0,
            false,
            &mut HashMap::new(),
        );
        assert!(!to_master);
        assert_eq!(sends.len(), 1);
    }

    #[test]
    fn checking_a_bus_adds_a_send_at_unity() {
        let mut to_master = true;
        let mut sends = Vec::new();
        let touched = apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            2,
            true,
            &mut HashMap::new(),
        );
        assert_eq!(touched.as_deref(), Some("Voice"));
        assert_eq!(sends, vec![send("Voice", DEFAULT_LEVEL)]);
        assert!(to_master);
    }

    #[test]
    fn sends_stay_in_bus_order() {
        let mut to_master = false;
        let mut sends = Vec::new();
        let mut remembered = HashMap::new();
        for row in [3, 1, 2] {
            apply_toggle(
                &mut to_master,
                &mut sends,
                &buses(),
                row,
                true,
                &mut remembered,
            );
        }
        assert_eq!(
            sends.iter().map(|s| s.bus.as_str()).collect::<Vec<_>>(),
            vec!["Music", "Voice", "Effects"]
        );
    }

    #[test]
    fn unchecking_removes_the_send_and_remembers_its_level() {
        let mut to_master = false;
        let mut sends = vec![send("Music", 40), send("Voice", 70)];
        let mut remembered = HashMap::new();
        apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            1,
            false,
            &mut remembered,
        );
        assert_eq!(sends, vec![send("Voice", 70)]);
        assert_eq!(remembered.get("Music"), Some(&40));

        // ...and re-checking restores it rather than resetting to unity.
        apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            1,
            true,
            &mut remembered,
        );
        assert_eq!(sends, vec![send("Music", 40), send("Voice", 70)]);
    }

    #[test]
    fn toggling_to_the_state_it_is_already_in_changes_nothing() {
        let mut to_master = false;
        let mut sends = vec![send("Music", 40)];
        let mut remembered = HashMap::new();
        apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            1,
            true,
            &mut remembered,
        );
        assert_eq!(sends, vec![send("Music", 40)]);
        apply_toggle(
            &mut to_master,
            &mut sends,
            &buses(),
            3,
            false,
            &mut remembered,
        );
        assert_eq!(sends, vec![send("Music", 40)]);
    }

    #[test]
    fn routed_nowhere_is_what_the_closing_notice_looks_for() {
        assert!(!is_routed(false, &[]));
        assert!(is_routed(true, &[]));
        assert!(is_routed(false, &[send("Music", 0)]));
    }

    #[test]
    fn a_routed_source_says_nothing_on_the_way_out() {
        assert_eq!(closing_notice(false, true, true, &[]), None);
        assert_eq!(
            closing_notice(false, true, false, &[send("Music", 50)]),
            None
        );
    }

    #[test]
    fn an_unrouted_source_is_heard_nowhere() {
        assert_eq!(
            closing_notice(false, true, false, &[]),
            Some(Notice::HeardNowhere)
        );
    }

    #[test]
    fn unrouted_speech_is_still_heard_by_the_broadcaster() {
        assert_eq!(
            closing_notice(true, true, false, &[]),
            Some(Notice::SpeechOffStream)
        );
    }

    #[test]
    fn speech_kept_off_the_stream_on_purpose_says_nothing() {
        // "Send speech to the stream" being off is exactly what an empty
        // destination list means for that source, so there is nothing to report.
        assert_eq!(closing_notice(true, false, false, &[]), None);
    }
}

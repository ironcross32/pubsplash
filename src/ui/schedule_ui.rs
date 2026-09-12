//! File > Schedule stream: arming a stream to go live at a set time, and the
//! once-a-second tick that fires it.
//!
//! The state machine and all the time arithmetic live in [`crate::schedule`],
//! which has no widgets and is unit-tested. This file is the dialog, the
//! `Runtime` bookkeeping, and the three actions a stage can call for.
//!
//! Two rules run through the whole file.
//!
//! **Nothing on the firing path may show a modal.** A dialog raised from the
//! one-second timer opens a nested event loop with nobody in front of the
//! machine to dismiss it, so the broadcast the schedule was armed for would sit
//! behind it instead of going out. Every question that could be asked is
//! therefore asked in [`show`], while the user is sitting there: the streaming
//! service, and the stream info. What can still go wrong later — the service
//! being disconnected in the meantime — is handled by cancelling the schedule
//! with a spoken and logged reason, which is what CLAUDE.md asks for anything
//! that arrives unbidden and especially anything that can arrive mid-broadcast.
//! [`pump`] calls `super::begin_stream`, never `super::start_streaming`, so this
//! is structural rather than a promise.
//!
//! **The schedule is updated before the action it describes is taken.** Setting
//! `connect_fired` before starting the stream, and clearing the schedule before
//! switching the scene, is the same discipline `mastodon_post::maybe_periodic`
//! applies for the same reason: both `begin_stream` and `switch_to_scene_named`
//! reach back into wx, and a re-entrant tick that found the old state would fire
//! twice.

use crate::t;
use super::App;
use crate::schedule::{self, Advanced, Schedule, Stage};
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

fn caption() -> String {
    t!("Schedule stream")
}

/// Index of the Advanced row in the mode radio box.
const ADVANCED: i32 = 1;

/// Shows the dialog and, if the user confirms, arms the schedule.
///
/// The preflight below runs *before* the dialog is built, so a user who cannot
/// schedule anything is told why rather than made to fill a form in first.
pub fn show(app: &Rc<App>, parent: &Frame) {
    if let Some(reason) = why_not(app) {
        super::show_error(parent, &caption(), &reason);
        return;
    }
    // Asked now rather than at the scheduled moment, which is the whole reason
    // this is here: `start_streaming` would raise this dialog on a firing path
    // that cannot afford one. Unlike the connected-service check above, this one
    // is a real guarantee — `stream_info_set` is only ever set, never cleared.
    if !app.run.borrow().stream_info_set && !super::stream_info_dialog::show(app, parent) {
        return;
    }
    let Some(schedule) = ask(app, parent) else {
        return;
    };
    let when = schedule.connect_at;
    app.run.borrow_mut().schedule = Some(schedule);
    app.refresh_stream_ui();
    let remaining = schedule::format_countdown(when.saturating_sub(crate::mastodon::now_unix()));
    super::help::announce(&t!("Stream scheduled, connecting in {remaining}", remaining = remaining));
}

/// Why a schedule cannot be armed right now, or `None` if one can.
fn why_not(app: &Rc<App>) -> Option<String> {
    let run = app.run.borrow();
    if app.schedule_armed() {
        return Some(t!(
            "A stream is already scheduled. Cancel it from the Home tab before scheduling another."
        ));
    }
    if !matches!(run.stream, super::StreamState::Idle) {
        return Some(t!("A stream is already running."));
    }
    if run.recording || run.recording_pending {
        return Some(t!(
            "Stop the recording first: Pubsplash cannot stream while one is running."
        ));
    }
    if run.connected_service.is_none() {
        // Deliberately the same sentence `start_streaming` uses, so the two
        // routes to a stream do not describe the same problem two ways.
        return Some(t!(
            "Connect to a streaming service first (File > Setup streaming services)."
        ));
    }
    None
}

/// Builds and runs the dialog, returning the schedule the user described.
fn ask(app: &Rc<App>, parent: &Frame) -> Option<Schedule> {
    let scenes: Vec<String> = app
        .config
        .borrow()
        .scenes
        .scenes
        .iter()
        .map(|s| s.name.clone())
        .collect();
    let active = app.config.borrow().scenes.active_scene.clone();
    let active_index = scenes.iter().position(|n| *n == active).unwrap_or(0) as u32;

    // Sized for the advanced layout, which is the taller of the two: the fields
    // are hidden rather than removed, so the dialog does not shrink back.
    let dialog = Dialog::builder(parent, &caption())
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(480, 460)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let modes = [t!("Simple"), t!("Advanced")];
    let modes: Vec<&str> = modes.iter().map(String::as_str).collect();
    let mode = RadioBox::builder(&panel, &modes)
        .with_label(&t!("Scheduling mode"))
        .with_style(RadioBoxStyle::SpecifyRows)
        .with_major_dimension(1)
        .build();
    mode.set_selection(0);
    // A radio box's items are real child windows, so `set_accessible_name` can
    // only reach the group. See `native_acc::install_radio_box`.
    super::native_acc::install_radio_box(&mode, &t!("Scheduling mode"));
    super::help::tag(&mode, "dialog.schedule.mode", "Scheduling mode selector");

    // Each picker gets two accessibility calls, and they answer different
    // questions. `set_accessible_name` names the control, which is right here
    // because a picker has no MSAA children for it to shadow (`native_acc`'s
    // header records the measurement). `picker_acc::install` is what makes the
    // *fields* speak as the user arrows across them, which Windows does not do on
    // its own — that module's header says why at length.
    let date_label = StaticText::builder(&panel).with_label(&t!("Date")).build();
    let date = DatePickerCtrl::builder(&panel).build();
    super::set_accessible_name(&date, &t!("Date"));
    // Before the value and the range below, and it has to be: identifying the
    // fields sets a probe value and reads back what the control displays, and a
    // range would clamp that probe to something else.
    super::picker_acc::install(&date, super::picker_acc::Kind::Date);
    let today = DateTime::now();
    date.set_value(&today);
    // No past date can be chosen at all, which removes one whole class of
    // "that time has already passed" before the user can reach it.
    date.set_range(Some(&today), None);
    super::help::tag(&date, "dialog.schedule.date", "Date to go live on");

    // Labelled for simple mode; `apply_mode` rewrites both the visible label and
    // the accessible name for advanced, since the field means something more
    // specific there.
    let first_label = StaticText::builder(&panel).with_label(&t!("Time")).build();
    let first_time = TimePickerCtrl::builder(&panel).build();
    super::set_accessible_name(&first_time, &t!("Time"));
    super::picker_acc::install(&first_time, super::picker_acc::Kind::Time);
    super::help::tag(
        &first_time,
        "dialog.schedule.time",
        "Time to connect and go live",
    );

    let second_label = StaticText::builder(&panel)
        .with_label(&t!("Start of stream time"))
        .build();
    let second_time = TimePickerCtrl::builder(&panel).build();
    super::set_accessible_name(&second_time, &t!("Start of stream time"));
    super::picker_acc::install(&second_time, super::picker_acc::Kind::Time);
    super::help::tag(
        &second_time,
        "dialog.schedule.startTime",
        "Time the content begins and the scene switches",
    );

    let pre_scene_label = StaticText::builder(&panel)
        .with_label(&t!("Pre-stream scene"))
        .build();
    let pre_scene = Choice::builder(&panel).build();
    let start_scene_label = StaticText::builder(&panel)
        .with_label(&t!("Start of stream scene"))
        .build();
    let start_scene = Choice::builder(&panel).build();
    for name in &scenes {
        pre_scene.append(name);
        start_scene.append(name);
    }
    pre_scene.set_selection(active_index);
    start_scene.set_selection(active_index);
    super::set_accessible_name(&pre_scene, &t!("Pre-stream scene"));
    super::set_accessible_name(&start_scene, &t!("Start of stream scene"));
    super::help::tag(
        &pre_scene,
        "dialog.schedule.preScene",
        "Scene to broadcast before the content starts",
    );
    super::help::tag(
        &start_scene,
        "dialog.schedule.startScene",
        "Scene to switch to when the content starts",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = super::ok_button(&panel, &t!("OK"));
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label(&t!("Cancel"))
        .build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&mode, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&date_label, 0, SizerFlag::All, 4);
    sizer.add(&date, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&first_label, 0, SizerFlag::All, 4);
    sizer.add(&first_time, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&second_label, 0, SizerFlag::All, 4);
    sizer.add(&second_time, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&pre_scene_label, 0, SizerFlag::All, 4);
    sizer.add(&pre_scene, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&start_scene_label, 0, SizerFlag::All, 4);
    sizer.add(&start_scene, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // Show/hide rather than enable/disable, following `connect_dialog`: a hidden
    // control leaves the Tab order entirely, where a disabled one stays in the
    // accessibility tree while being unreachable.
    let apply_mode = {
        move || {
            let advanced = mode.get_selection() == ADVANCED;
            // The first picker means something more specific in advanced mode, so
            // its accessible name is re-set alongside its label — without that a
            // screen reader goes on reading the wording it was built with. This
            // does not disturb `picker_acc`, which announces fields from a window
            // subclass and not through the accessible object.
            let first_name = if advanced {
                t!("Pre-stream time")
            } else {
                t!("Time")
            };
            first_label.set_label(&first_name);
            super::set_accessible_name(&first_time, &first_name);
            second_label.show(advanced);
            second_time.show(advanced);
            pre_scene_label.show(advanced);
            pre_scene.show(advanced);
            start_scene_label.show(advanced);
            start_scene.show(advanced);
            panel.layout();
        }
    };
    apply_mode();
    {
        mode.clone().on_selected(move |_| apply_mode());
    }

    let result: Rc<RefCell<Option<Schedule>>> = Rc::new(RefCell::new(None));
    {
        let result = result.clone();
        let scenes = scenes.clone();
        ok.on_click(move |_| {
            let advanced = mode.get_selection() == ADVANCED;
            let Some(connect_at) = at(&date, &first_time) else {
                // No `end_modal`: the warning goes up over a dialog that keeps
                // every choice the user made, so a correction is one edit away.
                super::show_warning(&dialog, &caption(), &does_not_exist());
                first_time.set_focus();
                return;
            };
            let now = crate::mastodon::now_unix();
            if !advanced {
                if let Err(message) = schedule::check_future(connect_at, now) {
                    super::show_warning(&dialog, &caption(), &message);
                    first_time.set_focus();
                    return;
                }
                *result.borrow_mut() = Some(Schedule {
                    connect_at,
                    advanced: None,
                    connect_fired: false,
                });
                dialog.end_modal(ID_OK);
                return;
            }
            let Some(raw_switch) = at(&date, &second_time) else {
                super::show_warning(&dialog, &caption(), &does_not_exist());
                second_time.set_focus();
                return;
            };
            let (connect_at, switch_at) = match schedule::resolve_times(connect_at, raw_switch, now)
            {
                Ok(times) => times,
                Err(message) => {
                    super::show_warning(&dialog, &caption(), &message);
                    first_time.set_focus();
                    return;
                }
            };
            // Read now and resolved by name at fire time, matching how keybinds
            // and `ScenesConfig::active_scene` address a scene. Re-checked here
            // because the config can be edited while this dialog is open.
            let Some(pre) = chosen(&pre_scene, &scenes) else {
                super::show_warning(&dialog, &caption(), &no_scene());
                pre_scene.set_focus();
                return;
            };
            let Some(start) = chosen(&start_scene, &scenes) else {
                super::show_warning(&dialog, &caption(), &no_scene());
                start_scene.set_focus();
                return;
            };
            *result.borrow_mut() = Some(Schedule {
                connect_at,
                advanced: Some(Advanced {
                    switch_at,
                    pre_scene: pre,
                    start_scene: start,
                }),
                connect_fired: false,
            });
            dialog.end_modal(ID_OK);
        });
    }
    {
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    // Explicit rather than relying on the radio box being the first control
    // created: the spec asks for focus to land on the mode chooser, so it is
    // stated rather than inherited from creation order.
    mode.set_focus();
    dialog.show_modal();
    dialog.destroy();
    result.borrow_mut().take()
}

fn does_not_exist() -> String {
    t!(
        "That date and time does not exist. Check the date, and note that clocks going forward skip an \
         hour."
    )
}
fn no_scene() -> String {
    t!(
        "Choose a scene. If the list is empty, the scene may have been deleted while \
         this window was open."
    )
}

/// The date from `date` and the time from `time` as Unix seconds.
///
/// `DateTime`'s month is 1-based at both ends of the wxdragon API (the crate
/// converts wx's 0-based value itself), which is what `local_to_unix` takes, so
/// the components pass straight through.
fn at(date: &DatePickerCtrl, time: &TimePickerCtrl) -> Option<u64> {
    let day = date.get_value();
    let clock = time.get_value();
    if !day.is_valid() || !clock.is_valid() {
        return None;
    }
    schedule::local_to_unix(
        day.year(),
        day.month(),
        u16::try_from(day.day()).ok()?,
        u16::try_from(clock.hour()).ok()?,
        u16::try_from(clock.minute()).ok()?,
        u16::try_from(clock.second()).ok()?,
    )
}

/// The scene name a `Choice` is sitting on.
fn chosen(choice: &Choice, scenes: &[String]) -> Option<String> {
    let index = usize::try_from(choice.get_selection()?).ok()?;
    scenes.get(index).cloned()
}

/// Abandons an armed schedule at the user's request.
pub fn cancel(app: &Rc<App>) {
    if app.run.borrow_mut().schedule.take().is_none() {
        return;
    }
    app.refresh_stream_ui();
    super::help::announce(&t!("Scheduled stream cancelled"));
}

/// Why an armed schedule could not be honoured.
///
/// An enum rather than a string because the two ends of [`abandon`] want the
/// reason in different languages: the log is English whatever the interface is
/// set to, since users are asked to send it when something goes wrong, while the
/// spoken line is the one thing the broadcaster actually hears. Carried rather
/// than fixed because "the service went away" and "this was hours ago" need
/// different answers, and a broadcaster who was not watching needs to know
/// which happened.
enum Abandoned {
    /// The moment came and went with nobody in front of the machine.
    TooLate { late_by: String },
    /// The streaming service was disconnected after the schedule was armed.
    ServiceGone,
}

impl Abandoned {
    /// The English half, for the log.
    fn logged(&self) -> String {
        match self {
            Abandoned::TooLate { late_by } => {
                format!("it was due {late_by} ago, which is too late to start a broadcast")
            }
            Abandoned::ServiceGone => {
                "Pubsplash is no longer connected to a streaming service".to_string()
            }
        }
    }

    /// The translated half, for the spoken line.
    fn spoken(&self) -> String {
        match self {
            Abandoned::TooLate { late_by } => t!(
                "it was due {late_by} ago, which is too late to start a broadcast",
                late_by = late_by
            ),
            Abandoned::ServiceGone => {
                t!("Pubsplash is no longer connected to a streaming service")
            }
        }
    }
}

/// Abandons an armed schedule because Pubsplash cannot honour it.
///
/// The log and a spoken line, never a modal: this arrives unbidden, which is the
/// test CLAUDE.md sets.
fn abandon(app: &Rc<App>, reason: Abandoned) {
    app.run.borrow_mut().schedule = None;
    app.refresh_stream_ui();
    log::warn!("Scheduled stream cancelled: {}", reason.logged());
    super::help::announce(&t!(
        "Scheduled stream cancelled, {reason}",
        reason = reason.spoken()
    ));
}

/// Acts on the armed schedule, if it is time to. Called once a second.
pub fn pump(app: &Rc<App>) {
    // The borrow is dropped before anything below reaches into wx; see the
    // module header, and `home::refresh`'s note on the same hazard.
    let stage = {
        let run = app.run.borrow();
        let Some(schedule) = run.schedule.as_ref() else {
            return;
        };
        schedule.stage(crate::mastodon::now_unix())
    };
    match stage {
        Stage::WaitingToConnect { .. } | Stage::WaitingToSwitch { .. } => {}
        Stage::Connect { scene } => connect(app, scene),
        Stage::Switch { scene } => switch(app, scene),
        Stage::Stale { late_by } => abandon(
            app,
            Abandoned::TooLate {
                late_by: schedule::format_countdown(late_by.as_secs()),
            },
        ),
        Stage::Done => {
            app.run.borrow_mut().schedule = None;
            app.refresh_stream_ui();
        }
    }
}

/// Fires the connect leg.
fn connect(app: &Rc<App>, scene: Option<String>) {
    // The one preflight that cannot be settled at arm time: the user may have
    // disconnected the service since. Checked here rather than left to
    // `start_streaming`, which would answer with a modal nobody is there to see.
    if app.run.borrow().connected_service.is_none() {
        abandon(app, Abandoned::ServiceGone);
        return;
    }
    // Before the work, so a re-entrant tick cannot start a second stream — and
    // so a stream that fails to start is not retried every second for as long as
    // the app is open.
    {
        let mut run = app.run.borrow_mut();
        match run.schedule.as_mut() {
            Some(schedule) => schedule.connect_fired = true,
            None => return,
        }
        // Simple mode is spent the moment it fires; advanced mode still owes a
        // scene switch, so its schedule stays.
        if run.schedule.as_ref().is_some_and(|s| s.advanced.is_none()) {
            run.schedule = None;
        }
    }
    // The scene first: the encoder must be fed the pre-stream scene from its
    // very first block, or the opening seconds go out on whatever was active.
    if let Some(scene) = scene {
        super::home::switch_to_scene_named(app, &scene);
    }
    super::begin_stream(app);
}

/// Fires the scene switch leg, which is the last thing an advanced schedule owes.
fn switch(app: &Rc<App>, scene: String) {
    // Only if the stream it was meant for is actually up. The connect leg having
    // fired does not mean it succeeded — a stream that failed to start leaves an
    // idle app behind, and switching then would quietly rewrite the user's active
    // scene (which is persisted) for a broadcast that never happened. The failure
    // itself has already been reported by `NetEvent::StreamError`, so this is a
    // log line and nothing more.
    if !app.is_streaming_or_starting() {
        app.run.borrow_mut().schedule = None;
        app.refresh_stream_ui();
        log::info!(
            "Scheduled scene switch to {scene} skipped: the stream it was scheduled for is not \
             running."
        );
        return;
    }
    app.run.borrow_mut().schedule = None;
    // Announces "Scene <name>" itself, which matters here more than on a manual
    // switch: this one happens without the user having pressed anything, and it
    // moves focus if they are parked on a mixer strip.
    super::home::switch_to_scene_named(app, &scene);
    app.refresh_stream_ui();
}

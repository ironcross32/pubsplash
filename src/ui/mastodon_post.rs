//! When Pubsplash posts to Mastodon, and the dialogs around it.
//!
//! Three rules hold everywhere in this file:
//!
//! - **A post needs a live stream.** The URL comes from
//!   [`App::stream_url`](super::App::stream_url), which only answers while the
//!   state is `Live`, so a start-of-stream announcement can only ever be sent
//!   from the `StreamStarted` pump arm — after the stream is created *and*
//!   Icecast *and* the SSE feed are up.
//! - **A post needs the per-stream checkbox**, not just the Preferences one.
//!   The Preferences settings seed the Set stream info dialog; the boxes in that
//!   dialog decide.
//! - **Results never open a modal, and never reach the chat list.** They go to
//!   the log, the way speech failures do. A modal here would land in the middle
//!   of a live broadcast, and the chat list carries what viewers said.
//!
//! The flood gate is not here. It lives in `mastodon::net::post`, below every
//! caller, so that a mistake in the scheduling above cannot reach a timeline.

use super::{App, LastStream, StreamState};
use crate::mastodon::api::Link;
use crate::mastodon::net::{AuthEvent, Occasion};
use crate::mastodon::{self, TemplateKind, TokenContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use wxdragon::prelude::*;

/// A restart inside this window under the same title is a reconnect — the
/// stream never really stopped from a listener's point of view — so nothing is
/// posted and nothing is asked.
const RECONNECT_WINDOW: Duration = Duration::from_secs(60);

/// How often the waiting dialog looks for the authorization to finish.
const AUTH_POLL_MS: u32 = 200;

/// What to do about a stream that has just gone live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAction {
    /// A reconnect: same title, moments after the last one ended.
    Nothing,
    /// A resumption: same title, but long enough ago to be worth mentioning.
    AskAboutResuming,
    /// A new broadcast.
    PostStart,
}

/// The resume rule, kept pure so it can be tested without a stream.
pub fn decide_start(title: &str, last: Option<&LastStream>, now: Instant) -> StartAction {
    let Some(last) = last else {
        return StartAction::PostStart;
    };
    if last.title != title {
        return StartAction::PostStart;
    }
    if now.duration_since(last.ended) < RECONNECT_WINDOW {
        StartAction::Nothing
    } else {
        StartAction::AskAboutResuming
    }
}

/// Builds the token values for the stream that is live right now.
fn context(app: &Rc<App>) -> Option<TokenContext> {
    let url = app.stream_url()?;
    let run = app.run.borrow();
    Some(TokenContext {
        title: run.stream_info.title.clone(),
        description: run.stream_info.description.clone(),
        url,
        tod: mastodon::time_of_day(mastodon::local_hour()).to_string(),
    })
}

/// Queues a post. The only route from this module to the network.
fn dispatch(app: &Rc<App>, template: &str, occasion: Occasion) {
    let Some(ctx) = context(app) else {
        log::warn!(
            "Not posting the {} announcement: no live stream URL",
            occasion.describe()
        );
        return;
    };
    let (instance, token) = {
        let config = app.config.borrow();
        if !config.mastodon.is_linked() {
            return;
        }
        (
            config.mastodon.instance.clone(),
            config.mastodon.access_token.clone(),
        )
    };
    let status = mastodon::compose(template, &ctx);
    // Distinct per stream and per minute, so a retry of the same announcement
    // collapses server-side while a genuinely later one does not.
    let key = format!(
        "pubsplash-{}-{}-{}",
        occasion.describe(),
        ctx.url,
        mastodon::now_unix() / 60
    );
    mastodon::net::post(
        instance,
        token,
        status,
        key,
        occasion,
        app.mastodon_tx.clone(),
    );
}

/// Called from the `NetEvent::StreamStarted` pump arm.
pub fn on_stream_started(app: &Rc<App>) {
    // Arm the still-streaming clock first, so it is anchored to the stream
    // going live rather than to whatever the start-of-stream post did.
    {
        let mut run = app.run.borrow_mut();
        run.next_announcement = if run.stream_info.announce_periodic {
            let minutes = app.config.borrow().mastodon.interval_minutes;
            Some(Instant::now() + Duration::from_secs(u64::from(minutes) * 60))
        } else {
            None
        };
    }
    if !app.config.borrow().mastodon.is_linked() {
        return;
    }
    if !app.run.borrow().stream_info.announce_start {
        return;
    }
    let (title, last) = {
        let run = app.run.borrow();
        (run.stream_info.title.clone(), run.last_stream.clone())
    };
    match decide_start(&title, last.as_ref(), Instant::now()) {
        StartAction::Nothing => {
            log::info!(
                "Not announcing the stream: it restarted under the same title within a minute"
            );
        }
        StartAction::PostStart => {
            let template =
                mastodon::pick(&app.config.borrow().mastodon.templates, TemplateKind::Start);
            dispatch(app, &template.text, Occasion::Start);
        }
        StartAction::AskAboutResuming => ask_about_resuming(app),
    }
}

fn ask_about_resuming(app: &Rc<App>) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let ask = MessageDialog::builder(
        &frame,
        "This stream has the same title as the one that just ended. \
         Would you like to post about resuming it?",
        "Post to Mastodon",
    )
    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
    .build();
    let answer = ask.show_modal();
    ask.destroy();
    if answer != ID_YES {
        return;
    }
    let Some(text) = super::mastodon_templates::prompt_one_shot(&frame) else {
        return;
    };
    dispatch(app, &text, Occasion::Resume);
}

/// Called once a second from the pump timer. Deadline-driven rather than a tick
/// count, so an interval survives ticks missed under a modal dialog.
pub fn maybe_periodic(app: &Rc<App>) {
    let due = {
        let run = app.run.borrow();
        // The clock only runs while a stream is actually live.
        if !matches!(run.stream, StreamState::Live { .. }) {
            return;
        }
        match run.next_announcement {
            Some(deadline) if Instant::now() >= deadline => true,
            _ => false,
        }
    };
    if !due {
        return;
    }
    // Re-armed before the post is attempted, so a failure costs one interval
    // rather than stopping the announcements for the rest of the stream.
    let minutes = app.config.borrow().mastodon.interval_minutes;
    app.run.borrow_mut().next_announcement =
        Some(Instant::now() + Duration::from_secs(u64::from(minutes) * 60));
    if !app.config.borrow().mastodon.is_linked() {
        return;
    }
    let template = mastodon::pick(
        &app.config.borrow().mastodon.templates,
        TemplateKind::Continuation,
    );
    dispatch(app, &template.text, Occasion::Continuation);
}

/// Called from the `StreamEnded` and `StreamError` pump arms.
pub fn on_stream_ended(app: &Rc<App>) {
    let mut run = app.run.borrow_mut();
    run.next_announcement = None;
    run.last_stream = Some(LastStream {
        title: run.stream_info.title.clone(),
        ended: Instant::now(),
    });
}

/// Drains post outcomes into the log. Called from the pump.
///
/// The log rather than the chat list: that list carries what viewers said, and
/// Pubsplash does not report on itself there.
pub fn drain_results(app: &Rc<App>) {
    while let Ok(result) = app.mastodon_rx.try_recv() {
        match &result.result {
            Ok(()) => {
                log::info!(
                    "Posted the {} announcement to Mastodon.",
                    result.occasion.describe()
                );
                app.config.borrow_mut().mastodon.last_post_unix = mastodon::now_unix();
                app.save_config();
            }
            // The gate refusing a post is a Pubsplash problem, not the user's;
            // it is already logged, and there is nothing for them to do.
            Err(mastodon::api::MastodonError::RateLimited) => continue,
            Err(error) => log::warn!(
                "Could not post the {} announcement to Mastodon: {error}",
                result.occasion.describe()
            ),
        }
    }
}

// --- authorization ------------------------------------------------------

/// Runs an authorization to completion, showing a waiting dialog while the user
/// is in their browser. Blocks until it finishes, is cancelled, or times out.
///
/// The waiting dialog polls its own receiver on a `Timer` rather than going
/// through the pump: it runs a nested modal loop, and keeping the whole flow
/// inside one function is what makes "the browser never came back" a local
/// problem rather than a state machine spread across the pump.
pub fn authorize(parent: &Dialog, instance: &str) -> Option<Link> {
    let (tx, rx) = crossbeam_channel::unbounded::<AuthEvent>();
    let cancel = mastodon::oauth::cancel_flag();
    mastodon::net::authorize(instance.to_string(), tx, cancel.clone());

    let dialog = Dialog::builder(parent, "Authorize with Mastodon")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(460, 220)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let status = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .with_value(
            "Contacting the server. Your browser will open so you can sign in \
             and approve Pubsplash.",
        )
        .build();
    super::set_accessible_name(&status, "Authorization status");
    super::help::tag(
        &status,
        "dialog.mastodonAuth.status",
        "Mastodon authorization status",
    );
    // Dismiss-only: Escape and Enter both cancel, which is the only thing this
    // dialog can do on its own.
    let cancel_button = super::dismiss_button(&panel, "Cancel");
    sizer.add(&status, 1, SizerFlag::Expand | SizerFlag::All, 8);
    sizer.add(&cancel_button, 0, SizerFlag::All, 8);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    {
        let dialog = dialog.clone();
        let cancel = cancel.clone();
        cancel_button.on_click(move |_| {
            cancel.store(true, Ordering::Relaxed);
            dialog.end_modal(ID_CANCEL);
        });
    }

    let outcome: Rc<RefCell<Option<Link>>> = Rc::new(RefCell::new(None));
    let failure: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let timer = Timer::new(&dialog);
    {
        let dialog = dialog.clone();
        let status = status.clone();
        let outcome = outcome.clone();
        let failure = failure.clone();
        timer.on_tick(move |_| {
            while let Ok(event) = rx.try_recv() {
                match event {
                    AuthEvent::Open(url) => {
                        // Opened from the UI thread: `ShellExecuteW` wants an
                        // apartment, and this thread already has one.
                        if let Err(e) = super::shell_open(&url) {
                            *failure.borrow_mut() = Some(format!(
                                "Could not open your browser: {e}\r\n\r\nOpen this address by hand:\r\n{url}"
                            ));
                            dialog.end_modal(ID_CANCEL);
                            return;
                        }
                        set_status(
                            &status,
                            "Waiting for you to approve Pubsplash in your browser. \
                             This window closes on its own once you have.",
                        );
                    }
                    AuthEvent::NeedCode(reply) => {
                        set_status(
                            &status,
                            "Your browser is showing an authorization code. \
                             Copy it and paste it into the box that just opened.",
                        );
                        let code = prompt_for_code(&dialog);
                        let _ = reply.send(code);
                    }
                    AuthEvent::Done(Ok(link)) => {
                        *outcome.borrow_mut() = Some(link);
                        dialog.end_modal(ID_OK);
                        return;
                    }
                    AuthEvent::Done(Err(error)) => {
                        if !matches!(error, mastodon::api::MastodonError::Cancelled) {
                            *failure.borrow_mut() = Some(error.to_string());
                        }
                        dialog.end_modal(ID_CANCEL);
                        return;
                    }
                }
            }
        });
    }
    timer.start(AUTH_POLL_MS as i32, false);

    dialog.show_modal();
    // Before `destroy`: a timer whose owner window has been torn down keeps
    // firing into freed memory.
    timer.stop();
    // Whatever happened, the worker must stop waiting on a listener nobody is
    // going to read.
    cancel.store(true, Ordering::Relaxed);
    dialog.destroy();

    if let Some(message) = failure.borrow().clone() {
        super::show_error(parent, "Authorize", &message);
    }
    let link = outcome.borrow().clone();
    link
}

fn set_status(status: &TextCtrl, text: &str) {
    status.set_value(text);
    super::set_accessible_name(status, &format!("Authorization status, {text}"));
    super::help::announce(text);
}

/// The fallback when no loopback listener could be bound.
fn prompt_for_code(parent: &Dialog) -> Option<String> {
    let entry = TextEntryDialog::builder(
        parent,
        "Paste the authorization code your browser is showing.",
        "Authorization code",
    )
    .build();
    let answer = entry.show_modal();
    let value = entry.get_value().unwrap_or_default();
    entry.destroy();
    if answer == ID_OK && !value.trim().is_empty() {
        Some(value.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last(title: &str, ago: Duration) -> LastStream {
        LastStream {
            title: title.to_string(),
            ended: Instant::now() - ago,
        }
    }

    #[test]
    fn the_first_stream_of_a_session_is_announced() {
        assert_eq!(
            decide_start("Show", None, Instant::now()),
            StartAction::PostStart
        );
    }

    #[test]
    fn a_quick_restart_under_the_same_title_says_nothing() {
        let now = Instant::now();
        assert_eq!(
            decide_start("Show", Some(&last("Show", Duration::from_secs(5))), now),
            StartAction::Nothing
        );
        // The boundary belongs to the reconnect side.
        assert_eq!(
            decide_start(
                "Show",
                Some(&last("Show", RECONNECT_WINDOW - Duration::from_millis(1))),
                now
            ),
            StartAction::Nothing
        );
    }

    #[test]
    fn a_later_restart_under_the_same_title_asks() {
        assert_eq!(
            decide_start(
                "Show",
                Some(&last("Show", RECONNECT_WINDOW + Duration::from_secs(1))),
                Instant::now()
            ),
            StartAction::AskAboutResuming
        );
    }

    #[test]
    fn a_different_title_is_a_new_broadcast_however_soon_it_starts() {
        for ago in [Duration::from_secs(1), Duration::from_secs(3600)] {
            assert_eq!(
                decide_start("Tuesday", Some(&last("Monday", ago)), Instant::now()),
                StartAction::PostStart,
                "after {ago:?}"
            );
        }
    }
}

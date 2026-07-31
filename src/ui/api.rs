//! API tab: what each speech engine has been asked to do this session.
//!
//! A list, not a read-only text box and not a web view. The rows are rewritten
//! as speech happens, and rewriting a `TextCtrl` resets its caret so the arrow
//! keys never reach the end of it — the same trap the Home tab's overview was
//! moved out of. A `ListBox` is also what every other list in this app is, so
//! it goes through [`super::native_acc`] and joins the F6 pane ring for free.
//!
//! Only engines actually used appear, most recently used first, and the numbers
//! cover this session only — nothing here is persisted. Where a provider
//! publishes no such datum the row says `unavailable` rather than showing a
//! zero, because "0 credits left" and "this provider does not report credits"
//! are very different things to read out.

use super::{App, list};
use crate::tts::usage::{self, EngineUsage};
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown while nothing has spoken yet. See [`super::list`] — a `ListBox` with
/// no rows is announced as "Unknown".
const NO_USAGE: &str = "No engines used yet";

/// What a provider does not tell us. One spelling everywhere, so a screen
/// reader user learns the word once.
const UNAVAILABLE: &str = "unavailable";

/// The indent that marks a row as belonging to the engine heading above it.
/// A `ListBox` has no headings, so the structure has to live in the text.
const INDENT: &str = "    ";

/// Which datum a row carries. Rows are addressed by kind rather than by index
/// because engines join the list and reorder as they are used, and the refresh
/// below has to know whether a given row is the same row it wrote last time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApiField {
    /// The engine's name — the "heading" row.
    Heading,
    Requests,
    Characters,
    Credits,
    Balance,
    Models,
    Voices,
    Failures,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ApiRow {
    pub engine: &'static str,
    pub field: ApiField,
}

/// Whether a row the user has selected may be rewritten. Same rule, and the
/// same reason, as [`super::home`]'s: on MSW `set_string` deletes and reinserts
/// the item, and a screen reader announces that for the *selected* row whether
/// or not the list has focus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    /// Leave it alone: this refresh was not prompted by the user.
    Skip,
    /// Write it: the row is about to be read out anyway.
    Write,
}

pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, Button) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let list_label = StaticText::builder(panel)
        .with_label("API usage this session")
        .build();
    let usage_list = ListBox::builder(panel).build();
    // Nothing has spoken when the window opens, so seed the placeholder rather
    // than leaving the list empty.
    list::fill(&usage_list, &[], NO_USAGE);
    super::native_acc::install(&usage_list, "API usage this session");
    super::help::tag(
        &usage_list,
        "tab.api.usageList",
        "Per-engine API usage list",
    );

    // ALT+F. Mnemonics are dialog-wide, not per notebook page, so this dodges
    // every letter Home, Chat, Scenes and Buses already claim.
    let refresh_button = Button::builder(panel)
        .with_label("Re&fresh balances")
        .build();
    super::help::tag(
        &refresh_button,
        "tab.api.refreshButton",
        "Refresh account balances button",
    );

    sizer.add(&list_label, 0, SizerFlag::All, 4);
    sizer.add(&usage_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&refresh_button, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);

    // The rows are never rewritten while selected (see `refresh`), so they are
    // brought up to date at the two moments the user is about to hear one.
    //
    // Arrowing within the list: the row just vacated is no longer selected, so
    // it updates at once. The row being moved *onto* is still skipped, so this
    // cannot talk over it.
    {
        let app = app.clone();
        usage_list
            .clone()
            .on_selection_changed(move |_| refresh_usage(&app));
    }
    // Focus arriving on the list: whatever row it lands on is about to be read
    // out, so this is the one refresh allowed to write the selected row.
    {
        let app = app.clone();
        usage_list.clone().on_set_focus(move |event| {
            refresh(&app, Selected::Write);
            event.skip(true);
        });
    }

    {
        let app = app.clone();
        refresh_button.clone().on_click(move |_| refresh_balances(&app));
    }

    (usage_list, refresh_button)
}

/// Asks every used provider that publishes a balance what is left.
///
/// On demand rather than on a timer: this spends the user's API quota, and a
/// tab that quietly called a paid endpoint every few minutes would be a poor
/// thing to put in front of someone watching their spend.
fn refresh_balances(app: &Rc<App>) {
    let speech = app.config.borrow().speech.clone();
    let any = usage::snapshot()
        .iter()
        .any(|entry| usage::reports_balance(entry.engine));
    if !any {
        // Silence would read as a broken button. Say why nothing happened, in
        // the place the user already reads — never a modal.
        app.run.borrow_mut().chat.push(super::ChatEntry::new(
            "Speech".into(),
            "No engine used this session reports a credit balance.".into(),
        ));
        super::chat::append_new_messages(app, 1);
        return;
    }
    usage::start_balance_refresh(speech, app.usage_tx.clone());
}

/// Reports balance lookups that failed. Drained by the pump.
///
/// Returns how many chat entries were added, so the caller refreshes the list —
/// the same contract as `super::report_speech_problems`.
pub fn report_balance_failures(app: &Rc<App>) -> usize {
    let mut added = 0;
    while let Ok(failure) = app.usage_rx.try_recv() {
        app.run.borrow_mut().chat.push(super::ChatEntry::new(
            "Speech".into(),
            format!(
                "Could not read the {} balance: {}",
                crate::tts::engines::display_name(failure.engine),
                failure.error
            ),
        ));
        added += 1;
    }
    added
}

/// Brings the usage list up to date without disturbing the selected row.
pub fn refresh_usage(app: &App) {
    refresh(app, Selected::Skip);
}

fn refresh(app: &App, selected_rows: Selected) {
    let mut rows = usage_rows(&usage::snapshot(), &|engine, voice| {
        crate::tts::cached_voice_label(engine, voice)
    });

    let Some(list) = app.widgets(|w| w.usage_list) else {
        return;
    };
    // Nothing below touches a widget while `run` is borrowed: the list's own
    // selection handler calls back into here, and a borrow held across a
    // `set_string` or `set_selection` would be a panic waiting to happen.
    let mut shown = app.run.borrow().shown.api.clone();

    let selected = list.get_selection();

    // The common case: the same engines, one of their tallies moved on.
    if shown.len() == rows.len() && shown.iter().zip(&rows).all(|((a, _), (b, _))| a == b) {
        for (index, (_, label)) in rows.into_iter().enumerate() {
            if shown[index].1 == label {
                continue;
            }
            if selected_rows == Selected::Skip && selected == Some(index as u32) {
                // Leave the cache stale too, so this is retried the moment the
                // row stops being the selected one.
                continue;
            }
            list.set_string(index as u32, &label);
            shown[index].1 = label;
        }
    } else {
        // The row set changed: an engine spoke for the first time, or the order
        // moved. Refilling drops the selection, so put it back on the same
        // *row kind* rather than the same index — a user parked on ElevenLabs'
        // voice list would otherwise land on whatever now occupies that slot.
        let was_on = selected
            .and_then(|index| shown.get(index as usize))
            .map(|(row, _)| *row);
        let labels: Vec<String> = rows.iter().map(|(_, label)| label.clone()).collect();
        list::fill(&list, &labels, NO_USAGE);
        std::mem::swap(&mut shown, &mut rows);
        if let Some(index) = was_on.and_then(|row| shown.iter().position(|(r, _)| *r == row)) {
            list.set_selection(index as u32, true);
        }
    }

    app.run.borrow_mut().shown.api = shown;
}

/// Builds every row, in display order.
///
/// Pure over the snapshot so it can be tested without widgets, exactly as
/// `home::overview_rows` is. `label` resolves a voice id to its display name —
/// ElevenLabs' ids are opaque keys, and the catalog is what turns
/// `21m00Tcm4TlvDq8ikWAM` into `Rachel`. Resolving here rather than at record
/// time means a later catalog refresh improves rows already collected.
fn usage_rows(
    usage: &[EngineUsage],
    label: &dyn Fn(&str, &str) -> Option<String>,
) -> Vec<(ApiRow, String)> {
    let mut rows = Vec::new();
    for entry in usage {
        let engine = entry.engine;
        let mut push = |field, text: String| rows.push((ApiRow { engine, field }, text));

        push(ApiField::Heading, entry.display_name().to_string());
        push(
            ApiField::Requests,
            format!("{INDENT}Requests sent: {}", thousands(entry.requests)),
        );
        push(
            ApiField::Characters,
            format!("{INDENT}Characters sent: {}", thousands(entry.characters)),
        );
        push(
            ApiField::Credits,
            format!("{INDENT}Credits spent: {}", credits_spent(entry)),
        );
        push(
            ApiField::Balance,
            format!("{INDENT}Remaining balance: {}", balance_text(entry)),
        );
        push(
            ApiField::Models,
            format!("{INDENT}Models used: {}", join_or_unavailable(&entry.models)),
        );
        let voices: Vec<String> = entry
            .voices
            .iter()
            .map(|id| label(engine, id).unwrap_or_else(|| id.clone()))
            .collect();
        push(
            ApiField::Voices,
            format!("{INDENT}Voices used: {}", join_or_unavailable(&voices)),
        );
        push(
            ApiField::Failures,
            format!("{INDENT}Failures: {}", thousands(entry.failures)),
        );
    }
    rows
}

/// What this session cost, in the provider's own unit.
///
/// Only meaningful where the provider bills per character *and* publishes the
/// allowance in the same unit, which today means ElevenLabs alone: one credit
/// per character, so the session's character tally is its credit tally. For
/// everyone else the app has no exchange rate and says so.
fn credits_spent(entry: &EngineUsage) -> String {
    if usage::reports_balance(entry.engine) {
        thousands(entry.characters)
    } else {
        UNAVAILABLE.to_string()
    }
}

fn balance_text(entry: &EngineUsage) -> String {
    match &entry.balance {
        Some(balance) => {
            let core = format!(
                "{} of {}",
                thousands(balance.remaining()),
                thousands(balance.limit)
            );
            if balance.tier.is_empty() {
                core
            } else {
                format!("{core} ({})", balance.tier)
            }
        }
        // Distinguishing these two matters: one is a button the user has not
        // pressed yet, the other is a provider that will never answer.
        None if usage::reports_balance(entry.engine) => {
            "not fetched, press Refresh balances".to_string()
        }
        None => UNAVAILABLE.to_string(),
    }
}

fn join_or_unavailable<'a>(values: impl IntoIterator<Item = &'a String>) -> String {
    let joined = values
        .into_iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        UNAVAILABLE.to_string()
    } else {
        joined
    }
}

/// Groups digits in threes. Character counts run to six figures on a busy
/// stream, and a screen reader reads a grouped number as a number rather than
/// as a digit sequence.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tts::engines;
    use crate::tts::usage::Balance;
    use std::collections::BTreeSet;

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn entry(engine: &'static str) -> EngineUsage {
        EngineUsage {
            engine,
            requests: 17,
            characters: 1_204,
            failures: 0,
            models: set(&["eleven_multilingual_v2"]),
            voices: set(&["21m00Tcm4TlvDq8ikWAM"]),
            last_used: 1,
            balance: None,
        }
    }

    /// The rows for one engine, without the leading indent.
    fn texts(usage: &[EngineUsage]) -> Vec<String> {
        usage_rows(usage, &|_, _| None)
            .into_iter()
            .map(|(_, text)| text.trim_start().to_string())
            .collect()
    }

    #[test]
    fn every_engine_gets_a_heading_and_the_same_seven_fields() {
        let rows = usage_rows(&[entry(engines::OPENAI)], &|_, _| None);
        assert_eq!(rows.len(), 8);
        assert_eq!(rows[0].1, "OpenAI");
        assert_eq!(rows[0].0.field, ApiField::Heading);
        let fields: Vec<ApiField> = rows.iter().map(|(row, _)| row.field).collect();
        assert_eq!(
            fields,
            vec![
                ApiField::Heading,
                ApiField::Requests,
                ApiField::Characters,
                ApiField::Credits,
                ApiField::Balance,
                ApiField::Models,
                ApiField::Voices,
                ApiField::Failures,
            ]
        );
    }

    #[test]
    fn fields_are_indented_under_their_engine_heading() {
        let rows = usage_rows(&[entry(engines::OPENAI)], &|_, _| None);
        assert!(!rows[0].1.starts_with(' '), "the heading is not indented");
        for (_, text) in &rows[1..] {
            assert!(text.starts_with(INDENT), "{text:?} should be indented");
        }
    }

    #[test]
    fn a_provider_with_no_credit_data_says_unavailable() {
        let rows = texts(&[entry(engines::OPENAI)]);
        assert!(rows.contains(&"Credits spent: unavailable".to_string()));
        assert!(rows.contains(&"Remaining balance: unavailable".to_string()));
        // The locally countable figures are still real.
        assert!(rows.contains(&"Requests sent: 17".to_string()));
        assert!(rows.contains(&"Characters sent: 1,204".to_string()));
    }

    #[test]
    fn elevenlabs_reports_credits_and_points_at_the_button_before_a_fetch() {
        let rows = texts(&[entry(engines::ELEVENLABS)]);
        assert!(rows.contains(&"Credits spent: 1,204".to_string()));
        assert!(rows.contains(
            &"Remaining balance: not fetched, press Refresh balances".to_string()
        ));
    }

    #[test]
    fn a_fetched_balance_is_shown_as_remaining_of_limit_with_the_tier() {
        let mut usage = entry(engines::ELEVENLABS);
        usage.balance = Some(Balance {
            used: 1_204,
            limit: 10_000,
            tier: "creator".into(),
            resets_unix: None,
        });
        let rows = texts(&[usage]);
        assert!(rows.contains(&"Remaining balance: 8,796 of 10,000 (creator)".to_string()));
    }

    #[test]
    fn a_balance_with_no_tier_omits_the_parenthesis() {
        let mut usage = entry(engines::ELEVENLABS);
        usage.balance = Some(Balance {
            used: 0,
            limit: 500,
            ..Balance::default()
        });
        let rows = texts(&[usage]);
        assert!(rows.contains(&"Remaining balance: 500 of 500".to_string()));
    }

    #[test]
    fn an_engine_with_no_models_recorded_says_unavailable() {
        let mut usage = entry(engines::AZURE);
        usage.models = BTreeSet::new();
        usage.voices = BTreeSet::new();
        let rows = texts(&[usage]);
        assert!(rows.contains(&"Models used: unavailable".to_string()));
        assert!(rows.contains(&"Voices used: unavailable".to_string()));
    }

    #[test]
    fn opaque_voice_ids_are_shown_as_names_when_the_catalog_knows_them() {
        let rows = usage_rows(&[entry(engines::ELEVENLABS)], &|_, id| {
            (id == "21m00Tcm4TlvDq8ikWAM").then(|| "Rachel".to_string())
        });
        assert!(rows.iter().any(|(_, text)| text.contains("Voices used: Rachel")));
    }

    #[test]
    fn an_unknown_voice_id_falls_back_to_the_id_rather_than_vanishing() {
        let rows = texts(&[entry(engines::ELEVENLABS)]);
        assert!(
            rows.iter()
                .any(|text| text.contains("Voices used: 21m00Tcm4TlvDq8ikWAM"))
        );
    }

    #[test]
    fn engines_appear_in_snapshot_order_and_rows_carry_their_engine() {
        let mut first = entry(engines::SAPI);
        first.last_used = 2;
        let mut second = entry(engines::OPENAI);
        second.last_used = 1;
        let rows = usage_rows(&[first, second], &|_, _| None);
        assert_eq!(rows[0].0.engine, engines::SAPI);
        assert_eq!(rows[8].0.engine, engines::OPENAI);
    }

    #[test]
    fn an_empty_session_produces_no_rows_at_all() {
        assert!(usage_rows(&[], &|_, _| None).is_empty());
    }

    #[test]
    fn digits_are_grouped_in_threes() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(17), "17");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_204), "1,204");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}

//! Mastodon announcements: templates, tokens, and the rules around them.
//!
//! This module is deliberately free of I/O so the parts that are easy to get
//! wrong — brace parsing, the token table, the "there is always a template"
//! guarantee — are unit-testable without a network or a UI. The HTTP lives in
//! [`api`], the OAuth dance in [`oauth`], and the worker thread that runs both
//! in [`net`].
//!
//! Two rules here are policy rather than convenience and are not configurable:
//!
//! - **Every post Pubsplash makes ends with [`HASHTAG`].** [`compose`] is the
//!   only way a status string is built, and it appends the tag last and
//!   unconditionally, so anyone who wants to filter automated posts out of their
//!   timeline can. A template that already contains the tag does not get a
//!   second copy.
//! - **[`MIN_POST_INTERVAL`] between posts.** That gate is enforced down in
//!   [`net::post`], not by any caller, so a bug in future scheduling code cannot
//!   turn into a flood on someone else's timeline.

pub mod api;
pub mod net;
pub mod oauth;

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Appended to every post, always. See the module header.
pub const HASHTAG: &str = "#PubsplashStreamInfo";

/// The floor between two successful posts. Not user-alterable.
pub const MIN_POST_INTERVAL: Duration = Duration::from_secs(30);

/// The scopes we ask for: posting, and reading the account so the Preferences
/// tab can show which account is linked.
pub const SCOPES: &str = "read:accounts write:statuses";

/// Which moment a template is for.
///
/// The variant order is the sort order of the templates list — every start-of-
/// stream template groups above every continuation one — so `derive(Ord)` and
/// the declaration order here are load-bearing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    #[default]
    Start,
    Continuation,
}

impl TemplateKind {
    /// The name shown in the templates list and the radio box.
    pub fn label(self) -> &'static str {
        match self {
            TemplateKind::Start => "Start of stream",
            TemplateKind::Continuation => "Stream continuation",
        }
    }

    pub const ALL: [TemplateKind; 2] = [TemplateKind::Start, TemplateKind::Continuation];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Template {
    pub kind: TemplateKind,
    pub text: String,
}

impl Template {
    pub fn new(kind: TemplateKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }

    /// The row shown in the Preferences list: kind, a colon, then the text.
    pub fn list_label(&self) -> String {
        format!("{}: {}", self.kind.label(), self.text)
    }
}

pub const DEFAULT_START: &str = "I'm live on #Audiopub right now! Catch the stream at {url}";
pub const DEFAULT_CONTINUATION: &str =
    "I'm still streaming on #Audiopub, catch the stream at {url}";

/// The built-in template for `kind`.
///
/// The user can delete every template they have written; this is what stands in
/// when they do, so the announcement paths have no failure mode to handle.
pub fn fallback(kind: TemplateKind) -> Template {
    Template::new(
        kind,
        match kind {
            TemplateKind::Start => DEFAULT_START,
            TemplateKind::Continuation => DEFAULT_CONTINUATION,
        },
    )
}

/// Chooses a template of `kind`, at random when the user has written several.
///
/// **The only way any caller obtains a template.** An empty selection is not an
/// error — it yields [`fallback`] — which is what makes "the user deleted all
/// their templates" a non-event rather than a panic.
pub fn pick(templates: &[Template], kind: TemplateKind) -> Template {
    let matching: Vec<&Template> = templates
        .iter()
        .filter(|t| t.kind == kind && !t.text.trim().is_empty())
        .collect();
    match matching.len() {
        0 => fallback(kind),
        1 => matching[0].clone(),
        n => {
            use rand::Rng;
            matching[rand::thread_rng().gen_range(0..n)].clone()
        }
    }
}

/// Sorts a template list the way the Preferences list shows it: by kind, then
/// alphabetically within a kind. Called on add, edit and remove — never on a
/// timer, because rebuilding a list a screen reader is sitting in makes it talk.
pub fn sort(templates: &mut [Template]) {
    templates.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.text.cmp(&b.text)));
}

/// Every token, with the description the Help dialog shows.
///
/// One table drives validation, expansion and the help text, so a token cannot
/// be accepted by the parser but left unexpanded, or documented but unknown.
pub const TOKENS: &[(&str, &str)] = &[
    ("title", "The title of the stream."),
    ("description", "The description of the stream."),
    ("url", "The web address listeners use to tune in."),
    (
        "tod",
        "The time of day where you are: morning, afternoon, evening, or night.",
    ),
];

pub fn is_token(name: &str) -> bool {
    TOKENS.iter().any(|(token, _)| *token == name)
}

/// What the user gets told when a template will not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    Empty,
    UnknownToken(String),
    EmptyToken,
    UnclosedBrace,
    UnexpectedClosingBrace,
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateError::Empty => f.write_str("The template is empty. Type something to post."),
            TemplateError::UnknownToken(name) => write!(
                f,
                "{{{name}}} is not a token Pubsplash knows. \
                 Press Help for the list of tokens you can use."
            ),
            TemplateError::EmptyToken => f.write_str(
                "There is an empty pair of braces, {}, in the template. \
                 Put a token name between them, or remove them.",
            ),
            TemplateError::UnclosedBrace => f.write_str(
                "A token was opened with { but never closed with }. \
                 Every token has to be written like {url}.",
            ),
            TemplateError::UnexpectedClosingBrace => f.write_str(
                "There is a } in the template with no matching { before it. \
                 Every token has to be written like {url}.",
            ),
        }
    }
}

/// Checks that every `{...}` in `text` names a real token and that the braces
/// balance.
///
/// There is no escape sequence, so a literal brace is simply an error; the Help
/// dialog says so. That keeps the parser one left-to-right pass with no state
/// worth misreading.
pub fn validate(text: &str) -> Result<(), TemplateError> {
    if text.trim().is_empty() {
        return Err(TemplateError::Empty);
    }
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        // A `}` before the next `{` is a stray closer.
        if rest[..open].contains('}') {
            return Err(TemplateError::UnexpectedClosingBrace);
        }
        let after = &rest[open + 1..];
        let Some(end) = after.find('}') else {
            return Err(TemplateError::UnclosedBrace);
        };
        let name = &after[..end];
        // A `{` inside the braces means the first one was never closed.
        if name.contains('{') {
            return Err(TemplateError::UnclosedBrace);
        }
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(TemplateError::EmptyToken);
        }
        if !is_token(trimmed) {
            return Err(TemplateError::UnknownToken(trimmed.to_string()));
        }
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(TemplateError::UnexpectedClosingBrace);
    }
    Ok(())
}

/// The values a template's tokens expand to.
#[derive(Debug, Clone, Default)]
pub struct TokenContext {
    pub title: String,
    pub description: String,
    pub url: String,
    pub tod: String,
}

impl TokenContext {
    fn value(&self, token: &str) -> &str {
        match token {
            "title" => &self.title,
            "description" => &self.description,
            "url" => &self.url,
            "tod" => &self.tod,
            _ => "",
        }
    }
}

/// Substitutes every known token in `text`.
///
/// Unknown tokens are left alone rather than blanked: validation already
/// refuses them at the point the user types them, and if one ever survives into
/// a stored template, showing `{whatever}` in the post is a far better clue than
/// a hole in the sentence.
pub fn expand(text: &str, ctx: &TokenContext) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..end].trim();
        if is_token(name) {
            out.push_str(ctx.value(name));
        } else {
            out.push('{');
            out.push_str(&after[..end]);
            out.push('}');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Builds the status text that actually goes to Mastodon.
///
/// The single choke point for [`HASHTAG`] — see the module header.
pub fn compose(template: &str, ctx: &TokenContext) -> String {
    let body = expand(template, ctx).trim().to_string();
    if body
        .split_whitespace()
        .any(|word| word.eq_ignore_ascii_case(HASHTAG))
    {
        return body;
    }
    if body.is_empty() {
        return HASHTAG.to_string();
    }
    format!("{body} {HASHTAG}")
}

/// The word `{tod}` expands to. Takes the hour so it can be tested.
pub fn time_of_day(hour: u32) -> &'static str {
    match hour {
        5..=11 => "morning",
        12..=16 => "afternoon",
        17..=20 => "evening",
        _ => "night",
    }
}

/// The local hour, 0-23.
///
/// `GetLocalTime` rather than a date crate: the tree has neither `chrono` nor
/// `time`, and this is the only wall-clock reading in the app.
pub fn local_hour() -> u32 {
    // SAFETY: `GetLocalTime` only writes the `SYSTEMTIME` it is handed.
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    u32::from(now.wHour)
}

/// Seconds since the Unix epoch, for the "last posted" stamp in the config.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The still-streaming intervals offered by the Preferences dropdown, in
/// minutes, with their labels. The stored value is the number of minutes, so
/// re-ordering or adding a row does not invalidate anyone's settings.
pub const INTERVALS: &[(u32, &str)] = &[
    (60, "Every hour"),
    (90, "Every one-and-a-half hours"),
    (120, "Every two hours"),
    (150, "Every two-and-a-half hours"),
    (180, "Every three hours"),
    (300, "Every 5 hours"),
];

pub const DEFAULT_INTERVAL_MINUTES: u32 = 60;

/// Snaps a stored interval to the nearest offered one, so a hand-edited or
/// future-version config cannot leave the dropdown with nothing selected.
pub fn clamp_interval(minutes: u32) -> u32 {
    INTERVALS
        .iter()
        .map(|(m, _)| *m)
        .min_by_key(|m| m.abs_diff(minutes))
        .unwrap_or(DEFAULT_INTERVAL_MINUTES)
}

/// The body of the Help dialog, generated from [`TOKENS`] so it cannot drift.
pub fn help_text() -> String {
    let mut out = String::from(
        "Tokens are written inside curly braces. When Pubsplash posts, each one \
         is replaced with the real value.\r\n\r\n",
    );
    for (token, description) in TOKENS {
        out.push_str(&format!("{{{token}}}\t{description}\r\n"));
    }
    out.push_str(&format!(
        "\r\nBraces have no other meaning, so a {{ or a }} that is not part of a \
         token is an error.\r\n\r\nEvery post Pubsplash makes ends with \
         {HASHTAG}, whether or not you type it. That lets anyone who wants to \
         filter automated posts out of their timeline do so, and it cannot be \
         turned off.\r\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TokenContext {
        TokenContext {
            title: "Tuesday hangout".into(),
            description: "Chatting".into(),
            url: "https://audiopub.site/live/abc123".into(),
            tod: "evening".into(),
        }
    }

    #[test]
    fn every_token_expands() {
        assert_eq!(
            expand("{title} / {description} / {url} / {tod}", &ctx()),
            "Tuesday hangout / Chatting / https://audiopub.site/live/abc123 / evening"
        );
    }

    #[test]
    fn every_documented_token_is_expandable() {
        // The table drives validation, expansion and the help text; this is the
        // test that keeps them from drifting apart.
        for (token, _) in TOKENS {
            let text = format!("x{{{token}}}y");
            assert_eq!(validate(&text), Ok(()), "{token} should validate");
            assert_ne!(expand(&text, &ctx()), text, "{token} should expand");
            assert!(help_text().contains(&format!("{{{token}}}")), "{token}");
        }
    }

    #[test]
    fn unknown_tokens_are_named_in_the_error() {
        let error = validate("hello {bogus} there").unwrap_err();
        assert_eq!(error, TemplateError::UnknownToken("bogus".into()));
        assert!(error.to_string().contains("{bogus}"), "{error}");
    }

    #[test]
    fn brace_errors_are_distinguished() {
        assert_eq!(validate("a {url"), Err(TemplateError::UnclosedBrace));
        assert_eq!(validate("a {url{title}"), Err(TemplateError::UnclosedBrace));
        assert_eq!(
            validate("a url}"),
            Err(TemplateError::UnexpectedClosingBrace)
        );
        assert_eq!(
            validate("a } {url}"),
            Err(TemplateError::UnexpectedClosingBrace)
        );
        assert_eq!(validate("a {} b"), Err(TemplateError::EmptyToken));
        assert_eq!(validate("   "), Err(TemplateError::Empty));
    }

    #[test]
    fn a_template_with_no_tokens_is_fine() {
        assert_eq!(validate("Just some words."), Ok(()));
        assert_eq!(expand("Just some words.", &ctx()), "Just some words.");
    }

    #[test]
    fn unknown_tokens_survive_expansion_visibly() {
        assert_eq!(expand("a {bogus} b", &ctx()), "a {bogus} b");
        assert_eq!(expand("a {url b", &ctx()), "a {url b");
    }

    #[test]
    fn the_hashtag_is_always_appended() {
        let posted = compose("Live now at {url}", &ctx());
        assert!(posted.ends_with(HASHTAG), "{posted}");
        assert_eq!(
            posted,
            format!("Live now at https://audiopub.site/live/abc123 {HASHTAG}")
        );
        // Even an all-whitespace template still carries the tag.
        assert_eq!(compose("   ", &ctx()), HASHTAG);
        // And the defaults do.
        for kind in TemplateKind::ALL {
            assert!(compose(&fallback(kind).text, &ctx()).ends_with(HASHTAG));
        }
    }

    #[test]
    fn the_hashtag_is_not_doubled() {
        let posted = compose(&format!("Live now {HASHTAG}"), &ctx());
        assert_eq!(posted, format!("Live now {HASHTAG}"));
        assert_eq!(posted.matches(HASHTAG).count(), 1);
    }

    #[test]
    fn the_defaults_are_valid_templates() {
        assert_eq!(validate(DEFAULT_START), Ok(()));
        assert_eq!(validate(DEFAULT_CONTINUATION), Ok(()));
    }

    #[test]
    fn picking_from_an_empty_list_falls_back() {
        // The whole point: the user can delete every template and nothing breaks.
        for kind in TemplateKind::ALL {
            assert_eq!(pick(&[], kind), fallback(kind));
        }
        // A list holding only the other kind is just as empty for our purposes.
        let only_start = [Template::new(TemplateKind::Start, "mine")];
        assert_eq!(
            pick(&only_start, TemplateKind::Continuation),
            fallback(TemplateKind::Continuation)
        );
        assert_eq!(pick(&only_start, TemplateKind::Start).text, "mine");
        // A blank template does not count as one.
        let blank = [Template::new(TemplateKind::Start, "   ")];
        assert_eq!(
            pick(&blank, TemplateKind::Start),
            fallback(TemplateKind::Start)
        );
    }

    #[test]
    fn picking_stays_within_the_requested_kind() {
        let templates = vec![
            Template::new(TemplateKind::Start, "a"),
            Template::new(TemplateKind::Start, "b"),
            Template::new(TemplateKind::Continuation, "c"),
        ];
        for _ in 0..50 {
            assert_eq!(pick(&templates, TemplateKind::Continuation).text, "c");
            assert!(["a", "b"].contains(&pick(&templates, TemplateKind::Start).text.as_str()));
        }
    }

    #[test]
    fn sorting_groups_starts_before_continuations() {
        let mut templates = vec![
            Template::new(TemplateKind::Continuation, "zebra"),
            Template::new(TemplateKind::Start, "beta"),
            Template::new(TemplateKind::Continuation, "alpha"),
            Template::new(TemplateKind::Start, "alpha"),
        ];
        sort(&mut templates);
        let rows: Vec<String> = templates.iter().map(|t| t.list_label()).collect();
        assert_eq!(
            rows,
            [
                "Start of stream: alpha",
                "Start of stream: beta",
                "Stream continuation: alpha",
                "Stream continuation: zebra",
            ]
        );
    }

    #[test]
    fn time_of_day_covers_every_hour() {
        assert_eq!(time_of_day(0), "night");
        assert_eq!(time_of_day(4), "night");
        assert_eq!(time_of_day(5), "morning");
        assert_eq!(time_of_day(11), "morning");
        assert_eq!(time_of_day(12), "afternoon");
        assert_eq!(time_of_day(16), "afternoon");
        assert_eq!(time_of_day(17), "evening");
        assert_eq!(time_of_day(20), "evening");
        assert_eq!(time_of_day(21), "night");
        assert_eq!(time_of_day(23), "night");
        for hour in 0..24 {
            assert!(
                ["morning", "afternoon", "evening", "night"].contains(&time_of_day(hour)),
                "hour {hour}"
            );
        }
    }

    #[test]
    fn intervals_snap_to_an_offered_value() {
        for (minutes, _) in INTERVALS {
            assert_eq!(clamp_interval(*minutes), *minutes);
        }
        assert_eq!(clamp_interval(0), 60);
        assert_eq!(clamp_interval(u32::MAX), 300);
        assert!(INTERVALS.iter().any(|(m, _)| *m == clamp_interval(77)));
    }
}

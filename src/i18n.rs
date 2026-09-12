//! UI localization: one embedded gettext catalog per language, looked up by the
//! English source text.
//!
//! Three deliberate choices, because each of them is load-bearing and the
//! obvious alternative is worse for *this* app:
//!
//! 1. **The msgid is the English string itself, not an invented key.** Pubsplash
//!    is used almost entirely by screen-reader users, and a partially translated
//!    build is the normal state of affairs for a long time after a language is
//!    added. With English as the key, a string nobody has translated yet reads
//!    out as correct English; with invented keys, it reads out as
//!    `prefs-audio-output-device`, which is worse than not translating at all.
//!    Every `t!` therefore degrades to its own argument, and that is a property
//!    of the design rather than a fallback path someone has to remember to add.
//!
//! 2. **The catalog is a `.po` parsed here, not a `.mo` loaded by wxWidgets.**
//!    wxdragon does expose the whole `wxTranslations` API, and using it was the
//!    first plan, but it wants compiled `.mo` files on disk under
//!    `locale/<lang>/LC_MESSAGES/`. That means a build step (`msgfmt`, which is
//!    not on a stock Windows box), files to add to the packager's resource list,
//!    and two install layouts to get right — see `data_dir` — for a feature
//!    whose failure mode is a silently English UI. Embedding the `.po` with
//!    `include_str!` and parsing it once costs a few milliseconds at startup,
//!    ships nothing, and behaves identically in a portable copy and an
//!    installed one. It also lets us support `msgctxt`, which `wxTranslations`
//!    does not expose and which [`translate_ctx`] needs. Note that the vendored
//!    wxWidgets ships no message catalogs of its own either, so
//!    `add_std_catalog()` would have found nothing to load.
//!
//! 3. **Log lines are never translated.** Users are asked to send their log when
//!    something goes wrong, and a log in a language the maintainer cannot read
//!    is not a diagnostic. Nothing inside a `log::` macro goes through `t!`.
//!    The same goes for the identity names in `SourceConfig.name`, which route
//!    external feeds and speech requests — see `source_name`, which is where the
//!    *displayed* form of a source is built and where translation belongs.
//!
//! The catalog is written once during [`init`] and only read afterwards, so
//! [`t!`] is callable from any thread — the TTS workers and the media player
//! reach it as readily as the UI thread does.
//!
//! Adding a language is a `.po` file under `po/`, a row in [`LANGUAGES`], and a
//! line in [`CATALOGS`]. `cargo run --bin gen-po` refreshes every `.po` from the
//! source and from `help.toml` without disturbing existing translations.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The languages offered in Preferences, in the order they are listed there.
///
/// The label is deliberately in the language itself: someone who has landed in
/// a UI they cannot read needs to find their own language in the list, and
/// "Spanish" does not help them do that.
pub const LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("es", "Español")];

/// The embedded catalogs. English is absent by construction — it is what the
/// msgids already are, so there is nothing to look up.
const CATALOGS: &[(&str, &str)] = &[("es", include_str!("../po/es.po"))];

/// The active catalog, or `None` when running in English. Written once by
/// [`init`] before any UI exists and read-only from then on.
static CATALOG: OnceLock<Option<Catalog>> = OnceLock::new();

/// A parsed `.po`: every entry keyed by its context and msgid, plus the plural
/// rule from the header.
struct Catalog {
    /// `(msgctxt, msgid) -> [msgstr, msgstr_plural...]`. An entry always has at
    /// least one string; a singular-only entry has exactly one.
    entries: HashMap<(Option<String>, String), Vec<String>>,
    /// The `plural=` expression from the `Plural-Forms:` header, pre-parsed.
    plural: PluralRule,
}

/// Resolves the language to use and parses its catalog. Call once, early in
/// `main` — before the UI is built, and after logging is up so the choice is
/// visible in the log.
///
/// `configured` is the user's setting: `None` or an empty string means "follow
/// Windows", which is the default. A configured language with no catalog (or a
/// system language we do not ship) leaves the UI in English rather than failing.
pub fn init(configured: Option<&str>) {
    let wanted = match configured.map(str::trim).filter(|s| !s.is_empty()) {
        Some(explicit) => explicit.to_string(),
        None => {
            let system = system_language();
            log::info!("No language configured; following Windows, which reports {system}");
            system
        }
    };
    let resolved = best_match(&wanted);
    let catalog = match resolved {
        None => {
            // Either English was asked for, or a language we do not ship. Both
            // mean the msgids stand as they are, which is a correct UI.
            if wanted != "en" && !wanted.starts_with("en-") && !wanted.starts_with("en_") {
                log::info!("No catalog for {wanted}; the interface stays in English");
            }
            None
        }
        Some((code, source)) => match Catalog::parse(source) {
            Ok(catalog) => {
                log::info!(
                    "Loaded the {code} interface catalog: {} translated messages",
                    catalog.entries.len()
                );
                Some(catalog)
            }
            Err(e) => {
                // A malformed catalog is a build-time mistake, but it must not
                // cost the user their app: English is always a working UI.
                log::error!("Could not parse the {code} catalog, staying in English: {e}");
                None
            }
        },
    };
    if CATALOG.set(catalog).is_err() {
        log::warn!("i18n::init called twice; the first language stands");
    }
}

/// The language tag Windows reports for the user's interface, such as `es-ES`.
/// Falls back to `en` if the call fails, which leaves the UI in English.
fn system_language() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if len <= 0 {
        return "en".to_string();
    }
    // The count includes the terminating null.
    String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)])
}

/// Picks the catalog for a language tag, matching the base language when the
/// exact region is not shipped — `es-AR`, `es_MX` and `es` all land on `es`,
/// which is the whole point of shipping one Spanish rather than twenty.
fn best_match(tag: &str) -> Option<(&'static str, &'static str)> {
    let lower = tag.trim().to_ascii_lowercase().replace('_', "-");
    let base = lower.split('-').next().unwrap_or("").to_string();
    CATALOGS
        .iter()
        .find(|(code, _)| *code == lower || *code == base)
        .map(|(code, source)| (*code, *source))
}

fn catalog() -> Option<&'static Catalog> {
    // `get()` rather than `get_or_init`: a `t!` that runs before `init` (a very
    // early error path) should read as English, not race a second resolution.
    CATALOG.get().and_then(|c| c.as_ref())
}

// --- lookup ----------------------------------------------------------------

/// Translates one message. Returns `msgid` unchanged when there is no catalog
/// or no entry for it, which is what makes an untranslated build read correctly.
pub fn translate(msgid: &str) -> String {
    lookup(None, msgid).unwrap_or(msgid).to_string()
}

/// Translates a message carrying a `msgctxt`. Used for the F1 help, where the
/// context is the control's help-id: it disambiguates two controls whose English
/// text happens to match, and — more usefully — tells the translator which
/// control a paragraph of prose belongs to. See `ui::help`.
pub fn translate_ctx(context: &str, msgid: &str) -> String {
    lookup(Some(context), msgid)
        // A translator working from a `.pot` may have dropped the context, and
        // an entry that matches the text is still the right translation.
        .or_else(|| lookup(None, msgid))
        .unwrap_or(msgid)
        .to_string()
}

/// Translates a message with singular and plural forms, choosing by `n`
/// according to the catalog's own plural rule.
pub fn translate_plural(singular: &str, plural: &str, n: i64) -> String {
    let Some(catalog) = catalog() else {
        return if n == 1 { singular } else { plural }.to_string();
    };
    let key = (None, singular.to_string());
    match catalog.entries.get(&key) {
        Some(forms) => {
            let index = catalog.plural.index(n);
            // A catalog with fewer forms than the rule selects is malformed;
            // take the last rather than panicking on a user's machine.
            let chosen = forms.get(index).or_else(|| forms.last());
            match chosen.filter(|s| !s.is_empty()) {
                Some(s) => s.clone(),
                None => if n == 1 { singular } else { plural }.to_string(),
            }
        }
        None => if n == 1 { singular } else { plural }.to_string(),
    }
}

fn lookup(context: Option<&str>, msgid: &str) -> Option<&'static str> {
    let catalog = catalog()?;
    let key = (context.map(str::to_string), msgid.to_string());
    catalog
        .entries
        .get(&key)
        .and_then(|forms| forms.first())
        // An empty msgstr is gettext for "not translated", not for "translates
        // to nothing" — a `.po` fresh out of the extractor is entirely empty
        // strings, and they must all fall through to the English.
        .filter(|s| !s.is_empty())
        .map(|s| s.as_str())
}

// --- interpolation ---------------------------------------------------------

/// Substitutes `{name}` placeholders in an already-translated string.
///
/// This exists because `format!` cannot help us: its template must be a
/// literal, and by this point the template has come out of a catalog. Named
/// rather than positional placeholders so a translator can reorder them —
/// Spanish routinely puts the pieces of a sentence in a different order from
/// English, and a positional `{}` would make that impossible to express.
///
/// `{{` and `}}` are literal braces, as in `format!`. A placeholder naming an
/// argument that was not supplied is left standing rather than dropped, so a
/// typo in a translation shows up as visible text instead of a silent hole.
pub fn interpolate(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find(['{', '}']) {
        let (before, tail) = rest.split_at(open);
        out.push_str(before);
        let mut chars = tail.chars();
        let brace = chars.next().unwrap_or('{');
        if chars.clone().next() == Some(brace) {
            // `{{` or `}}` — one literal brace, and skip both.
            out.push(brace);
            rest = &tail[brace.len_utf8() * 2..];
            continue;
        }
        if brace == '}' {
            // An unmatched `}`; pass it through rather than guessing.
            out.push('}');
            rest = &tail[1..];
            continue;
        }
        match tail.find('}') {
            Some(close) => {
                let name = &tail[1..close];
                match args.iter().find(|(k, _)| *k == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    None => out.push_str(&tail[..=close]),
                }
                rest = &tail[close + 1..];
            }
            None => {
                // No closing brace at all: the remainder is literal text.
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

// --- macros ----------------------------------------------------------------

/// Translates a literal English message, optionally interpolating named
/// arguments into it.
///
/// ```ignore
/// let label = t!("Output device");
/// let msg = t!("Streaming at {rate} kbps", rate = 128);
/// ```
///
/// The message must be a literal: `gen-po` finds the strings to translate by
/// scanning for these call sites, so a `t!(some_variable)` would be invisible
/// to the extractor and silently never translated.
#[macro_export]
macro_rules! t {
    ($msgid:literal $(,)?) => {
        $crate::i18n::translate($msgid)
    };
    ($msgid:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::interpolate(
            &$crate::i18n::translate($msgid),
            &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

/// Translates a message that has a plural form, choosing by `n`.
///
/// `n` is bound as a placeholder automatically, so both forms can use `{n}`;
/// further named arguments may follow.
///
/// ```ignore
/// let line = tn!("{n} listener", "{n} listeners", count);
/// ```
#[macro_export]
macro_rules! tn {
    ($singular:literal, $plural:literal, $n:expr $(,)?) => {{
        let n = $n;
        $crate::i18n::interpolate(
            &$crate::i18n::translate_plural($singular, $plural, n as i64),
            &[("n", &n as &dyn ::std::fmt::Display)],
        )
    }};
    ($singular:literal, $plural:literal, $n:expr, $($name:ident = $value:expr),+ $(,)?) => {{
        let n = $n;
        $crate::i18n::interpolate(
            &$crate::i18n::translate_plural($singular, $plural, n as i64),
            &[
                ("n", &n as &dyn ::std::fmt::Display),
                $((stringify!($name), &$value as &dyn ::std::fmt::Display)),+
            ],
        )
    }};
}

// --- plural rules ----------------------------------------------------------

/// The `plural=` expression from a catalog's `Plural-Forms:` header, kept as a
/// tiny expression tree.
///
/// Spanish needs only `n != 1`, which hardly justifies an evaluator — but the
/// header is the one place a `.po` states its own rule, and hard-coding
/// "English rules for everyone" is the bug that makes the first Slavic or Celtic
/// translation quietly wrong in a way nobody reviewing the `.po` would see. The
/// grammar below is the whole of what gettext allows in that expression, so a
/// future catalog is a file rather than a code change.
struct PluralRule {
    expr: Option<PluralExpr>,
}

impl PluralRule {
    /// The `n != 1` default, used when a catalog has no parsable header.
    fn default_rule() -> Self {
        Self { expr: None }
    }

    fn index(&self, n: i64) -> usize {
        match &self.expr {
            Some(expr) => expr.eval(n).max(0) as usize,
            None => usize::from(n != 1),
        }
    }
}

#[derive(Debug, Clone)]
enum PluralExpr {
    N,
    Num(i64),
    Binary(Box<PluralExpr>, PluralOp, Box<PluralExpr>),
    Ternary(Box<PluralExpr>, Box<PluralExpr>, Box<PluralExpr>),
    Not(Box<PluralExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PluralOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Mod,
}

impl PluralExpr {
    fn eval(&self, n: i64) -> i64 {
        match self {
            PluralExpr::N => n,
            PluralExpr::Num(v) => *v,
            PluralExpr::Not(inner) => i64::from(inner.eval(n) == 0),
            PluralExpr::Ternary(cond, yes, no) => {
                if cond.eval(n) != 0 {
                    yes.eval(n)
                } else {
                    no.eval(n)
                }
            }
            PluralExpr::Binary(lhs, op, rhs) => {
                let a = lhs.eval(n);
                // Short-circuit, so a `&&` guarding a modulo cannot divide by
                // zero through the right-hand side.
                match op {
                    PluralOp::Or => return i64::from(a != 0 || rhs.eval(n) != 0),
                    PluralOp::And => return i64::from(a != 0 && rhs.eval(n) != 0),
                    _ => {}
                }
                let b = rhs.eval(n);
                match op {
                    PluralOp::Eq => i64::from(a == b),
                    PluralOp::Ne => i64::from(a != b),
                    PluralOp::Lt => i64::from(a < b),
                    PluralOp::Gt => i64::from(a > b),
                    PluralOp::Le => i64::from(a <= b),
                    PluralOp::Ge => i64::from(a >= b),
                    PluralOp::Mod => {
                        if b == 0 {
                            0
                        } else {
                            a % b
                        }
                    }
                    PluralOp::Or | PluralOp::And => unreachable!("handled above"),
                }
            }
        }
    }
}

/// A recursive-descent parser for the gettext plural expression grammar,
/// lowest-precedence first: `?:`, `||`, `&&`, comparison, equality, `%`.
mod plural_parse {
    use super::{PluralExpr, PluralOp};

    pub fn parse(src: &str) -> Option<PluralExpr> {
        let tokens = lex(src)?;
        let mut pos = 0;
        let expr = ternary(&tokens, &mut pos)?;
        if pos == tokens.len() {
            Some(expr)
        } else {
            None
        }
    }

    #[derive(Debug, PartialEq, Clone)]
    enum Tok {
        N,
        Num(i64),
        Op(&'static str),
        LParen,
        RParen,
        Question,
        Colon,
    }

    fn lex(src: &str) -> Option<Vec<Tok>> {
        let bytes: Vec<char> = src.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            match c {
                ' ' | '\t' | '\n' | '\r' => i += 1,
                'n' => {
                    out.push(Tok::N);
                    i += 1;
                }
                '0'..='9' => {
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    let text: String = bytes[start..i].iter().collect();
                    out.push(Tok::Num(text.parse().ok()?));
                }
                '(' => {
                    out.push(Tok::LParen);
                    i += 1;
                }
                ')' => {
                    out.push(Tok::RParen);
                    i += 1;
                }
                '?' => {
                    out.push(Tok::Question);
                    i += 1;
                }
                ':' => {
                    out.push(Tok::Colon);
                    i += 1;
                }
                '%' => {
                    out.push(Tok::Op("%"));
                    i += 1;
                }
                '!' | '=' | '<' | '>' | '&' | '|' => {
                    let two: String = bytes[i..(i + 2).min(bytes.len())].iter().collect();
                    let op = match two.as_str() {
                        "==" => Some("=="),
                        "!=" => Some("!="),
                        "<=" => Some("<="),
                        ">=" => Some(">="),
                        "&&" => Some("&&"),
                        "||" => Some("||"),
                        _ => None,
                    };
                    match op {
                        Some(op) => {
                            out.push(Tok::Op(op));
                            i += 2;
                        }
                        None => match c {
                            '<' => {
                                out.push(Tok::Op("<"));
                                i += 1;
                            }
                            '>' => {
                                out.push(Tok::Op(">"));
                                i += 1;
                            }
                            '!' => {
                                out.push(Tok::Op("!"));
                                i += 1;
                            }
                            // A bare `&`, `|` or `=` is not in the grammar.
                            _ => return None,
                        },
                    }
                }
                _ => return None,
            }
        }
        Some(out)
    }

    fn eat(tokens: &[Tok], pos: &mut usize, want: &Tok) -> bool {
        if tokens.get(*pos) == Some(want) {
            *pos += 1;
            true
        } else {
            false
        }
    }

    fn ternary(tokens: &[Tok], pos: &mut usize) -> Option<PluralExpr> {
        let cond = binary(tokens, pos, 0)?;
        if eat(tokens, pos, &Tok::Question) {
            let yes = ternary(tokens, pos)?;
            if !eat(tokens, pos, &Tok::Colon) {
                return None;
            }
            let no = ternary(tokens, pos)?;
            return Some(PluralExpr::Ternary(
                Box::new(cond),
                Box::new(yes),
                Box::new(no),
            ));
        }
        Some(cond)
    }

    /// Binding power per level, loosest first.
    const LEVELS: &[&[(&str, PluralOp)]] = &[
        &[("||", PluralOp::Or)],
        &[("&&", PluralOp::And)],
        &[("==", PluralOp::Eq), ("!=", PluralOp::Ne)],
        &[
            ("<", PluralOp::Lt),
            (">", PluralOp::Gt),
            ("<=", PluralOp::Le),
            (">=", PluralOp::Ge),
        ],
        &[("%", PluralOp::Mod)],
    ];

    fn binary(tokens: &[Tok], pos: &mut usize, level: usize) -> Option<PluralExpr> {
        if level >= LEVELS.len() {
            return unary(tokens, pos);
        }
        let mut lhs = binary(tokens, pos, level + 1)?;
        loop {
            let Some(Tok::Op(text)) = tokens.get(*pos) else {
                break;
            };
            let Some((_, op)) = LEVELS[level].iter().find(|(t, _)| t == text) else {
                break;
            };
            *pos += 1;
            let rhs = binary(tokens, pos, level + 1)?;
            lhs = PluralExpr::Binary(Box::new(lhs), *op, Box::new(rhs));
        }
        Some(lhs)
    }

    fn unary(tokens: &[Tok], pos: &mut usize) -> Option<PluralExpr> {
        if tokens.get(*pos) == Some(&Tok::Op("!")) {
            *pos += 1;
            return Some(PluralExpr::Not(Box::new(unary(tokens, pos)?)));
        }
        match tokens.get(*pos)? {
            Tok::N => {
                *pos += 1;
                Some(PluralExpr::N)
            }
            Tok::Num(v) => {
                let v = *v;
                *pos += 1;
                Some(PluralExpr::Num(v))
            }
            Tok::LParen => {
                *pos += 1;
                let inner = ternary(tokens, pos)?;
                if eat(tokens, pos, &Tok::RParen) {
                    Some(inner)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

// --- .po parsing -----------------------------------------------------------
//
// [`parse_po`] is public, and deliberately so: `src/bin/gen_po.rs` reads the
// existing catalogs with it (by `#[path]`-including this file, the same trick
// `audio/convert.rs` uses for the standalone soundpack binaries), so the writer
// and the reader of a `.po` can never drift apart. That is also why
// [`po_escape`] lives here beside [`unquote`] — the round-trip test below is
// only writable where both halves are.

/// One entry of a `.po`, as it appears in the file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoEntry {
    /// The `msgctxt`, if the entry has one.
    pub ctxt: Option<String>,
    /// The `msgid`. Empty for the header entry.
    pub id: String,
    /// The `msgid_plural`, if the entry has plural forms.
    pub id_plural: Option<String>,
    /// The `msgstr`, or the `msgstr[n]` forms in order. Empty strings are
    /// gettext for "not translated" and are preserved as such.
    pub msgstrs: Vec<String>,
}

/// Parses a `.po` into its entries, in file order.
///
/// Deliberately tolerant: a line it cannot make sense of is skipped rather than
/// failing the file, because one bad entry must not cost the user every other
/// translation in the catalog. Obsolete `#~` entries are skipped wholesale —
/// they are the extractor's memory, not translations.
// `flush!` resets `current` on every path, including the last one before the
// loop ends, where nothing reads it again. That reset is what keeps the macro
// correct wherever it is called from, so it stays.
#[allow(unused_assignments)]
pub fn parse_po(source: &str) -> Vec<PoEntry> {
    /// Which buffer a bare continuation line `"..."` appends to.
    #[derive(Clone, Copy, PartialEq)]
    enum Field {
        Ctxt,
        Id,
        IdPlural,
        Str(usize),
    }

    let mut out: Vec<PoEntry> = Vec::new();
    let mut entry = PoEntry::default();
    let mut started = false;
    let mut current: Option<Field> = None;

    // An entry ends at a blank line or at the next `msgctxt`/`msgid` that is
    // not continuing the one in hand.
    macro_rules! flush {
        () => {
            if started {
                out.push(std::mem::take(&mut entry));
            } else {
                entry = PoEntry::default();
            }
            started = false;
            current = None;
        };
    }

    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() {
            flush!();
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("msgctxt ") {
            flush!();
            entry.ctxt = Some(unquote(rest));
            started = true;
            current = Some(Field::Ctxt);
        } else if let Some(rest) = line.strip_prefix("msgid_plural ") {
            entry.id_plural = Some(unquote(rest));
            current = Some(Field::IdPlural);
        } else if let Some(rest) = line.strip_prefix("msgid ") {
            // A `msgid` directly after a `msgctxt` continues that entry; one
            // anywhere else starts a fresh one.
            if current != Some(Field::Ctxt) {
                flush!();
            }
            entry.id = unquote(rest);
            started = true;
            current = Some(Field::Id);
        } else if let Some(rest) = line.strip_prefix("msgstr[") {
            let Some((index, text)) = rest.split_once(']') else {
                continue;
            };
            let Ok(index) = index.trim().parse::<usize>() else {
                continue;
            };
            if entry.msgstrs.len() <= index {
                entry.msgstrs.resize(index + 1, String::new());
            }
            entry.msgstrs[index] = unquote(text.trim());
            current = Some(Field::Str(index));
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            if entry.msgstrs.is_empty() {
                entry.msgstrs.push(String::new());
            }
            entry.msgstrs[0] = unquote(rest);
            current = Some(Field::Str(0));
        } else if line.starts_with('"') {
            let text = unquote(line);
            match current {
                Some(Field::Ctxt) => entry.ctxt.get_or_insert_default().push_str(&text),
                Some(Field::Id) => entry.id.push_str(&text),
                Some(Field::IdPlural) => entry.id_plural.get_or_insert_default().push_str(&text),
                Some(Field::Str(i)) => {
                    if let Some(slot) = entry.msgstrs.get_mut(i) {
                        slot.push_str(&text);
                    }
                }
                None => {}
            }
        }
    }
    flush!();
    out
}

impl Catalog {
    /// Folds a parsed `.po` into the lookup map, taking the plural rule from
    /// the header entry (the one whose msgid is empty).
    fn parse(source: &str) -> Result<Self, String> {
        let mut entries: HashMap<(Option<String>, String), Vec<String>> = HashMap::new();
        let mut plural = PluralRule::default_rule();

        for entry in parse_po(source) {
            if entry.id.is_empty() {
                if let Some(header) = entry.msgstrs.first()
                    && let Some(rule) = parse_plural_header(header)
                {
                    plural = rule;
                }
                continue;
            }
            if entry.msgstrs.is_empty() {
                continue;
            }
            entries.insert((entry.ctxt, entry.id), entry.msgstrs);
        }

        if entries.is_empty() {
            return Err("no translated entries".to_string());
        }
        Ok(Catalog { entries, plural })
    }
}

/// Pulls the `plural=` expression out of a `Plural-Forms:` header value.
fn parse_plural_header(header: &str) -> Option<PluralRule> {
    let line = header
        .lines()
        .find(|l| l.trim_start().starts_with("Plural-Forms:"))?;
    let value = line.split_once(':')?.1;
    let expr = value
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("plural="))?;
    plural_parse::parse(expr).map(|expr| PluralRule { expr: Some(expr) })
}

/// Strips the surrounding quotes from a `.po` string and unescapes it.
fn unquote(text: &str) -> String {
    let text = text.trim();
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Escapes a string for writing into a `.po`. Used by the `gen-po` tool, which
/// is why it lives here beside [`unquote`] rather than in the binary — the two
/// have to agree, and a round-trip test can only be written where both are.
/// Only `gen-po` calls this. It lives here rather than in that binary so the
/// round-trip test below can hold it against [`unquote`], its inverse — this
/// file is `#[path]`-included into three binaries besides the app, and it is
/// dead in all of them but one.
#[allow(dead_code)]
pub fn po_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips() {
        for original in [
            "plain",
            "with \"quotes\"",
            "a\nnewline",
            "a\\backslash",
            "tab\there",
        ] {
            let written = format!("\"{}\"", po_escape(original));
            assert_eq!(unquote(&written), original, "round trip of {original:?}");
        }
    }

    #[test]
    fn interpolation_substitutes_named_arguments() {
        let args: &[(&str, &dyn std::fmt::Display)] = &[("rate", &128), ("name", &"mic")];
        assert_eq!(
            interpolate("{name} at {rate} kbps", args),
            "mic at 128 kbps"
        );
    }

    #[test]
    fn interpolation_lets_a_translation_reorder_the_placeholders() {
        // The reason placeholders are named: Spanish does not keep English's
        // order, and a positional `{}` could not express this at all.
        let args: &[(&str, &dyn std::fmt::Display)] = &[("rate", &128), ("name", &"mic")];
        assert_eq!(
            interpolate("a {rate} kbps desde {name}", args),
            "a 128 kbps desde mic"
        );
    }

    #[test]
    fn interpolation_keeps_literal_braces_and_unknown_names() {
        let args: &[(&str, &dyn std::fmt::Display)] = &[("known", &1)];
        assert_eq!(interpolate("{{literal}}", args), "{literal}");
        // Left standing so a mistyped placeholder is visible rather than a hole.
        assert_eq!(interpolate("{unknown}", args), "{unknown}");
        assert_eq!(interpolate("no placeholders", args), "no placeholders");
        assert_eq!(interpolate("unclosed {oops", args), "unclosed {oops");
    }

    #[test]
    fn a_catalog_parses_entries_contexts_and_plurals() {
        let po = r#"
msgid ""
msgstr ""
"Plural-Forms: nplurals=2; plural=(n != 1);\n"

msgid "Output device"
msgstr "Dispositivo de salida"

msgctxt "dialog.preferences.audio.outputDevice"
msgid "Pick a device."
msgstr "Elija un dispositivo."

msgid "{n} listener"
msgid_plural "{n} listeners"
msgstr[0] "{n} oyente"
msgstr[1] "{n} oyentes"
"#;
        let catalog = Catalog::parse(po).expect("parses");
        assert_eq!(
            catalog
                .entries
                .get(&(None, "Output device".to_string()))
                .map(|v| v[0].as_str()),
            Some("Dispositivo de salida")
        );
        assert_eq!(
            catalog
                .entries
                .get(&(
                    Some("dialog.preferences.audio.outputDevice".to_string()),
                    "Pick a device.".to_string()
                ))
                .map(|v| v[0].as_str()),
            Some("Elija un dispositivo.")
        );
        let listeners = catalog
            .entries
            .get(&(None, "{n} listener".to_string()))
            .expect("plural entry");
        assert_eq!(listeners, &["{n} oyente", "{n} oyentes"]);
        assert_eq!(catalog.plural.index(1), 0);
        assert_eq!(catalog.plural.index(2), 1);
        assert_eq!(catalog.plural.index(0), 1);
    }

    #[test]
    fn a_multi_line_entry_is_joined() {
        // Long help paragraphs are wrapped across lines by every `.po` editor,
        // so this is the normal shape of the biggest entries in our catalog.
        let po = "msgid \"\"\nmsgstr \"\"\n\nmsgid \"\"\n\"one \"\n\"two\"\nmsgstr \"\"\n\"uno \"\n\"dos\"\n";
        let catalog = Catalog::parse(po).expect("parses");
        assert_eq!(
            catalog
                .entries
                .get(&(None, "one two".to_string()))
                .map(|v| v[0].as_str()),
            Some("uno dos")
        );
    }

    #[test]
    fn an_untranslated_entry_falls_through_to_english() {
        // A freshly extracted `.po` is entirely empty msgstrs. Every one of them
        // has to read as the English it came from, not as an empty label.
        let po = "msgid \"\"\nmsgstr \"\"\n\nmsgid \"real\"\nmsgstr \"real-es\"\n\nmsgid \"Close\"\nmsgstr \"\"\n";
        let catalog = Catalog::parse(po).expect("parses");
        assert!(catalog.entries.contains_key(&(None, "Close".to_string())));
        let forms = &catalog.entries[&(None, "Close".to_string())];
        assert!(forms[0].is_empty(), "stored, but empty");
    }

    #[test]
    fn plural_rules_come_from_the_header() {
        // Spanish and English agree, so this test uses rules that do not, to
        // prove the header is actually consulted.
        let french = plural_parse::parse("(n > 1)").expect("parses");
        assert_eq!(PluralRule { expr: Some(french) }.index(0), 0);

        let polish = plural_parse::parse(
            "(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2)",
        )
        .expect("parses");
        let rule = PluralRule { expr: Some(polish) };
        assert_eq!(rule.index(1), 0);
        assert_eq!(rule.index(3), 1);
        assert_eq!(rule.index(5), 2);
        assert_eq!(rule.index(22), 1);
        assert_eq!(rule.index(112), 2);
    }

    #[test]
    fn an_unparsable_plural_rule_falls_back_to_the_english_one() {
        assert!(plural_parse::parse("n ~ 1").is_none());
        assert_eq!(PluralRule::default_rule().index(1), 0);
        assert_eq!(PluralRule::default_rule().index(7), 1);
    }

    #[test]
    fn best_match_falls_back_to_the_base_language() {
        // The reason we ship one Spanish rather than twenty: every regional tag
        // has to find it.
        for tag in ["es", "es-ES", "es_MX", "es-419", "ES-ar"] {
            assert_eq!(best_match(tag).map(|(c, _)| c), Some("es"), "tag {tag}");
        }
        assert_eq!(best_match("en-GB").map(|(c, _)| c), None);
        assert_eq!(best_match("ja").map(|(c, _)| c), None);
    }

    #[test]
    fn the_shipped_catalogs_all_parse() {
        // The one test that would have caught a hand-edited `.po` going in
        // broken: `init` only logs a parse failure, and the app would come up
        // silently English.
        for (code, source) in CATALOGS {
            let catalog = Catalog::parse(source)
                .unwrap_or_else(|e| panic!("the {code} catalog does not parse: {e}"));
            assert!(
                catalog.entries.len() > 100,
                "the {code} catalog has only {} entries, which suggests a truncated file",
                catalog.entries.len()
            );
        }
    }

    #[test]
    fn a_shipped_translation_is_actually_reachable() {
        // End to end through the real Spanish catalog: parse it, look a message
        // up, and interpolate. A test that only parses would still pass if the
        // lookup key were subtly wrong.
        let catalog = Catalog::parse(CATALOGS.iter().find(|(c, _)| *c == "es").unwrap().1)
            .expect("the Spanish catalog parses");
        let forms = catalog
            .entries
            .get(&(None, "Start streaming".to_string()))
            .expect("a message every user sees");
        assert_eq!(forms[0], "Iniciar la emisión");

        // And one carrying a placeholder, through `interpolate`.
        let listeners = &catalog.entries[&(None, "Listeners: {count}".to_string())][0];
        let args: &[(&str, &dyn std::fmt::Display)] = &[("count", &12)];
        assert_eq!(interpolate(listeners, args), "Oyentes: 12");
    }

    #[test]
    fn a_context_help_message_is_only_reachable_with_its_context() {
        // The property that let the F1 help go untranslated while nothing looked
        // wrong: `gen-po` files every `help.toml` message under the control's
        // help-id as its `msgctxt`, so the catalog carries all 228 of them and
        // the binary embeds all of them -- and a context-free `translate` finds
        // none of them. `ui::help::message_for_stamp` must therefore ask through
        // `translate_ctx`. If that is ever changed back, F1 reads English out on
        // a translated install and no test but this one would say so.
        let catalog = Catalog::parse(CATALOGS.iter().find(|(c, _)| *c == "es").unwrap().1)
            .expect("the Spanish catalog parses");
        let id = "dialog.appPicker.refresh";
        let english = "Look for applications again. Use this after starting an application, or after playing something in one, so that it shows up in the list.";

        assert!(
            catalog.entries.get(&(None, english.to_string())).is_none(),
            "a context-keyed message must not be reachable without its context"
        );
        let translated = catalog
            .entries
            .get(&(Some(id.to_string()), english.to_string()))
            .expect("the help message is filed under its help-id");
        assert!(
            translated[0].starts_with("Vuelve a buscar"),
            "unexpected translation: {}",
            translated[0]
        );
    }

    #[test]
    fn no_translation_loses_or_invents_a_placeholder() {
        // The one translation mistake that cannot be caught by reading the
        // result: a dropped `{name}` still reads as a sentence, and the device,
        // file or URL it was carrying simply never appears. An invented one is
        // as bad the other way, since `interpolate` leaves an unknown name
        // standing and the user is read the braces. `gen-po` checks this too,
        // but that is a dev tool nobody is obliged to run.
        fn names(text: &str) -> std::collections::BTreeSet<String> {
            let mut out = std::collections::BTreeSet::new();
            let chars: Vec<char> = text.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                if chars[i] != '{' {
                    i += 1;
                    continue;
                }
                if chars.get(i + 1) == Some(&'{') {
                    i += 2;
                    continue;
                }
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '}' {
                    j += 1;
                }
                let name: String = chars[start..j.min(chars.len())].iter().collect();
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    out.insert(name);
                }
                i = j + 1;
            }
            out
        }

        for (code, source) in CATALOGS {
            for entry in parse_po(source) {
                if entry.id.is_empty() {
                    continue;
                }
                let mut wanted = names(&entry.id);
                if let Some(plural) = &entry.id_plural {
                    wanted.extend(names(plural));
                }
                for form in entry.msgstrs.iter().filter(|s| !s.is_empty()) {
                    assert_eq!(
                        names(form),
                        wanted,
                        "{code}: placeholders differ for {:?}",
                        entry.id
                    );
                }
            }
        }
    }

    #[test]
    fn every_language_offered_has_a_catalog_or_is_english() {
        // Preferences lists `LANGUAGES`; picking one with no catalog would look
        // like the setting had been ignored.
        for (code, _) in LANGUAGES {
            assert!(
                *code == "en" || CATALOGS.iter().any(|(c, _)| c == code),
                "{code} is offered in Preferences but has no catalog"
            );
        }
    }
}

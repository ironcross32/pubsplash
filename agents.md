# Instructions for AI agents

This file defines standard procedures every AI agent must follow when working on Pubsplash.

## Keep the changelog up to date

`changelog.md` must be updated in the same change set as the code it describes.

- Every completed item gets its own bullet point.
- Keep entries concise and to the point; don't include **why** something was done, just **that** it was done
- Entries go under the permanent `## Unreleased` major heading until a release is cut; each released version gets its own `## <version>` major heading.
- Under each major heading, place entries in exactly one of the three subheadings: `### Additions`, `### Fixes`, or `### Changes`.
- When a release is tagged, rename `Unreleased`'s content to the new version heading and recreate an empty `Unreleased` section above it.

## Documentation

Do not make edits, additions, or modifications to the README unless instructed to do so. Instead, place proposed changes into a file called proposed_doc.md which does not get checked into the repo. If you've written to this document before in the same session, append to it, otherwise, overwrite its contents.

## No new mnemonics in the UI

Do not add an `&` mnemonic to a new or existing control, and do not reintroduce one when editing a label, unless the user explicitly asks for it in that change set.

- Every control must be reachable and operable from the keyboard by Tab/arrow navigation; that is what accessibility requires here, not an ALT chord.
- Deliberate keyboard shortcuts belong in the user keybind system (`keybind.rs` / `ui/keybinds.rs`) or as a menu accelerator after the `\t` in a menu item's label (e.g. `"Preferences...\tCtrl+,"`), never as a mnemonic.
- A **literal** ampersand in a label still has to be doubled (`"Logging && debugging"`), because wx parses labels for mnemonics regardless. That is an escape, not a mnemonic.
- **23 labels do carry a mnemonic today**, on the Buses and Scenes tabs, the mixer strip's context menu, the Media Player source dialog, the Setup streaming services dialog, and the Speech tab's Validate button. They predate this rule and are left alone. Do not add to them, and do not strip them out as a drive-by either — removing one changes a shortcut somebody may be using, so it wants its own change set and the user's say-so.
- A mnemonic that survives into a translation **must stay unique within its own dialog**. Translating moves the letter: "Folder"/"Calibrate" are F and C in English but both C in Spanish. Check the whole dialog, not just the label you changed.

## Translations

The interface is translated. `src/i18n.rs` holds the machinery and its header explains the design; these are the rules a change set has to follow.

**Every string a user can see or hear goes through `t!` or `tn!`.** That includes labels, dialog titles and messages, list rows, accessible names installed by hand, spoken announcements, and any `Err(String)` that reaches a message box.

- The argument must be a **literal**. `cargo run --bin gen-po` finds translatable text by scanning for these call sites, so `t!(some_variable)` is invisible to it and would silently never be translated.
- Use `tn!(singular, plural, n)` for anything counted. Do not build a plural by appending an "s" — most languages do not work that way.
- Placeholders are named (`{path}`, not `{}`) so a translator can reorder them, which Spanish routinely needs to.
- Compose whole phrases rather than slotting a word into a frame. `"{base} and recording"` beats `"{base} and {word}"`: languages agree the second half with the first, and a lone word gives the translator nothing to agree with.

**These are deliberately never translated**, and each has bitten once:

- **Log lines.** Users are asked to send their log when something goes wrong; a log the maintainer cannot read is not a diagnostic. `chat_feed_line`, `server_state_line` and `audio_link_line` in `ui/mod.rs` exist only to be logged and must stay English, even though they read like user-facing prose.
- **`SourceConfig.name` and `SourceKindConfig::type_display_name`.** That name is an identity key — it routes `ExternalFeeds` and keys TTS speech requests — so a source created in Spanish would not match the same source created in English. What the user actually sees is built by `source_name`, which *is* translated.
- **Both arguments of `help::tag`.** The first is a key into `help.toml`; the second is a note for whoever writes that file.
- Panic and `expect` messages, protocol strings, config keys, URLs, and file paths.

**Never read a widget's text back and compare it to an English literal.** Key on the selection index instead. `scenes::model_choice_value` used to test a combo box against `"Provider default"`, which stops matching the moment the row is Spanish; anything decorating a row that has to be parsed off again (see `unavailable_suffix`) must be built and stripped from the same translated string.

**Run `cargo run --bin gen-po` in the same change set**, and commit the updated `po/`. It rescans the source and `help.toml`, adds new messages with an empty translation, keeps every translation already written, preserves departed messages as commented-out `#~` entries, and warns about any translation that has lost or invented a placeholder. An empty translation is not a bug — the msgid is the English text, so an untranslated message shows its English original — but fill in the Spanish for anything you add, or say in your summary that you left it English.

Other notes:

- A shipped standalone binary that needs `t!` gets `#[path = "../i18n.rs"] mod i18n;`, as `soundpack_manager.rs` does. That file is the crate root there, so it must **not** `use crate::t;` — a `#[macro_export]` macro already lives at the crate root and importing it again is a redefinition.
- Adding a language is a `.po` under `po/`, a row in `i18n::LANGUAGES`, and a line in `i18n::CATALOGS`. Nothing ships beside the executable; catalogues are embedded.

## Coding Strategy

- Whenever a decision is reached to accomplish a task by polling, determine if the same task can be done via an event driven approach, if so, prefer it unless there's a good reason not to, then explain why it was done in the summary

## Other conventions

- The app version shown in the About dialog comes from `Cargo.toml`; bump it as part of cutting a release.
- Configuration schema changes must remain backward compatible or handled by the corruption/defaults recovery path in `src/config.rs`. New settings need README documentation — which, per **Documentation** above, means proposing the wording in `proposed_doc.md` rather than editing the README yourself.
- A new control needs a context-help message: `cargo run --bin gen-help` adds it to `help.toml` with a blank `message` for you to fill in. A blank one falls back to "No help available for this control." at runtime, so leaving it empty is a visible gap, not a neutral default.

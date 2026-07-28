//! Preferences dialog. Tabbed: "Archiving", "Speech", "Sound packs", and
//! "VST plugins".
//! The VST tab manages the plugin folder list and starts scans; scan progress
//! arrives on the pump (see `pump_scan_events` in `ui/mod.rs`). Every tab saves
//! as the user changes a control, so the dialog only needs a Close button.

use super::{App, ScanUi, WXK_DELETE, show_error};
use crate::vst::scan::{self, ScanMode};
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when no plugin folders are configured. See [`super::list`].
const NO_PLUGIN_FOLDERS: &str = "No plugin folders";

pub fn show(app: &Rc<App>, frame: &Frame) {
    let dialog = Dialog::builder(frame, "Preferences")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(560, 480)
        .build();

    let notebook = Notebook::builder(&dialog).build();
    let archiving_panel = Panel::builder(&notebook).build();
    notebook.add_page(&archiving_panel, "Archiving", true, None);
    build_archiving_tab(app, &dialog, &archiving_panel);
    let speech_panel = Panel::builder(&notebook).build();
    notebook.add_page(&speech_panel, "Speech", false, None);
    build_speech_tab(app, &speech_panel);
    let sounds_panel = Panel::builder(&notebook).build();
    notebook.add_page(&sounds_panel, "Sound packs", false, None);
    let sounds = build_sounds_tab(app, &dialog, &sounds_panel);
    let vst_panel = Panel::builder(&notebook).build();
    notebook.add_page(&vst_panel, "VST plugins", false, None);
    build_vst_tab(app, &dialog, &vst_panel);

    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let close_button = Button::builder(&dialog)
        .with_id(ID_CANCEL)
        .with_label("C&lose")
        .build();
    {
        let dialog = dialog.clone();
        close_button.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 4);
    dialog_sizer.add(&close_button, 0, SizerFlag::All, 8);
    dialog.set_sizer(dialog_sizer, true);

    dialog.show_modal();
    // Closing the dialog is also "settling": apply whatever the pack picker is
    // showing, then make sure its timer cannot outlive the window it belongs to.
    (sounds.apply)();
    sounds.alive.set(false);
    sounds.settle.stop();
    // A scan can still be running: `ScanEvent::Started` only builds a progress
    // dialog when the folders actually held plugins, so scanning an empty
    // folder leaves the scan alive with this dialog fully interactive. Its
    // `ScanUi` holds this dialog as the parent for its progress and result
    // message boxes, so it must not outlive the `destroy()` below.
    // Dropping the handle cancels the worker and joins it, so take it out from
    // under the borrow first.
    let scan = app.scan.borrow_mut().take();
    drop(scan);
    dialog.destroy();
}

fn build_archiving_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Stream Archiving group.
    let stream_group =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Stream Archiving")
            .build();
    let archive_default = CheckBox::builder(panel)
        .with_label("Archive streams by default")
        .build();
    super::set_accessible_name(&archive_default, "Archive streams by default");
    super::help::tag(
        &archive_default,
        "dialog.preferences.archive.archiveDefault",
        "Archive streams by default checkbox",
    );
    archive_default.set_value(app.config.borrow().archiving.archive_streams_by_default);
    stream_group.add(&archive_default, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let archive_default = archive_default.clone();
        archive_default.clone().on_toggled(move |_| {
            app.config.borrow_mut().archiving.archive_streams_by_default =
                archive_default.get_value();
            app.save_config();
        });
    }

    // Recording group.
    let recording_group =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Recording").build();

    let record_default = CheckBox::builder(panel)
        .with_label("Record streams by default")
        .build();
    super::set_accessible_name(&record_default, "Record streams by default");
    super::help::tag(
        &record_default,
        "dialog.preferences.archive.recordDefault",
        "Record streams by default checkbox",
    );
    record_default.set_value(app.config.borrow().archiving.record_streams_by_default);
    recording_group.add(&record_default, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let record_default = record_default.clone();
        record_default.clone().on_toggled(move |_| {
            app.config.borrow_mut().archiving.record_streams_by_default =
                record_default.get_value();
            app.save_config();
        });
    }

    let folder_label = StaticText::builder(panel)
        .with_label("Recording folder")
        .build();
    let folder_input = TextCtrl::builder(panel)
        .with_value(&app.config.borrow().archiving.recording_folder)
        .build();
    super::set_accessible_name(&folder_input, "Recording folder");
    super::help::tag(
        &folder_input,
        "dialog.preferences.archive.recordingFolder",
        "Recording folder path input",
    );
    recording_group.add(&folder_label, 0, SizerFlag::All, 4);
    recording_group.add(&folder_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    {
        let app = app.clone();
        let folder_input = folder_input.clone();
        folder_input.clone().on_text_updated(move |_| {
            app.config.borrow_mut().archiving.recording_folder =
                folder_input.get_value().trim().to_string();
            app.save_config();
        });
    }

    let browse = Button::builder(panel).with_label("&Browse...").build();
    super::help::tag(
        &browse,
        "dialog.preferences.archive.browse",
        "Browse for recording folder button",
    );
    recording_group.add(&browse, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let dialog = dialog.clone();
        let folder_input = folder_input.clone();
        browse.on_click(move |_| {
            let start = app.config.borrow().archiving.recording_dir();
            let picker = DirDialog::builder(
                &dialog,
                "Choose a folder for stream recordings",
                &start.to_string_lossy(),
            )
            .with_style(DirDialogStyle::MustExist.bits())
            .build();
            if picker.show_modal() != ID_OK {
                return;
            }
            let Some(path) = picker.get_path() else {
                return;
            };
            let path = path.trim().to_string();
            if path.is_empty() {
                return;
            }
            folder_input.set_value(&path);
            app.config.borrow_mut().archiving.recording_folder = path;
            app.save_config();
        });
    }

    sizer.add_sizer(&stream_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&recording_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
}

/// Credentials and limits for the network speech engines.
///
/// These live here rather than on each TTS source because a key retyped into
/// every scene is a lot of typing for a screen-reader user, and a lot of
/// copies of a secret. The source dialog keeps the engine, voice, and prosody.
///
/// Keys are stored DPAPI-encrypted; see `crate::secret`.
///
/// One engine at a time: eleven credential fields in one tab order meant
/// tabbing past OpenAI, Azure, AWS and Google to reach Star. The picker comes
/// first and everything after it belongs to whichever engine it names, so the
/// tab order is only ever as long as the engine you are actually configuring.
fn build_speech_tab(app: &Rc<App>, panel: &Panel) {
    use crate::tts::engines;

    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let intro = StaticText::builder(panel)
        .with_label(
            "Keys entered here are encrypted for your Windows account. \
             SAPI, Microsoft Edge, and Google Translate need no key.",
        )
        .build();
    sizer.add(&intro, 0, SizerFlag::All, 6);

    let engine_label = StaticText::builder(panel)
        .with_label("Speech engine")
        .build();
    let engine_choice = Choice::builder(panel).build();
    let engine_list = crate::tts::engine_names();
    for (_, display) in &engine_list {
        engine_choice.append(display);
    }
    super::set_accessible_name(&engine_choice, "Speech engine");
    super::help::tag(
        &engine_choice,
        "dialog.preferences.speech.engine",
        "Speech engine choice",
    );
    sizer.add(&engine_label, 0, SizerFlag::All, 4);
    sizer.add(&engine_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);

    // A page per engine, all built up front and all but one hidden. Hidden
    // windows are skipped by both the sizer and the tab order, which is the
    // whole point; rebuilding on each switch would cost the same and lose
    // whatever the user was part-way through typing.
    let pages: Vec<(&'static str, Panel)> = engine_list
        .iter()
        .map(|(id, _)| {
            let page = Panel::builder(panel).build();
            let page_sizer = BoxSizer::builder(Orientation::Vertical).build();
            build_engine_page(app, &page, &page_sizer, id);
            page.set_sizer(page_sizer, true);
            sizer.add(&page, 0, SizerFlag::Expand | SizerFlag::All, 4);
            (*id, page)
        })
        .collect();

    let shown = engines::resolve_id(&app.config.borrow().speech.last_engine);
    let shown_index = engine_list
        .iter()
        .position(|(id, _)| *id == shown)
        .unwrap_or(0);
    engine_choice.set_selection(shown_index as u32);

    {
        let app = app.clone();
        let panel = panel.clone();
        let pages = pages.clone();
        let ids: Vec<&'static str> = engine_list.iter().map(|(id, _)| *id).collect();
        let choice = engine_choice.clone();
        engine_choice.clone().on_selection_changed(move |_| {
            let id = choice
                .get_selection()
                .and_then(|i| ids.get(i as usize).copied())
                .unwrap_or(engines::SAPI);
            show_engine_page(&panel, &pages, id);
            app.config.borrow_mut().speech.last_engine = id.to_string();
            app.save_config();
        });
    }

    // Global, not per-engine: these cap what any network engine will spend, so
    // they stay put no matter which engine the picker names.
    let limits =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Limits").build();

    let chars_label = StaticText::builder(panel)
        .with_label("Longest message to speak, in characters")
        .build();
    let chars = SpinCtrl::builder(panel)
        .with_range(50, 5000)
        .with_initial_value(app.config.borrow().speech.max_chars() as i32)
        .build();
    super::set_accessible_name(&chars, "Longest message to speak, in characters");
    super::help::tag(
        &chars,
        "dialog.preferences.speech.maxChars",
        "Maximum message length",
    );
    limits.add(&chars_label, 0, SizerFlag::All, 2);
    limits.add(&chars, 0, SizerFlag::All, 2);
    {
        let app = app.clone();
        let chars = chars.clone();
        chars.clone().on_value_changed(move |_| {
            app.config.borrow_mut().speech.max_chars = chars.value().max(1) as usize;
            app.save_config();
        });
    }

    let interval_label = StaticText::builder(panel)
        .with_label("Shortest gap between requests, in milliseconds")
        .build();
    let interval = SpinCtrl::builder(panel)
        .with_range(0, 10_000)
        .with_initial_value(app.config.borrow().speech.min_request_interval_ms as i32)
        .build();
    super::set_accessible_name(&interval, "Shortest gap between requests, in milliseconds");
    super::help::tag(
        &interval,
        "dialog.preferences.speech.minInterval",
        "Minimum request interval",
    );
    limits.add(&interval_label, 0, SizerFlag::All, 2);
    limits.add(&interval, 0, SizerFlag::All, 2);
    {
        let app = app.clone();
        let interval = interval.clone();
        interval.clone().on_value_changed(move |_| {
            app.config.borrow_mut().speech.min_request_interval_ms = interval.value().max(0) as u64;
            app.save_config();
        });
    }

    sizer.add_sizer(&limits, 0, SizerFlag::Expand | SizerFlag::All, 4);

    panel.set_sizer(sizer, true);
    // After the sizer is in place, so the eight hidden pages' space is actually
    // reclaimed rather than left as a gap.
    show_engine_page(panel, &pages, shown);
}

/// Shows the page belonging to `engine` and hides the rest.
///
/// Frozen across the swap so the tab does not flicker through a half-laid-out
/// state, and `layout()` is what actually reclaims the hidden pages' space.
fn show_engine_page(panel: &Panel, pages: &[(&'static str, Panel)], engine: &str) {
    panel.freeze();
    for (id, page) in pages {
        page.show(*id == engine);
    }
    panel.layout();
    panel.thaw();
}

/// Fills one engine's page with the fields that engine needs.
///
/// Spelled out per engine rather than driven from a table because `gen-help`
/// scans this file for tag call sites and reads the id and label as *literal*
/// arguments; a loop passing them as variables makes it scrape whatever string
/// literal comes next and write nonsense into `help.toml`.
fn build_engine_page(app: &Rc<App>, page: &Panel, sizer: &BoxSizer, engine: &str) {
    use crate::secret::Secret;
    use crate::tts::engines;

    match engine {
        engines::OPENAI => {
            let key = secret_row(
                app,
                page,
                sizer,
                "OpenAI API key",
                engines::OPENAI,
                |s| s.openai_api_key.as_str().to_string(),
                |s, v| s.openai_api_key = Secret::new(v),
            );
            super::help::tag(
                &key,
                "dialog.preferences.speech.openaiKey",
                "OpenAI API key",
            );
        }
        engines::ELEVENLABS => {
            let key = secret_row(
                app,
                page,
                sizer,
                "ElevenLabs API key",
                engines::ELEVENLABS,
                |s| s.elevenlabs_api_key.as_str().to_string(),
                |s, v| s.elevenlabs_api_key = Secret::new(v),
            );
            super::help::tag(
                &key,
                "dialog.preferences.speech.elevenlabsKey",
                "ElevenLabs API key",
            );
            let model = text_row(
                app,
                page,
                sizer,
                "ElevenLabs model",
                engines::ELEVENLABS,
                |s| s.elevenlabs_model.clone(),
                |s, v| s.elevenlabs_model = v,
            );
            super::help::tag(
                &model,
                "dialog.preferences.speech.elevenlabsModel",
                "ElevenLabs model",
            );
        }
        engines::AZURE => {
            let key = secret_row(
                app,
                page,
                sizer,
                "Azure subscription key",
                engines::AZURE,
                |s| s.azure_key.as_str().to_string(),
                |s, v| s.azure_key = Secret::new(v),
            );
            super::help::tag(
                &key,
                "dialog.preferences.speech.azureKey",
                "Azure subscription key",
            );
            let region = text_row(
                app,
                page,
                sizer,
                "Azure region, for example eastus",
                engines::AZURE,
                |s| s.azure_region.clone(),
                |s, v| s.azure_region = v,
            );
            super::help::tag(
                &region,
                "dialog.preferences.speech.azureRegion",
                "Azure region",
            );
        }
        engines::AWS => {
            let id = text_row(
                app,
                page,
                sizer,
                "AWS access key ID",
                engines::AWS,
                |s| s.aws_access_key_id.clone(),
                |s, v| s.aws_access_key_id = v,
            );
            super::help::tag(&id, "dialog.preferences.speech.awsId", "AWS access key ID");
            let key = secret_row(
                app,
                page,
                sizer,
                "AWS secret access key",
                engines::AWS,
                |s| s.aws_secret_access_key.as_str().to_string(),
                |s, v| s.aws_secret_access_key = Secret::new(v),
            );
            super::help::tag(
                &key,
                "dialog.preferences.speech.awsSecret",
                "AWS secret access key",
            );
            let region = text_row(
                app,
                page,
                sizer,
                "AWS region, for example us-east-1",
                engines::AWS,
                |s| s.aws_region.clone(),
                |s, v| s.aws_region = v,
            );
            super::help::tag(&region, "dialog.preferences.speech.awsRegion", "AWS region");
            let polly_engine = text_row(
                app,
                page,
                sizer,
                "Polly engine, neural or standard",
                engines::AWS,
                |s| s.aws_engine.clone(),
                |s, v| s.aws_engine = v,
            );
            super::help::tag(
                &polly_engine,
                "dialog.preferences.speech.awsEngine",
                "Polly engine",
            );
        }
        engines::GOOGLE => {
            let key = secret_row(
                app,
                page,
                sizer,
                "Google Cloud API key",
                engines::GOOGLE,
                |s| s.google_api_key.as_str().to_string(),
                |s, v| s.google_api_key = Secret::new(v),
            );
            super::help::tag(
                &key,
                "dialog.preferences.speech.googleKey",
                "Google Cloud API key",
            );
            let language = text_row(
                app,
                page,
                sizer,
                "Google Cloud language code, for example en-US",
                engines::GOOGLE,
                |s| s.google_language_code.clone(),
                |s, v| s.google_language_code = v,
            );
            super::help::tag(
                &language,
                "dialog.preferences.speech.googleLanguage",
                "Google Cloud language code",
            );
        }
        engines::STAR => {
            let host = text_row(
                app,
                page,
                sizer,
                "Star server URL",
                engines::STAR,
                |s| s.star_host.clone(),
                |s, v| s.star_host = v,
            );
            super::help::tag(
                &host,
                "dialog.preferences.speech.starHost",
                "Star server URL",
            );
        }
        // SAPI, Microsoft Edge and Google Translate. Nothing focusable here, so
        // the tab order runs straight from the picker to the limits below —
        // which is itself the answer to "what do I have to fill in for this
        // one?". The intro text says as much for anyone who tabs past it.
        _ => {
            let none = StaticText::builder(page)
                .with_label("This engine needs no settings.")
                .build();
            sizer.add(&none, 0, SizerFlag::All, 6);
        }
    }
}

/// A masked credential field. See [`speech_row`].
fn secret_row(
    app: &Rc<App>,
    panel: &Panel,
    sizer: &BoxSizer,
    label: &str,
    engine: &'static str,
    read: fn(&crate::config::SpeechConfig) -> String,
    write: fn(&mut crate::config::SpeechConfig, String),
) -> TextCtrl {
    speech_row(app, panel, sizer, label, engine, true, read, write)
}

/// A plain field — a region, a model id, a host. See [`speech_row`].
fn text_row(
    app: &Rc<App>,
    panel: &Panel,
    sizer: &BoxSizer,
    label: &str,
    engine: &'static str,
    read: fn(&crate::config::SpeechConfig) -> String,
    write: fn(&mut crate::config::SpeechConfig, String),
) -> TextCtrl {
    speech_row(app, panel, sizer, label, engine, false, read, write)
}

/// Builds one setting row and wires it to save as the user types.
///
/// The caller tags the returned control for context help, because `gen-help`
/// needs those arguments to be literals at the call site.
#[allow(clippy::too_many_arguments)]
fn speech_row(
    app: &Rc<App>,
    panel: &Panel,
    sizer: &BoxSizer,
    label: &str,
    engine: &'static str,
    secret: bool,
    read: fn(&crate::config::SpeechConfig) -> String,
    write: fn(&mut crate::config::SpeechConfig, String),
) -> TextCtrl {
    let caption = StaticText::builder(panel).with_label(label).build();
    let mut builder = TextCtrl::builder(panel).with_value(&read(&app.config.borrow().speech));
    if secret {
        builder = builder.with_style(TextCtrlStyle::Password);
    }
    let input = builder.build();
    // The adjacent StaticText is not announced; name the control itself.
    super::set_accessible_name(&input, label);
    sizer.add(&caption, 0, SizerFlag::All, 2);
    sizer.add(&input, 0, SizerFlag::Expand | SizerFlag::All, 2);
    {
        let app = app.clone();
        let input = input.clone();
        input.clone().on_text_updated(move |_| {
            write(
                &mut app.config.borrow_mut().speech,
                input.get_value().trim().to_string(),
            );
            // A voice list fetched under the old credentials is stale.
            crate::tts::forget_voices(engine);
            app.save_config();
        });
    }
    input
}

/// The Sound packs tab. Unlike the other two it opens no sub-dialogs, so it
/// takes no `dialog` argument.
/// The live parts of the Sound packs tab that `show` has to tear down: the
/// pack picker's debounce timer must be stopped, and its pending selection
/// applied, before the dialog is destroyed.
struct SoundsTab {
    apply: Rc<dyn Fn()>,
    settle: Rc<Timer<Dialog>>,
    alive: Rc<std::cell::Cell<bool>>,
}

/// How long the pack picker waits after the last keypress before loading.
/// Same reasoning as the speech-engine picker in `ui/scenes.rs` (see the long
/// comment there): a `Choice` fires a selection change on every arrow key, and
/// loading a pack means AES over the whole file plus a decode of every sound,
/// so arrowing through the list must not do the work per key.
const SETTLE_MS: i32 = 300;

fn build_sounds_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) -> SoundsTab {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let pack_group =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Sound pack").build();

    let pack_choice = Choice::builder(panel).build();
    super::set_accessible_name(&pack_choice, "Sound pack");
    super::help::tag(
        &pack_choice,
        "dialog.preferences.sounds.pack",
        "Sound pack choice",
    );
    pack_group.add(&pack_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);

    let pack_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let import_pack = Button::builder(panel).with_label("&Import pack...").build();
    // ALT+K, not ALT+M: the VST tab's "Re&move folder" already claims that
    // mnemonic in this dialog.
    let remove_pack = Button::builder(panel).with_label("Remove pac&k").build();
    super::help::tag(
        &import_pack,
        "dialog.preferences.sounds.importPack",
        "Import sound pack button",
    );
    super::help::tag(
        &remove_pack,
        "dialog.preferences.sounds.removePack",
        "Remove sound pack button",
    );
    pack_buttons.add(&import_pack, 0, SizerFlag::All, 4);
    pack_buttons.add(&remove_pack, 0, SizerFlag::All, 4);
    pack_group.add_sizer(&pack_buttons, 0, SizerFlag::Expand, 0);

    // Index 0 is always the built-in pack, so a choice index maps to
    // `packs[index - 1]`.
    let packs: Rc<std::cell::RefCell<Vec<crate::soundpack::InstalledPack>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));

    let selected_pack: Rc<dyn Fn() -> String> = {
        let pack_choice = pack_choice.clone();
        let packs = packs.clone();
        Rc::new(move || match pack_choice.get_selection() {
            Some(0) | None => String::new(),
            Some(index) => packs
                .borrow()
                .get(index as usize - 1)
                .map(|p| p.file_name.clone())
                .unwrap_or_default(),
        })
    };

    // Refills the list from disk and selects `select` by file name, falling
    // back to the built-in pack when it is gone.
    let refresh_packs: Rc<dyn Fn(&str)> = {
        let pack_choice = pack_choice.clone();
        let remove_pack = remove_pack.clone();
        let packs = packs.clone();
        Rc::new(move |select: &str| {
            let installed = crate::soundpack::installed_packs();
            pack_choice.clear();
            pack_choice.append("Built-in default");
            let mut selection = 0u32;
            for (i, pack) in installed.iter().enumerate() {
                pack_choice.append(&pack.display_name);
                if pack.file_name == select {
                    selection = i as u32 + 1;
                }
            }
            *packs.borrow_mut() = installed;
            pack_choice.set_selection(selection);
            remove_pack.enable(selection > 0);
        })
    };
    refresh_packs(&app.config.borrow().sounds.pack.clone());

    // What is actually loaded, so passing back through the current selection on
    // the way to another one costs a string compare and nothing else.
    let applied = Rc::new(std::cell::RefCell::new(
        app.config.borrow().sounds.pack.clone(),
    ));
    let alive = Rc::new(std::cell::Cell::new(true));
    // Bumped per apply so a second switch made while the first is still loading
    // wins, however the two loads finish relative to each other.
    let generation = Rc::new(std::cell::Cell::new(0u64));

    let apply_pack: Rc<dyn Fn()> = {
        let app = app.clone();
        let dialog = dialog.clone();
        let pack_choice = pack_choice.clone();
        let selected_pack = selected_pack.clone();
        let applied = applied.clone();
        let alive = alive.clone();
        let generation = generation.clone();
        Rc::new(move || {
            let wanted = selected_pack();
            if wanted == *applied.borrow() {
                return;
            }
            *applied.borrow_mut() = wanted.clone();
            app.config.borrow_mut().sounds.pack = wanted.clone();
            app.save_config();

            let this = generation.get().wrapping_add(1);
            generation.set(this);
            if wanted.is_empty() {
                crate::soundpack::set_active(None);
                return;
            }

            // Decrypting and decoding a whole pack on the UI thread would
            // freeze the dialog, so the work goes to a thread and the result
            // comes back on the idle pump.
            let (sender, receiver) = crossbeam_channel::bounded(1);
            let loading = wanted.clone();
            std::thread::Builder::new()
                .name("soundpack-load".into())
                .spawn(move || {
                    let _ = sender.send(crate::soundpack::load_installed(&loading));
                })
                .ok();

            let app = app.clone();
            let dialog = dialog.clone();
            let pack_choice = pack_choice.clone();
            let applied = applied.clone();
            let alive = alive.clone();
            let generation = generation.clone();
            super::run_when_ready(move || {
                let result = match receiver.try_recv() {
                    Ok(result) => result,
                    Err(crossbeam_channel::TryRecvError::Empty) => return false,
                    Err(_) => return true,
                };
                // A later selection has already been applied; this pack is
                // stale and must not replace it.
                if generation.get() != this {
                    return true;
                }
                match result {
                    Ok(pack) => crate::soundpack::set_active(Some(pack)),
                    Err(e) => {
                        // Fall back rather than leave the user with a pack that
                        // is chosen but silent.
                        crate::soundpack::set_active(None);
                        *applied.borrow_mut() = String::new();
                        app.config.borrow_mut().sounds.pack = String::new();
                        app.save_config();
                        if alive.get() {
                            pack_choice.set_selection(0);
                            show_error(
                                &dialog,
                                "Sound pack",
                                &format!("That sound pack could not be loaded: {e}"),
                            );
                        } else {
                            log::warn!("Could not load the sound pack {wanted}: {e}");
                        }
                    }
                }
                true
            });
        })
    };

    // Owned here so it dies with the dialog: a timer whose owner window has
    // been destroyed keeps firing into freed memory. `show` stops it before
    // `destroy()`.
    let settle = Rc::new(Timer::new(dialog));
    {
        let apply_pack = apply_pack.clone();
        let alive = alive.clone();
        settle.on_tick(move |_| {
            if alive.get() {
                apply_pack();
            }
        });
    }

    {
        let settle = settle.clone();
        let remove_pack = remove_pack.clone();
        let pack_choice_for_event = pack_choice.clone();
        pack_choice.clone().on_selection_changed(move |_| {
            // Immediate, because it is only the button's own state: the load
            // itself waits for the timer.
            remove_pack.enable(pack_choice_for_event.get_selection().unwrap_or(0) > 0);
            settle.start(SETTLE_MS, true);
        });
    }

    {
        // A user who has tabbed away should not have to wait out the timer.
        let apply_pack = apply_pack.clone();
        let alive = alive.clone();
        pack_choice.clone().on_kill_focus(move |event| {
            if alive.get() {
                apply_pack();
            }
            event.skip(true);
        });
    }

    {
        let dialog = dialog.clone();
        let apply_pack = apply_pack.clone();
        let refresh_packs = refresh_packs.clone();
        import_pack.on_click(move |_| {
            // Flush a pending debounce first: importing while a half-made
            // selection is still parked would apply it afterwards.
            apply_pack();
            let picker = FileDialog::builder(&dialog)
                .with_message("Import a sound pack")
                .with_wildcard("Pubsplash sound packs (*.pspack)|*.pspack")
                .with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
                .build();
            if picker.show_modal() != ID_OK {
                return;
            }
            let Some(path) = picker.get_path().map(std::path::PathBuf::from) else {
                return;
            };
            let mut replace = false;
            match crate::soundpack::installed_name(&path) {
                Ok(name) => {
                    if crate::soundpack::packs_dir().join(&name).exists() {
                        let confirm = MessageDialog::builder(
                            &dialog,
                            &format!(
                                "A sound pack named {} is already installed. Replace it?",
                                name.trim_end_matches(".pspack")
                            ),
                            "Import pack",
                        )
                        .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                        .build();
                        if confirm.show_modal() != ID_YES {
                            return;
                        }
                        replace = true;
                    }
                }
                Err(e) => {
                    show_error(&dialog, "Import pack", &e);
                    return;
                }
            }
            match crate::soundpack::import(&path, replace) {
                Ok(file_name) => {
                    refresh_packs(&file_name);
                    // An import is an explicit act, so it takes effect now
                    // rather than after the settle timer.
                    apply_pack();
                }
                Err(e) => show_error(&dialog, "Import pack", &e),
            }
        });
    }

    {
        let dialog = dialog.clone();
        let apply_pack = apply_pack.clone();
        let refresh_packs = refresh_packs.clone();
        let selected_pack = selected_pack.clone();
        let pack_choice = pack_choice.clone();
        remove_pack.clone().on_click(move |_| {
            apply_pack();
            let file_name = selected_pack();
            if file_name.is_empty() {
                return;
            }
            let confirm = MessageDialog::builder(
                &dialog,
                &format!(
                    "Remove the sound pack {}?",
                    file_name.trim_end_matches(".pspack")
                ),
                "Remove pack",
            )
            .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
            .build();
            if confirm.show_modal() != ID_YES {
                return;
            }
            // Selecting the built-in pack first drops the removed pack's
            // samples, so nothing is left holding a pack that no longer exists.
            pack_choice.set_selection(0);
            apply_pack();
            if let Err(e) = crate::soundpack::remove(&file_name) {
                show_error(&dialog, "Remove pack", &e);
            }
            refresh_packs("");
        });
    }

    sizer.add_sizer(&pack_group, 0, SizerFlag::Expand | SizerFlag::All, 4);

    let interface_group =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, panel, "Interface sounds")
            .build();

    let startup = CheckBox::builder(panel)
        .with_label("Play the startup sound")
        .build();
    super::set_accessible_name(&startup, "Play the startup sound");
    super::help::tag(
        &startup,
        "dialog.preferences.sounds.playStartup",
        "Play the startup sound checkbox",
    );
    startup.set_value(app.config.borrow().sounds.play_startup);
    interface_group.add(&startup, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let startup = startup.clone();
        startup.clone().on_toggled(move |_| {
            app.config.borrow_mut().sounds.play_startup = startup.get_value();
            app.save_config();
        });
    }

    let shutdown = CheckBox::builder(panel)
        .with_label("Play the shut-down sound")
        .build();
    super::set_accessible_name(&shutdown, "Play the shut-down sound");
    super::help::tag(
        &shutdown,
        "dialog.preferences.sounds.playShutdown",
        "Play the shut-down sound checkbox",
    );
    shutdown.set_value(app.config.borrow().sounds.play_shutdown);
    interface_group.add(&shutdown, 0, SizerFlag::All, 4);
    {
        let app = app.clone();
        let shutdown = shutdown.clone();
        shutdown.clone().on_toggled(move |_| {
            app.config.borrow_mut().sounds.play_shutdown = shutdown.get_value();
            app.save_config();
        });
    }

    sizer.add_sizer(&interface_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);

    SoundsTab {
        apply: apply_pack,
        settle,
        alive,
    }
}

fn build_vst_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let folders_label = StaticText::builder(panel)
        .with_label("Plugin folders")
        .build();
    let folders_list = ListBox::builder(panel).build();
    super::native_acc::install(&folders_list, "Plugin folders");
    super::help::tag(
        &folders_list,
        "dialog.preferences.vst.folderList",
        "VST plugin folders list",
    );

    let folder_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add_folder = Button::builder(panel).with_label("&Add folder...").build();
    let remove_folder = Button::builder(panel).with_label("Re&move folder").build();
    super::help::tag(
        &add_folder,
        "dialog.preferences.vst.addFolder",
        "Add plugin folder button",
    );
    super::help::tag(
        &remove_folder,
        "dialog.preferences.vst.removeFolder",
        "Remove plugin folder button",
    );
    folder_buttons.add(&add_folder, 0, SizerFlag::All, 4);
    folder_buttons.add(&remove_folder, 0, SizerFlag::All, 4);

    let scan_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let scan_new = Button::builder(panel)
        .with_label("Scan for &new plugins")
        .build();
    let rescan_all = Button::builder(panel)
        .with_label("&Rescan all plugins")
        .build();
    super::help::tag(
        &scan_new,
        "dialog.preferences.vst.scanNew",
        "Scan for new plugins button",
    );
    super::help::tag(
        &rescan_all,
        "dialog.preferences.vst.rescanAll",
        "Rescan all plugins button",
    );
    scan_buttons.add(&scan_new, 0, SizerFlag::All, 4);
    scan_buttons.add(&rescan_all, 0, SizerFlag::All, 4);

    sizer.add(&folders_label, 0, SizerFlag::All, 4);
    sizer.add(&folders_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&folder_buttons, 0, SizerFlag::Expand, 0);
    sizer.add_sizer(&scan_buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    let refresh_folders = {
        let app = app.clone();
        let folders_list = folders_list.clone();
        move |select: Option<&str>| {
            let config = app.config.borrow();
            let folders = &config.plugins.folders;
            super::list::fill(&folders_list, folders, NO_PLUGIN_FOLDERS);
            if !folders.is_empty() {
                let select_index = folders
                    .iter()
                    .position(|folder| Some(folder.as_str()) == select)
                    .unwrap_or(0);
                folders_list.set_selection(select_index as u32, true);
            }
        }
    };
    refresh_folders(None);

    {
        let app = app.clone();
        let dialog = dialog.clone();
        let refresh_folders = refresh_folders.clone();
        add_folder.on_click(move |_| {
            let picker = DirDialog::builder(&dialog, "Choose a folder containing VST plugins", "")
                .with_style(DirDialogStyle::MustExist.bits())
                .build();
            if picker.show_modal() != ID_OK {
                return;
            }
            let Some(path) = picker.get_path() else {
                return;
            };
            let path = path.trim().to_string();
            if path.is_empty() {
                return;
            }
            {
                let mut config = app.config.borrow_mut();
                if config
                    .plugins
                    .folders
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(&path))
                {
                    drop(config);
                    show_error(&dialog, "Add folder", "That folder is already in the list.");
                    return;
                }
                config.plugins.folders.push(path.clone());
            }
            app.save_config();
            refresh_folders(Some(&path));
        });
    }

    let do_remove = {
        let app = app.clone();
        let folders_list = folders_list.clone();
        let refresh_folders = refresh_folders.clone();
        move || {
            let count = app.config.borrow().plugins.folders.len();
            let Some(index) = super::list::selection(&folders_list, count) else {
                return;
            };
            app.config.borrow_mut().plugins.folders.remove(index);
            app.save_config();
            refresh_folders(None);
        }
    };
    {
        let do_remove = do_remove.clone();
        remove_folder.on_click(move |_| do_remove());
    }
    folders_list
        .clone()
        .on_key_down(move |event| match super::key_of(&event) {
            Some((WXK_DELETE, _)) => do_remove(),
            _ => event.skip(true),
        });

    {
        let app = app.clone();
        let dialog = dialog.clone();
        scan_new.on_click(move |_| begin_scan(&app, &dialog, ScanMode::NewOnly));
    }
    {
        let app = app.clone();
        let dialog = dialog.clone();
        rescan_all.on_click(move |_| begin_scan(&app, &dialog, ScanMode::RescanAll));
    }
}

fn begin_scan(app: &Rc<App>, dialog: &Dialog, mode: ScanMode) {
    if app.scan.borrow().is_some() {
        return;
    }
    let folders = app.config.borrow().plugins.folders.clone();
    if folders.is_empty() {
        show_error(
            dialog,
            "Scan plugins",
            "Add at least one plugin folder first.",
        );
        return;
    }
    let existing = app.plugins.borrow().clone();
    match scan::start_scan(folders, mode, existing) {
        Ok(handle) => {
            // Enumeration alone can take a while on big folders; show an
            // indeterminate progress dialog right away. It is replaced with
            // a real one once the worker reports how many plugins it found.
            let progress = ProgressDialog::builder(
                dialog,
                "Scanning VST plugins",
                "Looking for plugins in the configured folders...",
                100,
            )
            .can_abort()
            .build();
            progress.pulse(None);
            *app.scan.borrow_mut() = Some(ScanUi {
                handle,
                progress: Some(progress),
                determinate: false,
                parent: dialog.clone(),
            });
            // Bring up the timer that drives the progress dialog now, rather
            // than waiting for the next idle to notice the scan exists.
            super::sync_fast_timer(app);
        }
        Err(message) => show_error(dialog, "Scan plugins", &message),
    }
}

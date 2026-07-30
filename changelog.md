# Changelog

## Unreleased

### Additions

- Added validation for OpenAI, ElevenLabs, Azure, AWS Polly, and Google Cloud credentials before they are saved.


- Added per-source voice settings for ElevenLabs, OpenAI, Azure, Google Cloud, AWS Polly, and Google Translate.

### Fixes

- Preserved source monitoring when sources are added, edited, reordered, or removed.
- Fixed ElevenLabs speech-rate requests at the supported speed limits.
- Applied rate, volume, and supported pitch settings to AWS Polly speech.
- Added accessible names to per-source speech engine controls.
- Prevented removed bus and master effects from being destroyed on the audio thread.
- Fixed a crash when removing an effect from a bus or the master chain. Plugin DLLs are now kept loaded for the session, so a plugin that leaves work running behind it can no longer be unloaded out from under itself, and a plugin module's factory is held for as long as the app runs — shell plugins such as Waves' WaveShell keep their per-process state on it and crashed while shutting down without it.
- Fixed VST2 effects being asked for their state, or having their interface closed, while the audio engine was still processing them — the two are now serialized, and the effect is faded out of the signal path first, as VST3 effects already were.
- Stopped bypassing the wrong effect when an earlier effect in the same chain is missing or failed to load.
- Editing an effect on one bus no longer rebuilds the master chain and the other buses, and effects that are fading in or out are no longer snapped back when an unrelated effect on the same bus is added or removed.
- Fixed an effect's own interface window, or a resize it requests, being matched to a different effect after the original was removed.

### Changes

- Added crash reporting: if Pubsplash is brought down by a hosted plugin, the log now names the plugin file responsible and a crash dump is written to `%LOCALAPPDATA%\pubsplash\crashes\`.
- Added a persistent TTS catalog with automatic startup refresh and stale-result retention.
- Replaced OpenAI and ElevenLabs model fields and Polly engine selection with catalog-backed dropdowns.
- Removed manual voice fetching in favor of automatically refreshed voice dropdowns.


## 0.1.2

### Additions


- Added streaming service profiles with Audiopub and direct Icecast service types.
- Added Icecast server, port, mount point, username, and password settings for direct source streaming.
- Added native interface windows for VST3 plugins that provide one.
- Added VST3 effect insertion, processing, parameters, and state restore for scanned plugins, including plugins with mono input or mono output.
- VST3 instruments and other plugins with no audio input can now be added to a chain; their output is mixed into the bus rather than replacing it. They are played from their own interface — Pubsplash sends no MIDI.
- Effects now fade in and out of the signal path over 50 ms instead of switching instantly, so bypassing an effect, or opening a VST3 plugin's interface while streaming, no longer clicks or drops the effect out of the stream.
- The effects list now names each plugin's format ("1. Compressor (VST3)"), so a plugin installed in both formats can be told apart.
- Added `F6`/`SHIFT+F6` to cycle between lists on the current tab and the tab bar.
- Added user-configurable keybinds (Preferences > Keybinds), covering streaming, recording, next/previous scene, switching to a named scene, and monitoring or muting master, any source, and any bus.
- Added default keybinds: `F9` starts and stops streaming, `F10` starts and stops recording. Both can be changed or removed like any other binding.
- Keybinds can be marked **Global** so they work while another application is in front. A global binding must include `CTRL`, `ALT` or `SHIFT`, or be a function key, since a bare letter would be swallowed everywhere you type.
- The Add binding dialog captures a shortcut by having you press it, with `Escape` or `Delete` to clear. `Tab` and `Shift+Tab` still move focus rather than being captured, and binding a combination already in use offers to move it.
- Starting and stopping a stream or a recording is now announced through the screen reader, however it was triggered, so a keybind pressed from another tab or another application is never silent.
- Added eight more TTS engines: Microsoft Edge, Google Translate, OpenAI, ElevenLabs, Azure, AWS Polly, Google Cloud, and self-hosted Star.
- Added a Speech tab in Preferences for per-engine credentials (DPAPI-encrypted).
- Added an available-voices count to the TTS source dialog.
- Added Get available voices (`ALT+G`) and Preview voice (`ALT+P`) buttons to the TTS source dialog.
- Added a pitch control to TTS sources.
- Added per-engine message length and rate limits to the Speech tab; oldest queued chat messages are now dropped instead of piling up.
- Added TTS failure reporting to the chat list, rate-limited to one per minute.
- Added custom sound pack import/removal (Preferences > Sound packs).
- Sound pack audio is now decoded and held in memory instead of read from disk on each cue.
- The sound pack picker no longer loads a pack on every arrow key, only once you stop or tab away.
- Added Help > View Changelog, opening the changelog in the browser.
- The Home tab overview now reports recording state and counts recording duration.

### Fixes

- Fixed `CTRL+M` on a mixer strip playing the Windows error sound as well as toggling monitoring. Typing any other letter on a volume slider is now silent too.
- Fixed the Send level slider in the Sends dialog and the Voice volume, Speech rate, and Voice pitch sliders in the TTS source dialog moving the wrong way on the arrow and page keys, and being read by screen readers as a percentage of their range. They now behave exactly like the mixer's volume sliders.
- Fixed the Service type radio buttons in the streaming service dialogs announcing nothing; they now speak their own labels, role, and checked state.
- Plugins that are installed but fail to load are no longer reported as "not installed"; both the startup summary and the Add plugin error now say what actually went wrong.
- Fixed a plugin's saved settings being replaced with defaults in `config.json` when it could not report its state.
- Fixed the plugin parameter dialog being able to announce one parameter's name while editing a different one, after a preset was loaded in the plugin's own interface.
- Typed values in the plugin parameter dialog are now spoken as rejected when the plugin cannot parse them, instead of being discarded silently.
- Fixed mono-input VST2 effects being fed the left channel alone instead of a centre downmix.
- Fixed a failing VST3 plugin writing a log line every 10 ms; the failure is now reported once.
- The Home tab overview is now a list instead of a text box, fixing arrow keys not being able to reach past rows rewritten each second.
- Fixed the overview's selected row being re-announced by its own per-second refresh.
- Empty lists now show a placeholder row ("No chats", "No sources", etc.) instead of announcing "Unknown".
- Fixed lists re-announcing themselves on unrelated refreshes (Scenes list on source delete, bus list on plugin add).
- Fixed the TTS engine list stalling on every keypress; the voice list now rebuilds after you stop moving or tab away.
- Fixed the voice count showing the previous engine's figure after switching engines.
- Fixed the selected voice being discarded when passing over other engines.
- Fixed "Send these sounds to the stream" muting local playback of Sound Events cues.
- Fixed reordering/deleting a bus saving an open plugin's settings to the wrong bus.
- Settings files are now written atomically, preventing corruption on crash or power loss.
- Fixed exiting while recording truncating the file.
- Fixed a crash closing Preferences during a plugin scan.
- Fixed `Escape` not closing several dialogs (Preferences, Configure Audio Pub, Set stream info, TTS/Sound Events source dialogs, Sends, Load chain, Missing plugins, chat message window) and not working on all controls in the plugin parameter dialog.
- `Escape` now also closes a plugin's interface window when focus is on its toolbar.
- Fixed reordering a bus briefly misrouting a source's send.
- Fixed duplicate-name collisions when a numbered name was already taken.
- Fixed a corrupt settings file overwriting its own backup.
- Fixed the Sound Pack Manager and VST plugin scanner not starting from the portable ZIP.
- Fixed the Sound Pack Manager opening a console window alongside its own.
- Missing helper program errors now name the file and folder instead of "os error 2".

### Changes


- Repaired mangled text encoding across the source and documentation. Em and en dashes had been round-tripped through Windows-1252 up to three times and were showing up as runs of six to nine accented Latin and currency characters in `README.md`, `changelog.md`, and several source comments; they are proper dashes again. Byte-order marks were also removed from `README.md`, `changelog.md`, `help.toml`, and `src/bin/soundpack.rs`, so every file in the repository is now plain UTF-8 without a BOM.

- Removed the status bar. Screen readers could no longer find it, and everything it showed is on the Home tab's stream overview list, which is fully keyboard reachable.
- The stream overview list now includes a Quality row ("Quality: 128 kbps MP3") while streaming, which used to be shown only in the status bar.
- Renamed File > Configure Audio Pub to File > Setup streaming services.
- "Send speech to the stream" now only affects the stream mix; monitoring (`CTRL+M`) still lets you hear other engines locally.
- Chat messages are now spoken immediately instead of after up to a 0.1s delay; status/error messages also appear immediately.
- Reduced idle power usage.
- Fixed the mixer and Sources list hitching every couple of seconds with an Application source configured.
- Recording now happens on its own thread, avoiding gaps from slow disks.
- Pubsplash now drops audio instead of buffering it when the server connection stalls.
- The stream/record buttons are no longer re-announced once a second while streaming.
- The chat list no longer rebuilds on every incoming message.
- Settings are now saved once a second and on exit instead of on every slider/text change.
- Fixed the bus effects list and plugin parameter filter stalling on large presets.
- Moved log writing off the audio mixing thread.
- Help > Open Readme now opens the local copy (falling back to GitHub) without flashing a console window.
- The changelog now ships as a web page instead of a Markdown file.
- `soundpack.exe` is now installed alongside Pubsplash.
- The installer no longer includes `gen-help.exe`.

## 0.1.1

### Additions


- Added streaming service profiles with Audiopub and direct Icecast service types.
- Added Icecast server, port, mount point, username, and password settings for direct source streaming.
- Sound packs: randomized WAV cues for listener changes and incoming or outgoing chat, played through configurable Sound Events sources. A Sound Events source plays the pack built into Pubsplash; each of the five events has its own checkbox in the source's edit dialog, so you choose which ones make a sound. (Pointing a source at a pack of your own will arrive on the Preferences Sound packs tab; the per-source path box that briefly existed has been removed.) A standalone Sound Pack Manager is available from Tools, with project creation/opening, interface and stream-event tabs, one-WAV-per-event saving, test playback, and revision-bumping compilation, producing encrypted `.pspack` files.

- A default sound pack is now baked into the executable and plays local startup and shutdown cues automatically.

- Preferences has a new **Sound packs** tab (between Archiving and VST plugins) with an **Interface sounds** group holding "Play the startup sound" and "Play the shut-down sound". Both are on by default and saved in the configuration. Turning the shut-down sound off also makes closing the window immediate, since there is no longer a cue to wait for.

- Sound Events sources can be kept off the air. Their edit dialog gained a "Send these sounds to the stream" checkbox, on by default (so the previous behavior is unchanged); clear it and the cues play on your own output device instead, so only you hear them and nothing reaches the stream or a recording. A cue kept local bypasses the mixer, so the source's volume slider no longer affects it — muting the source still silences it. The setting is saved per source, like the rest of that dialog.

- Capture sources reconnect on their own. If a microphone or desktop audio source cannot be opened — the interface has not finished starting up, it is unplugged, or its driver resets mid-session — Pubsplash keeps trying (quickly at first, then every five seconds) until it comes back, instead of leaving that source silent until you edited it. While a source is retrying, its mixer strip and its entry on the Scenes and Sources tab read "(reconnecting)", so a screen reader tells you the microphone is not on air; the labels change in place, without moving keyboard focus. A source configured for a specific microphone is never quietly switched to a different one — being silent is better than going on air through the laptop's built-in microphone without being told.
- Application sources now show the application's real name. While the app is running, Pubsplash reads the product name out of its executable and uses that — an Application source pointing at `nvda.exe` reads as "NVDA volume" on the Home tab, and "Application: NVDA (nvda.exe)" on the Scenes and Sources tab — so it is clear which app a slider is adjusting. When the app is not running, the name you typed is shown instead, with "(not running)" on the sources list.
- Applications are picked up automatically once they start. Previously an Application source only attached to its process when the scene was switched or the source was edited, so naming an app that wasn't running yet left the source silent until you went back and re-saved it. Pubsplash now checks every couple of seconds and starts capturing as soon as the app appears (and updates the labels to match), without moving keyboard focus. Note that attaching restarts the other capture sources for an instant, so a source starting or exiting mid-stream can cause a brief blip.
- Volume boost on mixer strips: every volume slider on the Home tab (Master, each source, and each bus) now has a context menu — opened with a right click, `SHIFT+F10`, or the `Applications` key — containing a checkable **Enable volume boost** item. With it on, that slider's range grows from 0-100% to 0-500%, adding up to 5x of make-up gain for a source that is too quiet at the Windows level; turning it off snaps the slider back to 100% if it was higher. The setting is per strip and saved with your configuration. There is no limiter, so an already-loud source will distort if pushed. Mixer sliders also gained a UIA provider so screen readers speak the real percentage ("250%") instead of the native trackbar's percentage-of-range.
- Release tooling: `tools/release-changelog.ps1` rolls the changelog's `## Unreleased` entries into a new `## <version>` section (version read from `Cargo.toml`, or `-Version` to override), leaving Unreleased's sub-headings in place but empty. Run it before committing and tagging a release; use `-DryRun` to preview. It only rewrites the changelog — committing and tagging are left to you.
- Context-sensitive help: press **F1** on any control to have its purpose spoken by your screen reader (the announcement interrupts current speech). The messages are hand-written per control in `help.toml` at the project root; a control with no message yet falls back to a generic "No help available for this control." The `gen-help` dev tool (`cargo run --bin gen-help`) round-trips that file from the source — it adds newly added controls with a blank message to fill in, refreshes each control's label, preserves everything already written, and moves removed controls to a `[[stale]]` section rather than deleting your text.
- Packaging via cargo-packager (`[package.metadata.packager]`): builds a Windows NSIS installer that lets the user choose a per-user (`AppData\Local`) or per-machine (`Program Files`) install, using `assets/icon/pubsplash.ico` for installer and shortcut icons. A `build.rs` embeds the same icon (and version metadata) into the executable via `winresource`, so Explorer, the taskbar, and Alt+Tab show it.
- Initial project scaffolding: wxDragon window with Home, Chat, and Scenes and Sources tabs plus a status bar.
- JSON configuration in `%LOCALAPPDATA%\pubsplash\` with automatic generation, and corrupt-file recovery via `.bak` backup.
- Logger with selectable levels, file rotation, and `PUBSPLASH_LOG_<LEVEL>` environment variable override.
- Release CI workflow: `v` tags build a release with cargo-packager, producing an NSIS installer (which places the converted README and changelog next to the executable) alongside a portable ZIP.
- Audio engine: WASAPI capture for microphone, desktop audio (loopback), and per-application sources; 48 kHz stereo mix bus with per-source and master volume; mute/unmute with short fades that restore the previous volume.
- MP3 encoding via LAME with configurable bitrate, streamed to Audio Pub over the Icecast source protocol.
- Audio Pub client: login, stream key retrieval, stream creation and teardown, chat sending, and the live events feed (listener counts, incoming chat).
- Home tab: live stream overview box, mixer with keyboard-friendly sliders (Home = max, End = min), scene list with ALT+W/Enter switching, and a context-aware Start/Stop streaming button (ALT+S / ALT+T).
- Chat tab: accessible message list in `user: message: relative time` format with live-updating relative times, a View window (ALT+V) with copyable text, and an input box that Escape clears.
- Scenes and Sources tab: full scene/source management with the specced buttons and shortcuts, CTRL+Up/Down reordering, Delete removal (default scene protected), and per-type source edit dialogs (microphone device picker, TTS engine/voice/volume/rate).
- Configure Audio Pub dialog: site list with the permanent main site, add/remove custom instances, masked password entry, connect/disconnect with validation feedback, and automatic reconnect to the last used site on launch.
- Exit confirmation when streaming, with clean stream teardown on confirm.
- SAPI voice enumeration for the TTS source dialog.
- Incoming chat messages are now read aloud through SAPI by unmuted Text-to-Speech sources in the active scene, honoring the configured voice, rate, and volume.
- TTS sources with "Send speech to the stream" enabled now synthesize speech directly into the outgoing stream mix (at the mixer's native format), while still playing locally so the broadcaster hears it.
- Desktop Audio capture now excludes Pubsplash's own process, so locally played TTS and sound cues can never feed back into the stream.
- Set stream info dialog (File menu): stream title, description, and an "Archive the stream" checkbox, sent to the server when the stream is created. If the info was never set, clicking Start streaming opens the dialog first; OK starts the stream with whatever is filled in (defaults: "Stream" / "This is just a stream" / archiving off). Tabbing into a text field selects its contents for easy overwriting. The values reset on every launch.
- `{title}` and `{url}` stream tokens (title and public stream page link) are available internally for upcoming social-media announcement support.
- Preferences now has an Archiving tab (before the VST plugins tab) with a "Stream Archiving" group and a "Recording" group. Its "Archive streams by default" checkbox (off by default, saved in the config) makes the "Archive the stream" box in the Set stream info dialog start checked on every launch; you can still uncheck it for an individual stream.
- Standalone recording: a "Start recording" / "Stop recording" button on the Home tab (next to Start streaming, and next after it in Tab order; `ALT+R` to start, `ALT+C` to stop) records the current audio mix to an MP3 file **without** streaming. It writes the same kind of file as "Record this stream", named `recording_<yyyy-mm-dd>_<HH-MM-SS>.mp3`. Recording and streaming are mutually exclusive: the record button is disabled while streaming or connecting, and Start streaming is disabled while recording.
- Local stream recording: turn on "Record this stream" in the Set stream info dialog to save a copy of the broadcast to disk while you stream. Recordings are the exact MP3 that is streamed (no re-encoding), written to `recording_<yyyy-mm-dd>_<HH-MM-SS>.mp3` in the recording folder. If a new recording would land on a name that already exists (for example a stop and restart within the same second), a `__001`/`__002` suffix is added so an earlier recording is never overwritten. The Preferences Archiving tab's Recording group sets the destination folder (a type-in box with a Browse button, defaulting to your Music library) and a "Record streams by default" checkbox (off by default) that, like the archiving default, seeds the per-stream checkbox on each launch.
- Preferences dialog (File menu, `CTRL+,`) with a VST plugins tab: manage the folders scanned for plugins (Add folder via a directory picker, Remove folder or Delete key), pre-populated with the standard Windows VST locations and the `HKLM\SOFTWARE\VST\VSTPluginsPath` registry value.
- VST plugin discovery and scanning: finds VST2 DLLs (only those that actually export a VST entry point), single-file VST3 plugins, and VST3 bundle folders. "Scan for new plugins" scans only files not seen before; "Rescan all plugins" starts over. A progress dialog (screen-reader announced) shows scan progress with a Cancel button; cancelling stores nothing.
- Each plugin is loaded in a separate `pubsplash-scan.exe` helper process, so a plugin that crashes or hangs while loading cannot take Pubsplash down; plugin activation dialogs appear normally. VST3 bundles with a `moduleinfo.json` are identified without loading at all. Plugins built for a different processor architecture (e.g. 32-bit) are skipped.
- Discovered plugins are cached in `%LOCALAPPDATA%\pubsplash\vst_plugins.json` and loaded at startup, so scans don't need to be repeated.
- Mixing buses: create any number of global buses on the new Buses tab (add, rename, remove, reorder with CTRL+Up/Down, Delete to remove). Every bus outputs to master and gets its own volume/mute strip in the Home mixer.
- Per-source sends: each source has a "Sends..." dialog (Scenes and Sources tab) choosing which buses it feeds and at what level, plus a "Send directly to master" checkbox — on (the previous behavior) for aux-style dry+FX mixes, off to route a source only through its buses.
- VST2 effect chains on buses and on the master output: add plugins to a bus's chain on the Buses tab (the "Master output" row hosts the master chain), reorder them (their order is the processing order), bypass them, and remove them. Chains are applied live to the audio while streaming. Each plugin's state (its saved chunk or parameter values) is remembered in the config. (VST3 effect processing is planned for a later release; VST3 plugins are catalogued but not yet insertable.)
- FX chain library: save the selected chain under a name, load a saved chain, and delete saved chains — all stored together in `%LOCALAPPDATA%\pubsplash\fx_chains.json`. Chains can be exported to a standalone `.pubfx` file and imported on another machine to share setups.
- Robust chain loading: when a saved or imported chain references plugins that aren't installed on this machine, a dialog lists the missing plugins; if at least one plugin is available you can apply the chain with just those, or cancel. At startup, plugins missing from your configured buses are reported once and skipped (never dropped from the config).
- Accessible plugin control, two ways: "Edit parameters" opens an OSARA-style dialog that works for every plugin — filter and pick a parameter, adjust its value with the arrow keys (Page Up/Down for larger steps, Home/End for maximum/minimum), and move between parameters with CTRL+Tab; the parameter name and its formatted value are announced as you go. "Open interface" shows the plugin's own window for plugins that have one, and F6 always moves focus out of the plugin's interface back to the window's toolbar so the keyboard is never trapped.
- The audio engine now flushes denormals to zero, keeping CPU use stable once FX plugins with long reverb/filter tails are in the chain.
- The Set stream info dialog now has a Quality dropdown to choose the MP3 encoder bitrate (48–320 kbps). Unlike the title and description, this setting is saved in the configuration file and persists across sessions.
- The plugin scan progress dialog now has a Skip button: if a plugin hangs or stalls the scan (for example while waiting on a hidden dialog), Skip abandons just that plugin and the scan moves on. Skipped plugins are counted in the scan summary and can be retried with "Rescan all plugins".

### Fixes

- The Sound Events source dialog is now accessible. Its five event checkboxes had no accessible names or F1 help, so a screen reader announced them as unlabeled controls; they now read out properly and answer F1 like every other control. Their labels also come from the sound pack's own event names, so this dialog and the Sound Pack Manager can no longer name the same event differently.

- Sound packs now accept any readable WAV file instead of rejecting cues solely because they are not 48 kHz stereo; playback converts them to the engine format when decoded.

- The Sound Pack Manager now creates new projects in a sanitized child folder, stores that name in `sound-pack.toml`, and adds an explicit Save step that copies the selected WAVs into the project before Compile reads them.

- The Sound Pack Manager Source WAV field now follows the selected interface sound or stream event without wiping browsed or typed paths from other events.

- The Sound Pack Manager now disables its project editing tabs and Compile button until a project is created with **New** or loaded with **Open**, avoiding ambiguous save or compile actions before a project folder exists.

- Windows builds now embed a Common Controls v6 application manifest, suppressing wxWidgets manifest warnings in the standalone Sound Pack Manager and other Pubsplash executables.

- Lists no longer stop halfway (and dialogs no longer refuse to open) because of an application's own version information. Names shown for running applications are read out of the executable's version resource, and Windows reports the length of the whole stored value rather than of the text inside it — Spotify's, for example, comes back as `Spotify` followed by a NUL and a few stray bytes from the next entry. Handing such a name to a wx list raised an error that was swallowed without a trace, so the list simply ended at that application, whatever was meant to be selected in it was lost, and once the offending app was in the list at all the dialog stopped appearing. Names are now cut at the first NUL, where they really end.
- Crashes inside a control's own handling — pressing a button, typing in a field — are now written to the log with the exact source location. Previously they were discarded by the UI toolkit before reaching the log: no message, no speech, no crash, just a control that stopped doing anything, and no way to tell what had happened. This is what turned the bug above into a silent one.
- Editing a source can no longer discard the change without saying so. If the list selection the edit was based on has since gone (the scene or source no longer exists), that now goes to the log instead of being dropped in silence.
- The process lookup no longer answers "nothing is running" for the rest of the session after an internal error. Its caches recover explicitly and log that they did, instead of leaving the application picker permanently empty and every Application source reading "(not running)".
- A source whose device was not ready the moment Pubsplash launched no longer stays silent for the whole session. USB audio interfaces are sometimes still being brought up by Windows when Pubsplash opens its sources, a few hundred milliseconds into launch; the endpoint is already listed but not yet active, and opening it failed with a bare "The system cannot find the file specified" that then killed the source until the user went and re-picked the device by hand. Pubsplash now checks that a configured microphone is actually active before opening it, and retries instead of giving up (see the reconnection entry above). The failure is also reported once rather than twice in the log, and each message now names the step that failed instead of only the raw Windows error.
- Mixer volume sliders no longer move the wrong way, and no longer get stuck after a single step. `Up`, `Right`, and `Page Up` now always raise the volume and `Down`, `Left`, and `Page Down` always lower it, with `Home` for maximum and `End` for minimum. Arrows step by 1% and page keys by 10%. The underlying Windows trackbar's built-in mapping is the opposite of what people expect (natively `Up` and `Page Up` move *down*, and `Home` jumps to the *minimum*), so Pubsplash defines the whole mapping itself — the same on every strip and at either range. It also now marks those keys as handled, which it previously failed to do: the trackbar was still processing every keystroke a second time with its own mapping, cancelling out the arrow keys, reversing the page and `Home`/`End` keys, and overwriting the saved volume, while the screen reader announced the value Pubsplash had intended.
- In a VST plugin's accessible parameters dialog, the value slider's thumb now matches the value being announced. It was lagging behind by a step, for the same reason: the native trackbar was also acting on each keystroke.
- Exiting with ALT+F4 no longer crashes on shutdown (access violation, `0xc0000005`). The 100 ms pump timer was leaked and never stopped, so during the frame's teardown it kept firing `WM_TIMER` into the already-destroyed frame and crashed inside wxWidgets' event dispatch. The timer is now owned by the app and stopped when the window closes; the close handler also destroys the frame through wxWidgets' deferred teardown (the same path File > Exit uses) so both exit routes behave identically.
- Release CI now builds: corrected the NSIS installer-mode key in `Cargo.toml` (`installer-mode`, not `install-mode`), which cargo-packager rejected as an unknown field, and bumped `actions/checkout` to v5 (v4 pins the deprecated Node.js 20 runtime).
- The plugin parameter dialog now announces a parameter's new value to screen readers as you adjust it, reading the plugin's own formatted value (e.g. "-3.0 dB") rather than a raw slider position. The value slider is a native Windows control whose built-in accessibility only exposes a numeric position, so Pubsplash installs its own UI Automation provider on it: the provider reports the formatted value and raises a value-change event on each keyboard step, which the screen reader speaks immediately (and reads correctly when you tab back to the slider).
- Escape now closes the plugin parameter dialog.
- Many working plugins are no longer wrongly rejected as "crashed while loading": the scan helper now reports its result before Windows notifies the plugin DLL of process exit (a teardown path where many otherwise fine plugins crash), and a probe whose result was already delivered is accepted even if the helper process then dies.
- The plugin scanner is friendlier to picky plugins, so fewer crash for real while being probed: the VST2 host callback now answers common startup queries (sample rate, block size, time info, host identification) instead of returning zero for everything, plugins are told the sample rate and block size after opening, COM is initialized in the scan helper, and plugins' own dependency DLLs now resolve from the plugin's folder.

- The TTS source edit dialog no longer freezes the app for several seconds while opening: SAPI voices are enumerated once on a background thread at startup and cached for the session (voices installed mid-session appear on next launch).
- List boxes (Scenes, Sources, Chat, Sites) announce their items again under NVDA: the accessible-name override now applies only to the control itself instead of also renaming every list item after it.
- Login and stream creation now understand SvelteKit's JSON action-result envelope (the server answers 200 with the real outcome in the body when the client is not a browser), so connecting to Audio Pub works and failed logins report the server's actual error message.
- Stream key retrieval no longer fails when the site's layout data includes the session user; the parser now skips nodes without a stream key instead of giving up.
- Connection results (success and failure) are now announced in a dialog parented to the Configure Audio Pub window, and keyboard focus returns to the Connect button afterwards instead of landing in an unrelated spot. Successful connections are reported explicitly, and the Connect/Disconnect button label updates immediately.
- Incoming chat messages no longer vanish: the server sends users with both `name` and `displayName`, which the message parser previously rejected as a duplicate field, silently dropping every chat event. Messages now appear in the Chat tab (showing the display name) and trigger TTS.
- The sources list now shows each source's actual configuration - the selected microphone's device name (for example "Microphone (Zoom H1)"), the captured application, or the TTS engine and voice - instead of just repeating the source type.
- Screen readers now announce proper names for controls that previously read as bare widgets: the "Send speech to the stream" checkbox, mixer volume sliders and mute buttons (which include the source name and update on toggle), the scene/source/site lists, the chat list and input box, the stream overview, and the email/password fields.

### Changes


- Renamed File > Configure Audio Pub to File > Setup streaming services.
- The startup cue now plays while Pubsplash is still loading rather than after the window is up, so you hear the app coming to life immediately instead of after a silent pause while plugins are instantiated.
- Exiting no longer freezes the window while the shutdown cue plays. The window now disappears the moment you confirm the exit, the cue plays through to its end, and only then is the app torn down. If the playback device cannot be opened, the exit gives up on the cue after five seconds rather than hanging.

- Application sources are now chosen from a list of running applications instead of typing a program name from memory. Adding or editing an Application source opens a picker that, by default, offers only the applications that have played sound since they started — a short, arrowable list where each entry reads as the application's real name followed by its program file ("Firefox (firefox.exe)"). Unchecking **Only show apps that have played sound** widens it to every open application, including ones that have not made a sound yet; Windows' own components are left out of both views, and Pubsplash never offers itself. Choose an application with **Select**, by pressing ENTER, or by double-clicking it; ESCAPE cancels. **Refresh** looks again, for an application you have just started or just played something in. Editing an existing source reopens the picker with that application already highlighted, or, when it is no longer running, as a "(not running)" entry at the top so cancelling cannot lose the setting. **Type a name...** keeps the old behaviour for an application that is not running yet: the source stays silent until the program starts, then picks it up on its own.
- Sources are now named after what they actually capture, on both the Scenes and Sources list and the Home tab's mixer, instead of repeating their type. A microphone reads as the device it is set to ("Microphone (ZOOM H1essential)"), so several microphones in one scene are told apart at a glance and by the volume sliders; Desktop Audio and Sound Events no longer say their own name twice; a Text-to-Speech source carries its voice ("Text-to-Speech (Blastbay Libby - English (United States))"), which the mixer previously left out entirely; and an Application source names the application rather than reading as a generic "Application volume". Two sources that would still read identically (two microphones both on the system default device, say) get a trailing number.
- The plugin parameter dialog's value field is now an editable type-in box instead of a read-only display. It still shows the selected parameter's current value in the plugin's own units (e.g. "-6.0 dB") and updates as you adjust the slider; now you can also type a value and press Enter or Tab away to set it. This uses the plugin's optional string-to-value conversion — for plugins that don't support it, the typed text can't be applied and the field simply reverts to the actual value (the slider and arrow keys still work as before).
- Mute and Bypass controls are now checkboxes instead of buttons, so their on/off state is directly announced by screen readers: each mixer strip's Mute checkbox (Home tab) is checked when muted, the Buses tab's Bypass checkbox reflects the selected plugin (and updates as you move through the chain), and the plugin interface window's Bypass checkbox reflects that plugin.
- The stream title is no longer remembered between sessions; it is part of the per-session stream info instead.

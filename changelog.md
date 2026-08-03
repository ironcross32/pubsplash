# Changelog

## Unreleased

### Additions

### Fixes

### Changes

## 0.1.4

### Additions

- Mastodon announcements. Preferences has a new **Mastodon** tab, in three groups. Under **Account**, type your server (just the host name, such as `mastodon.social`) and press **Authorize**: your browser opens at that server so you can sign in and approve Pubsplash, and the authorization comes straight back — there is nothing to copy or paste. **Unlink** removes it again, asks the server to cancel it, and can be used at any time.

- Under **Announcements**, two checkboxes: "Post to Mastodon when I start a new stream" and "Make periodic still-streaming Mastodon posts". Checking the second one makes a dropdown next to it available — every hour, every one-and-a-half hours, every two, two-and-a-half, three, or five hours. These two boxes set how the matching boxes in the **Set stream info** dialog start out; that dialog now has a **Mastodon** group of its own with "Post to Mastodon when this stream starts" and "Post periodic still-streaming announcements", so you can turn either off for a single stream without changing your defaults. Both are unavailable until an account is linked.

- Under **Templates**, the wording of the announcements. **Add** and **Edit** open a dialog with an announcement type — start of stream, or stream continuation — and the text. Write a token in curly braces anywhere you want a real value: `{title}`, `{description}`, `{url}`, and `{tod}`, which becomes morning, afternoon, evening or night depending on the time where you are. A **Help** button in that dialog lists every token and what it does, in a box you can select and copy from. If you misspell a token or leave a brace unclosed, choosing OK tells you which one and puts you straight back in the dialog with your text still there, so a typo takes one correction rather than a retype. The list on the Mastodon tab shows the type, a colon, and the text, sorted so all the start-of-stream ones come first. You can keep as many of each type as you like; when Pubsplash needs one it picks at random from that type, so repeat announcements do not all read the same. Deleting all of them is safe — Pubsplash falls back on wording of its own.

- **Every post Pubsplash makes ends with `#PubsplashStreamInfo`**, whether or not you type it. This cannot be turned off. It is there so that anyone who would rather not see automated posts can filter them out of their timeline.

- Announcements are only ever made while you are actually streaming, and a start-of-stream post waits until the stream is fully up so the link in it always works. If a stream stops and starts again with the same title within a minute, nothing is posted — that is a reconnect, not a new broadcast. If it starts again with the same title after longer than that, Pubsplash asks whether you would like to post about resuming, and offers a one-off message box pre-filled with "I've resumed my #Audiopub stream! Tune in at {url}", selected so you can type straight over it. Answering No, or cancelling, posts nothing. Pubsplash will not post to Mastodon twice within thirty seconds under any circumstances; that limit is fixed and cannot be changed.

- A **Go to** menu (`ALT+G`) in the menu bar. **Go to stream page** (`S`) opens the page listeners see for the stream you are broadcasting in your default browser; if you are not streaming, or the stream is still connecting, or you are streaming straight to an Icecast server and so have no Audio Pub page, it says which of those is the case rather than doing nothing. **Go to Pubsplash data directory** (`D`) opens the folder holding your settings, the log files, and any crash dumps — the one to open when you are asked for a log.

- The Chat tab has a **Reconnect chat** button (`ALT+O`) that opens a fresh connection to the chat feed on the spot. Your stream is not touched — chat and audio travel over separate connections — so this costs your listeners nothing. Pubsplash now reconnects on its own whenever the feed drops, so this is there for when you would rather not wait.

### Fixes

- **Authorizing against something that is not a Mastodon server now says so.** Typing an address that is not a Mastodon server — a typo, or a plain web site such as `google.com` — answered with that site's own error page pasted into a message box, starting "The server answered 404:" and followed by a couple of hundred characters of raw HTML, read out one angle bracket at a time. Pubsplash now asks the address whether it speaks the Mastodon API before anything else happens, so a wrong address is caught before your browser is ever opened, and the message is a single sentence: which host it was, what gave it away, and that the address is the thing to check. Servers that cannot be reached at all are separated out too — "that host name could not be found", "the connection was refused", "it did not answer in time", "the secure connection could not be set up" — instead of a line of network debugging. Mastodon-compatible servers such as Pleroma, Akkoma, GoToSocial and Firefish are still accepted. Whatever the reason, the cursor goes back to the Server box so you can correct it straight away.

- **No Mastodon message can read a web page at you any more.** Any error from a Mastodon server — while authorizing, while posting, or while unlinking — that turned out to be a web page rather than a message is now reported as such rather than quoted. Refusals also say what kind they were, so a rate limit, a server having trouble, and a request that was turned down no longer all read the same.

- **A recording that cannot start now says why.** If the Recording folder in Preferences pointed somewhere that does not exist — a mistyped path, or a drive that is not plugged in — pressing Start recording did nothing at all: no file, no message, nothing to go on. A message box now names the folder, says whether it is missing or cannot be written to, and points at the Recording folder setting; if the failure was the encoder rather than the folder, it says that instead. When the recording was the one that runs alongside a stream, the message also confirms that the stream itself is still live. A recording that fails *partway* through is unchanged and still reports through the Home tab and the log — that one can happen while you are talking, and a message box in the middle of a broadcast is worse than the status line.

- **A recording that fails to start no longer looks like one that is running.** Pressing Start recording turned the button into Stop recording and started the clock straight away, before anything had actually been created — so if the file could not be made, because the folder was gone or read-only or the disk was full, the window went on showing a healthy recording for the whole session and there was no file at the end of it. The Home tab now reads "Starting a recording" for the moment it takes to create the file, then "Recording" once the file genuinely exists; if it cannot be created, the button goes back to Start recording, the status says nothing about a recording, and the log says exactly what went wrong. This applies to the recording that runs alongside a stream as well as to a standalone one.

- **A recording that fails partway through now stops instead of pretending.** If the encoder failed mid-recording, Pubsplash carried on believing a recording was running: the file stayed open with nothing more written to it, the clock kept counting, and Start recording refused to work again for the rest of the session. The recording is now closed properly, so the audio recorded up to that point is a complete, playable file, and you can start a new one immediately.

- **A failed encoder is no longer a silent broadcast.** If the MP3 encoder could not be created, or failed partway through, nothing was going out — but the Home tab went on reading "Streaming" with the duration counting up, and nothing anywhere said otherwise. It now reads "Streaming (encoder failed, not sending audio)" and the log names the reason.

- **Connecting to a different service while a stream is live now ends that stream first.** Previously the old stream was left running: Pubsplash asked the *new* service to end it, which it knew nothing about, so the old stream stayed live on the server until it expired on its own — and if the new service was a plain Icecast server, there was no way to end it at all.

- **The Audio Pub site address is now properly checked.** Anything beginning with the letters "http" was accepted, including addresses that were not web addresses at all and addresses with no site name in them — and your email and password are sent to whatever that address names. Pubsplash now checks the address is a real one before sending anything, and explains what is wrong with it if not. An address beginning `http://` rather than `https://` is still allowed, since some people run their own server on a home network, but it is noted in the log because your password travels unencrypted over it.

- **A busy chat no longer creates a thread for every message.** Each sound event sent to the stream started an operating-system thread of its own, so a flood of chat messages started a flood of threads — all of them competing with the mixer at the moment it was busiest. Each sound-events source now has a single worker of its own. If cues arrive faster than they can be played, the oldest waiting ones are dropped rather than the newest, so what you hear stays in step with what is happening rather than falling further behind.

- A capture or monitoring device whose event handle stopped working could leave Pubsplash spinning a processor core forever instead of reopening the device. It now notices and goes through its normal reconnect.

- Scanning a VST plugin that printed a lot of text could hang the scan for good. Plugin output is now read as it arrives.

- **A brief internet drop no longer ends your broadcast.** Pubsplash now reconnects the outgoing audio connection on its own and keeps trying for four minutes. Because it reconnects to the same stream rather than starting a new one, your stream keeps its address, its chat history, its listener counts and its recording — listeners hear a gap and nothing else, and there is nothing for you to press. The log records when the connection drops, once more if it is still trying after two minutes, and again when it comes back, with how long the gap was. The Home tab reads "Streaming (reconnecting)" while it works, and the duration keeps counting, because the stream really is still running. If the connection cannot be restored within four minutes the stream ends and Pubsplash says so; four minutes is the limit because the server ends a stream that has been disconnected for five.

- **A drop is now noticed in seconds instead of minutes.** Sending audio had no time limit, so a dropped connection did not report an error until Windows finished retrying the send underneath — up to a couple of minutes during which Pubsplash showed a healthy stream with a running clock while listeners heard silence. Sending, connecting and the initial handshake are all now bounded.

- Streaming errors that the server explains, such as a stream key that is no longer valid or a stream the server has already ended, now report the server's own wording instead of only an error number. A connection refused because the previous one has not yet been released is now recognised as temporary and retried rather than treated as fatal.

- A streaming failure now stops the stream on the network side as well as in the window. Previously the two disagreed: the window returned to idle while the network thread still believed the stream was live, so the chat connection stayed open and the server was never told the stream had ended.

- Starting a stream while one is already live now ends the old one first instead of abandoning it with its connections still open.

- Requests to Audio Pub now have connection and response time limits, so a request that never completes can no longer leave the window stuck on "connecting" or hold up closing the app.

- **Chat could stop arriving partway through a stream and never come back.** Audio kept going out perfectly, so the only sign was silence, and the only cure was to stop and restart the stream. Pubsplash opened the chat connection once when the stream started and never checked on it again: if it was closed, or failed, or was quietly dropped by a router part way through a quiet spell, nothing noticed and nothing said so — not even the log. Pubsplash now watches the connection, reopens it when it dies, and records what happened in the log, whether that was a dropped connection it has already put right or a stream the server no longer has. A connection that is merely idle is caught too: the server sends a heartbeat every thirty seconds, so ninety seconds of true silence is now treated as a dead connection rather than a quiet room.

  One case is beyond Pubsplash's reach and it will now say so plainly. The Audiopub server can get into a state where it will not deliver messages to a listener that reconnects, and only a new stream clears it — so when Pubsplash reconnects it tells you that if messages still do not arrive, restarting the stream is the fix. This has been reported to the Audiopub developers.

- **Application sources captured nothing from browsers and Electron apps, most of the time.** Brave, Chrome, Edge, Spotify, Discord and Chromium-based games all run as a dozen or more processes under one name, and Pubsplash picked whichever of them the system happened to hand it first. It captures the chosen process *and everything it started*, so picking the main window's process caught the lot — and picking any of the others caught nothing at all, silently: no error, no warning, just a source that never made a sound. Which one it picked was effectively random and could change from one moment to the next, which is why the same app captured for one person and not another, and why it sometimes stopped working partway through a stream. Pubsplash now works out which process started the rest and captures that one, so the whole application is picked up however many processes it runs. When you have two copies of the same application open, it prefers the one that is actually playing sound, and having chosen, it stays with that copy until you close it.

- A related problem meant that changing an Application source could leave it silent for a couple of seconds and put "Process not running; source will be silent" in the log for an application that was plainly running. Pubsplash checks which applications are running in the background so the app never pauses while it does; if you changed a source while one of those checks was in flight, the older answer arrived afterwards and overwrote the newer one. Answers that have been overtaken are now discarded.

- Chat problems no longer stop a broadcast before it starts. If the chat feed could not be opened when a stream began, the whole stream start failed. Streaming now begins regardless and the chat feed connects on its own, reporting in the log if it cannot.

- The link to a live stream was built from the internal id of the streaming service rather than its address. For the built-in Audiopub site the two happen to be the same, so it was correct there and wrong everywhere else: a self-hosted service would have produced a link like `service-2/live/abc123`. Nothing showed the link before now, so this was never visible; the new Mastodon announcements use it.

- Scanning for plugins could hang at 0% and never finish, filling the log file with errors while the scan itself ran on invisibly in the background. The scan dialog has been rebuilt from scratch to fix it. It is now an ordinary Pubsplash dialog — a progress bar, a status line naming the plugin being loaded, and Skip and Cancel buttons — rather than the Windows progress dialog, which turned out not to be able to show a Skip button at all: asking for one stopped the dialog appearing. **Skip** (or ENTER) gives up on a plugin that is taking too long and moves to the next one; **Cancel** (or ESCAPE) stops the scan. Both now work even while the scan is stuck inside a plugin. The status line is read-only and does not read itself out as it changes, so you can tab to it whenever you want to know where the scan has got to without it interrupting you.

- The grouped sections of Preferences and of the Text-to-Speech source dialog are now real groups as far as a screen reader is concerned. Stream Archiving, Recording, Limits, Sound pack and Interface sounds in Preferences, and the per-engine voice settings for ElevenLabs, OpenAI, Azure, Google Cloud, AWS Polly and Google Translate, previously drew a labelled box around their controls that was purely visual — the label was never announced. Tabbing into one of these sections now announces the section name, so it is clear which group a setting belongs to. Nothing has moved: the controls are in the same order and announce their own names exactly as before.

- Pressing "Scan for new plugins" or "Rescan all plugins" more than once started that many scans at the same time, all racing over the same plugins and each running its own scanning processes — which made scanning far slower and could make plugins fail to scan that would otherwise have been fine. The second and later presses are now ignored while a scan is running, as they were always meant to be.

### Changes

- Releases are now checked before they are built: the tag has to be a plain version number, it has to match the version inside Pubsplash, and the changelog has to have a matching heading with nothing left under Unreleased. The test suite also runs before anything is packaged, so a release can no longer be published without it having passed.

- Pubsplash is now built with link-time optimization, which lets the compiler optimize across the whole program rather than one piece at a time. This mainly benefits the audio mixer and the MP3 encoder, which do their work in small steps every ten milliseconds.

- Releases now carry a separate `pubsplash-<version>-debug-symbols.zip`. It is not needed to run Pubsplash and most people should ignore it; it is what turns a crash dump into a readable report, so it is worth mentioning if you are reporting a crash.

- The source code now passes Clippy, Rust's linter, with no warnings left. Nothing Pubsplash does has changed; this is tidying, so that a real problem the linter finds in future is not lost among hundreds of harmless notes.

## 0.1.3

### Additions

- `ENTER` now presses the OK button of every dialog in Pubsplash, the counterpart to `ESCAPE` closing them. Stream info, the Text-to-Speech and Sound Events source dialogs, a source's sends, the application picker, Load chain, the missing-plugin notice, Add streaming service, and Add/Edit binding all confirm on `ENTER`; the dialogs that only close — Preferences, the chat message viewer, and an effect's parameters — close on it. Where a dialog checks what you entered, `ENTER` goes through the same check a click does and leaves the dialog open if something is wrong. `ENTER` keeps its existing meaning where a control has one: it inserts a line break in the stream description and in a viewed chat message, sends a chat message, commits a typed effect parameter, and is recorded as a shortcut in the binding capture box. In the streaming services dialog `ENTER` closes and saves rather than connecting, so it can never start or stop a connection by accident.

- Added an **API** tab, last on the tab bar, showing what each speech engine has been asked to do since Pubsplash started. Only engines that have actually spoken appear, most recently used first, and each is followed by its requests sent, characters sent, credits spent, remaining balance, models used, voices used, and failures. **Refresh balances** (`ALT+F`) asks each provider that publishes one how much credit is left; only ElevenLabs reports a balance, and anything a provider does not report reads as "unavailable" rather than as a zero. Nothing is fetched unless you press the button, and none of it is kept between sessions.

- Added validation for OpenAI, ElevenLabs, Azure, AWS Polly, and Google Cloud credentials before they are saved.


- Added per-source voice settings for ElevenLabs, OpenAI, Azure, Google Cloud, AWS Polly, and Google Translate.

- A Text-to-Speech source now keeps its settings for every engine, not just the one it is using. Voice, volume, rate, pitch and the engine's own settings are saved in a section per engine, so switching a source to another engine and back finds the first engine as you left it instead of reset to defaults.

- Added **Reset this engine to defaults** (`ALT+R`) to the Text-to-Speech source dialog. It resets the voice, volume, rate, pitch and settings of the engine currently selected, leaving every other engine's saved settings alone, and like the rest of the dialog it applies only when you press OK.

- ElevenLabs speech now starts playing as it is generated instead of after the whole message has been synthesized, which removes most of the delay before a chat message is read. "Stream audio as it is generated" is in the ElevenLabs settings of the Text-to-Speech source dialog and is on by default; it is unavailable for Eleven v3, which has no streaming endpoint.

### Fixes

- Fixed the ElevenLabs voice list emptying out when the model is set to Eleven v3. ElevenLabs publishes, per voice, the models it holds a high-quality rendition for, and Pubsplash was reading that as the set of models the voice could be used with — no premade voice names Eleven v3 in it, so choosing v3 left the picker holding little but your own cloned voices. Every voice on your account is offered for every model now, which is how ElevenLabs actually works.
- Fixed the ElevenLabs voice list stopping short on large accounts. Only the first page was ever read, because the voice list was requested from an older endpoint that returns no page markers.
- The voice count beside the voice picker now counts the voices actually in the picker, and is brought up to date when you change the model. On AWS Polly, where voices really are limited to particular engines, it could report every voice on the account next to a list holding a handful.
- Pressing ENTER to send a chat message no longer plays the Windows error sound alongside sending it. The same silence now applies to the value box in an effect's parameters dialog.
- Chat is now read aloud to you on every speech engine. Only SAPI 5 spoke to the broadcaster; the other eight engines were mixed into the stream and nowhere else, so unless you had thought to monitor the text-to-speech source's mixer strip, chat appeared never to be read at all. A text-to-speech source is now always played to you, whatever engine it uses and whether or not it is being sent to the stream.
- Speech kept off the stream no longer reaches it through a bus. "Send speech to the stream" dropped the source from the master mix but left its sends alone, and every bus mixes into master, so a text-to-speech source with a send was heard by listeners with the box unchecked.

- Stopped the chat message list from reading its selected message out once a second. A message's relative time changes every second for its first minute, and rewriting the row to show it was announced whether or not the list had focus; the selected row is now left alone and brought up to date when you move off it or when focus arrives on the list.
- ElevenLabs sources are now named by their voice, not by its identifier: the Sources list and the mixer strip say "Rachel" where they used to read out a 20-character key. Until the voice list has been fetched the label leaves the voice out rather than naming the key.
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

- Your Audio Pub password and your Icecast source password are now encrypted in the settings file, like the speech-engine API keys already were. Both were being stored as plain readable text, so anyone who could open `config.json` — or who was sent a copy of it — could read them. They are now tied to your Windows account, which means a copy of the settings file is of no use on another machine or under another account. Nothing is required of you: passwords already saved still work and are re-encrypted the next time your settings are saved.
- "Send speech to the stream" now decides only whether your listeners hear the speech. Whether you hear it is no longer tied to it, and with it unchecked the speech reaches neither the stream nor any bus the source sends to.
- SAPI 5 speech now goes through its mixer strip like every other engine's rather than being spoken separately, so the source's volume and mute apply to what you hear as well as to what your listeners hear. If SAPI fails, the reason now appears in the chat list alongside the other engines' rather than only in the log.
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

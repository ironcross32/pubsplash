# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Pubsplash is an accessibility-first Windows streaming client for Audio Pub (an Icecast-based livestreaming platform, server source: github.com/the-byte-bender/audiopub-sv), built with wxDragon (wxWidgets bindings). Its users are primarily screen-reader users; every control must be Tab-reachable and properly announced. The full product spec is in `project2.md` (`project.md` is an earlier draft of the same spec).

## Commands

- Build: `cargo build` (first build compiles wxWidgets via CMake/Ninja and takes several minutes; later builds are fast)
- Test: `cargo test`
- Single test: `cargo test <name_substring>`
- Ignored tests (hit the real audiopub.site or play audio): `cargo test <name> -- --include-ignored`. Notable: `real_login_bad_credentials` (live server), `sapi_speaks` (audible), `sapi_synthesizes_pcm`
- Run: `./target/debug/pubsplash.exe` (release builds hide the console via `windows_subsystem`)

Per `agents.md`: every change set must update `changelog.md` (bulleted entries under `## Unreleased` → `### Additions`/`### Fixes`/`### Changes`) and keep `README.md` accurate (especially the shortcut table).

## Architecture

Three long-lived domains connected by channels; the UI never blocks on audio or network:

1. **UI (main thread)** — `src/ui/`. wxdragon main loop. A 100 ms `Timer` ("the pump", in `ui/mod.rs::pump_events`) drains `NetEvent`/`EngineEvent` receivers into UI state; once a second it refreshes durations and chat relative times. `App` (an `Rc` holding `RefCell`s) is the shared UI-side state: persisted `Config`, transient `Runtime`, handles to the engine/net/speaker threads, and `Widgets` (populated after `ui::build`).
2. **Audio engine (thread)** — `src/audio/`. `engine_loop` mixes 10 ms blocks: per-source WASAPI capture threads push f32 into rtrb rings → per-source `ChannelStrip` (volume + 50 ms mute fades) → master strip → optional LAME MP3 encode → tokio mpsc to the Icecast sender. Sources that aren't OS-captured (TTS, sound events) are `FeedKind::External`: the engine parks an rtrb producer in `ExternalFeeds` keyed by source name, and other subsystems push samples via `ExternalFeeds::push` (retry on `Full`, abort on `Gone`).
3. **Network (tokio on a background thread)** — `src/net/`. `net_loop` owns the `AudioPubClient` (reqwest + cookie store) and, while streaming, two tasks: the Icecast source connection (hand-rolled `PUT` over `TcpStream` in `icecast.rs`) and the SSE consumer (`sse.rs` parser over `/live/{id}/events`).

TTS (`src/tts/sapi.rs`) runs on its own COM STA thread owning `ISpVoice`: requests arrive over a channel; each chat message is spoken locally (async) and, when `output_to_stream`, synthesized to memory at 48 kHz stereo and trickle-fed into the engine via `ExternalFeeds`.

`Config` (in `config.rs`) is both the persisted settings file and the scene/source model; `state.rs` adds list-manipulation methods (`switch_to`, `delete_scene`, `move_up`...) that return `ListEdit::Changed/Unchanged` so the UI knows whether to refresh/save. After any scene/source edit, call the chain in `ui/scenes.rs::after_scene_edit` (save → refresh both lists → `on_sources_changed` → `sync_engine_sources` + `rebuild_mixer`).

## Hard-won protocol facts (verified against the real server; do not "simplify" these away)

- The server kills any live source that isn't **MP3 or AAC** (it ffprobes the mount). MP3 via LAME is the default; the spec's mention of Opus is unimplementable.
- SvelteKit form actions (`/login`, `/live/new`) return **200 + JSON envelope** (`{"type":"redirect"|"failure",...}`) to non-browser clients, not a real 303. Parse the envelope (`audiopub.rs::action_result`), never trust the HTTP status alone.
- Chat SSE user objects contain **both `name` and `displayName`** — they must be separate serde fields (an alias makes serde reject the payload as a duplicate field, silently dropping every message).
- `__data.json` responses use SvelteKit's devalue flat-array encoding, and multiple nodes can carry a `user` object; only the stream-instructions page node has `streamKey` — skip non-matching nodes, don't bail.
- Connecting to the SSE endpoint counts as a listener; subtract 1 for display.
- Desktop Audio capture uses **process-exclusion loopback** (`new_application_loopback_client(std::process::id(), false)`) so Pubsplash's own output (TTS, cues) can never loop into the stream. Don't switch it back to plain render-device loopback.

## wxDragon notes

- Widget handles are `Copy`-like and cheap to clone into event closures; state lives in `Rc<App>` captured by clone.
- Screen readers do not announce adjacent `StaticText` labels or even some controls' own labels. Use `ui/mod.rs::set_accessible_name` (a name-only MSAA `AccessibleImpl`) on any new slider, checkbox, list, or text input — and re-set it when the meaning changes (see the send-level slider re-label in `sends.rs`). The crate's `set_accessibility_label` is macOS-only; don't use it.
- Keyboard shortcuts are mostly mnemonics in labels (`"S&witch to scene"` = ALT+W). The context-aware stream button relies on this: `&Start streaming` (ALT+S) vs `S&top streaming` (ALT+T).
- Key events: match through `ui/mod.rs::key_of` (returns `(key_code, ctrl_down)`); call `event.skip(true)` on unhandled keys or the control goes dead.
- Dialog outcome constants are `ID_OK`/`ID_YES`/`ID_CANCEL`; modal dialogs need explicit `.destroy()` after `show_modal()`.
- The mixer is rebuilt (not patched) on any source change — `rebuild_mixer` destroys and recreates an inner panel so creation order = Tab order.
- The frame `on_close` handler vetoes via `WindowEventData::General(e).veto()` when the user declines the streaming-exit confirmation.

## Buses, sends, and VST FX (implemented)

- **Routing** lives in `config.rs`: global `BusesConfig` (buses + `master_chain`), per-source `sends: Vec<SendConfig>` (bus by *name*) and `to_master`. Bus list edits are on `Config` (in `state.rs`) because rename/delete rewrites sends. `Config::fix_up_routing` (called in `load_from`) drops dangling sends.
- **Engine graph** (`audio/mod.rs::mix_one_block`): source strip → (if `to_master`) master, plus post-fader per-send `ChannelStrip` into each bus buffer; each bus runs its `FxChain` → bus strip → master; then the master `FxChain` → master strip. Buses/sources are addressed **by index**; sends carry a `bus_index` resolved from names in `ui/mod.rs::sync_engine_sources`. Denormals are flushed (FTZ/DAZ) at the top of `engine_loop`.
- **Plugin hosting is in-process** and VST2-only so far (VST3 identity is stored but not processed). `vst/host2.rs` is the real host (full `AEffect`, editor, chunks) — distinct from the scan-only `vst/vst2.rs` that compiles into `pubsplash-scan.exe`. **Threading contract** (documented at the top of `host2.rs`, enforced by structure): `load`/`Drop`/dispatcher/editor on the **UI thread**; `process_replacing` on the **engine thread**; `get/set_parameter` either. `Vst2Plugin` is `unsafe Send+Sync` on that basis.
- **Ownership**: `App.fx` (`FxRuntime`) holds one `Arc<Vst2Plugin>` per live instance, kept index-aligned with `config.buses`. The engine holds clones inside `FxChain`s. Removing a plugin parks its Arc in `FxRuntime.retiring`; the engine sends `EngineEvent::BusesApplied` after each `SetBuses`/`SetMasterChain`, and the pump then drops the retired Arcs (UI thread = last owner). Never let the engine drop the last reference. All chain lifecycle goes through `ui/fx.rs`.
- **Chain library** (`fx.rs`): named chains in `fx_chains.json`, `.pubfx` export/import, and `resolve_chain` (drives the missing-plugin dialog). `PluginRef::resolve` matches by unique_id/class_id, then path, then name.
- **Accessible params** (`ui/fx_params.rs`): OSARA-style dialog behind the testable `ParamSource` trait (`step_param`, `filter_params`, `formatted_value`). Announces via `set_accessible_name_value` (re-set on every change) plus a read-only value TextCtrl fallback.
- **Native editor** (`ui/fx_editor.rs`): plugin editor embedded in a wx `Frame` panel via `get_handle()` HWND; a `WH_KEYBOARD_LL` hook (installed only while editors are open) turns **F6** into "refocus the toolbar" so keyboard focus is never trapped. `editor_idle` and `audioMasterSizeWindow` are serviced from the 100 ms pump. Structural chain edits call `fx_editor::close_all` first (editor slot indices would go stale).

## Deferred by spec (don't build unless asked)

VST3 *processing/hosting* (identity is already stored; `FxChain` would grow an instance-kind enum), sound-event source configuration UI, the Preferences screen beyond VST folders, sound pack creation tool, OneCore/Edge/Piper TTS engines, AAC encoding.

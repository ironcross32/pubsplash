# Pubsplash

Pubsplash is an accessibility-first Windows app for streaming audio to [Audio Pub](https://audiopub.site/), the open-source audio sharing and livestreaming platform.

It is built in Rust with the wxDragon UI toolkit, and designed from the ground up to work well with screen readers such as NVDA and JAWS.

## Features

- Stream live audio to audiopub.site or any self-hosted Audio Pub instance
- MP3 encoding with configurable bitrate
- Scenes: group any number of audio sources and switch between them
- Source types: microphone, desktop audio, per-application audio, text-to-speech, and sound events
- Sources name themselves after what they capture — the microphone device, the running application's real name, the chosen speech voice — so the mixer's sliders say exactly what you are adjusting even with several of the same type in a scene
- Chat: read incoming messages in an accessible list, send outbound messages, and have chat read aloud automatically with text-to-speech (optionally spoken into the stream as well)
- Loop-safe by design: Desktop Audio capture excludes Pubsplash's own audio, so text-to-speech and sound cues can never echo into your stream
- Audio cues for stream events (listener changes, incoming and outgoing messages), with custom sound pack support planned for the future
- A fully keyboard-accessible mixer with per-source volume and mute, plus an optional per-strip volume boost for sources that are too quiet at 100%
- Mixing buses with per-source sends, so sources can be routed through shared processing (see below)
- VST2 effect chains on buses and the master output, with a screen-reader-friendly Osara-like parameter editor and shareable effect chains
- Status bar shows your stream status, quality, and duration

## Requirements

- Windows 10 or 11
- An Audio Pub account (your account must be trusted to stream)

## Getting started

1. Launch Pubsplash.
2. Open **File → Configure Audio Pub**, select the site, enter your email and password, and press **Connect**.
3. Optionally open **File → Set stream info** to set the stream's title, description, streaming quality (MP3 bitrate), whether the stream should be archived on the server, and whether to **record this stream** to a file on your computer. The title, description, archive, and record choices reset every time Pubsplash starts; the quality setting is saved and persists across sessions. To have archiving or recording pre-selected each launch, enable **Archive streams by default** or **Record streams by default** on the Archiving tab of **File → Preferences** (`CTRL+,`). Recordings are an exact copy of the streamed MP3, saved as `recording_<yyyy-mm-dd>_<HH-MM-SS>.mp3` in the recording folder set on that same tab (your Music library by default).
4. On the **Home** tab, press **Start streaming** (`ALT+S`). If you haven't set the stream info yet, the dialog opens first — press **OK** to start with what's filled in (tabbing into a text field selects its contents so you can just type over the defaults), or **Cancel** to not start streaming.
5. Press **Stop streaming** (`ALT+T`) when you're done.

To record locally without going live, use **Start recording** (`ALT+R`) next to the streaming button; press **Stop recording** (`ALT+C`) to finish. It saves the same MP3 to your recording folder without connecting to the server. Recording and streaming can't run at the same time.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `F1` | Speak context-sensitive help for the focused control |
| `ALT+S` / `ALT+T` | Start / stop streaming (Home tab) |
| `ALT+R` / `ALT+C` | Start / stop recording without streaming (Home tab) |
| `ALT+W` | Switch to the selected scene (Home tab) |
| `ALT+V` | View the focused chat message in a window (Chat tab) |
| `Escape` | Clear the chat input box |
| `CTRL+,` | Open Preferences |
| `CTRL+Up` / `CTRL+Down` | Move the focused scene, source, bus, or plugin |
| `Delete` | Remove the focused scene, source, bus, send, or plugin |
| `CTRL+Tab` / `CTRL+Shift+Tab` | Next / previous parameter (plugin parameter dialog) |
| `F6` | Move focus out of a plugin's own interface back to the toolbar |
| `SHIFT+F10` / `Applications` | Open the context menu for the focused control (mixer volume sliders) |

In the mixer, sliders respond to arrow keys for 1% steps, `Page Up` / `Page Down` for 10% steps, and `Home` / `End` for maximum / minimum volume. `Up`, `Right`, and `Page Up` always raise the volume; `Down`, `Left`, and `Page Down` always lower it.

### Volume boost

Mixer volume sliders normally stop at 100%, which is unity gain — the source's own level, unchanged. If a source is still too quiet there (a microphone or a capture device that is simply low at the Windows level), open the slider's context menu with `SHIFT+F10`, the `Applications` key, or a right click, and choose **Enable volume boost**. The item shows a check mark while boost is on, and that slider then runs from 0% to 500%, amplifying the signal by up to five times.

Boost is remembered per strip — Master, each source, and each bus have their own setting, saved with your configuration. Turning it back off snaps the slider down to 100% if it was higher. Note that the boost is a plain gain stage with no limiter, so amplifying an already-loud source will distort it; back the slider off until it sounds clean.

## Buses and sends

A **bus** is a shared mixing point that sources can feed and that hosts a chain of VST effects. Every bus outputs to the master mix.

Open the **Buses** tab to create and manage buses: **Add bus**, **Rename bus**, **Remove bus**, and reorder with **Move up** / **Move down** (or `CTRL+Up` / `CTRL+Down`; `Delete` removes the focused bus). Each bus appears in the Home mixer with its own volume slider and mute button, after the sources.

To route a source into a bus, select the source on the **Scenes and Sources** tab and press **Sends...**. In that dialog you can add a send to any bus, set its level, remove sends, and toggle **Send directly to master**. Leave "Send directly to master" on for aux-style routing (the source is heard directly, and the bus adds an effect such as reverb); turn it off to route the source only through its buses (insert-style, for processing a microphone with EQ or compression, for example).

## Effects (VST plugins on buses)

Each bus — and the master output — can run a chain of VST2 effects. On the **Buses** tab, select a bus (or the pinned **Master output** row) and use the effects list below it:

- **Add plugin** inserts a scanned VST2 plugin at the end of the chain.
- **Move plugin up** / **down** (or `CTRL+Up` / `CTRL+Down`) reorder the chain; effects are applied top to bottom.
- **Bypass** turns an effect off without removing it; **Remove plugin** (or `Delete`) takes it out.

Effects process live, including while you are streaming. Each plugin's settings are remembered between sessions.

(Only VST2 plugins can be inserted in this version. VST3 plugins are still catalogued by the scanner and will become insertable in a later release.)

### Adjusting plugin parameters

Two ways to control a plugin, both reachable from the effects list:

- **Edit parameters** opens a dialog that works with every plugin and is designed for screen readers. Type in the **Filter** box to narrow the parameter list, choose a parameter, and adjust its **Value** with the arrow keys (Page Up/Down for larger steps, Home/End for maximum/minimum). Press `CTRL+Tab` / `CTRL+Shift+Tab` to move to the next or previous parameter from anywhere in the dialog. The parameter's name and its formatted value are announced as you change it. Turn on **Show unnamed parameters** to reveal parameters the plugin didn't give proper names.
- **Open interface** shows the plugin's own window, for plugins that have one (many do not). Because a plugin's own interface can trap the keyboard, press **F6** at any time to move focus back to the window's toolbar (Parameters, Bypass, Plugin interface, Close), from which you can Tab normally or return to the plugin.

### Sharing effect chains

Use the chain library buttons under the effects list to reuse setups:

- **Save chain** stores the current chain in your library under a name.
- **Load chain** applies a saved chain to the selected bus (with a Load / Delete picker).
- **Export chain** writes the current chain to a `.pubfx` file you can copy to another machine.
- **Import chain** reads a `.pubfx` file into your library and offers to apply it.

When a chain you load or import uses plugins that aren't installed on this machine, Pubsplash lists the missing ones and lets you apply the chain with just the plugins you do have, or cancel. Chains are stored together in `%LOCALAPPDATA%\pubsplash\fx_chains.json`.

## VST plugins

Open **File → Preferences** (`CTRL+,`) and choose the **VST plugins** tab to tell Pubsplash where your plugins live. The folder list starts out with the standard Windows VST locations that exist on most machines ; add or remove folders as needed (`Delete` removes the focused folder).

Press **Scan for new plugins** to scan only files that haven't been scanned before, or **Rescan all plugins** to start over. Scans are able to be canceled at any time. If a plugin is taking too long and you suspect it's not going to scan, you can skip it. If a scan runs to completion, a cache will be written alongside Pubsplash's configuration file and these plugins will become available to use immediately.

## Configuration

Pubsplash stores its configuration data in `C:\Users\<Your-user-name>\AppData\Local\pubsplash`.

**config.json** is where all of your app settings live. It holds things like preferences, your Audiopub credentials, your scenes and sources, and so on. It is written when the app is first launched. Pubsplash will also regenerate it if it becomes missing or if it is found to be corrupt. In the latter case, the corrupted file will be renamed and given a .bak extension, allowing you to fix it if you so choose.

**vst_plugins.json** stores the plugin cache. It is written when a scan runs to completion. Pubsplash uses this to determine which plugins to offer when you go to add one to a bus.

**fx_chains.json** stores the FX chains you create. You can export one chain at a time or import chains and they will be added to this file.

## Logging

Logs are written to `%LOCALAPPDATA%\pubsplash\logs\`. The log level can be changed in the app, or forced with an environment variable such as `PUBSPLASH_LOG_TRACE=1` (which overrides the in-app setting). Levels: `off`, `error`, `warn`, `info`, `debug`, `trace`.

## Building from source

Prerequisites: Rust (stable), Visual Studio 2019+ with the Windows SDK, CMake, and Ninja. Then:

```
cargo build --release
```

The first build downloads prebuilt wxWidgets libraries automatically.

## License

See the repository for license details.

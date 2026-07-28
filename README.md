# Pubsplash

Pubsplash is an accessibility-first Windows app for streaming audio to [Audio Pub](https://audiopub.site/), the open-source audio sharing and livestreaming platform.

It is built in Rust with the wxDragon UI toolkit, and designed from the ground up to work well with screen readers such as NVDA and JAWS.

## Features

- Stream live audio to audiopub.site or any self-hosted Audio Pub instance
- MP3 encoding with configurable bitrate
- Scenes: group any number of audio sources and switch between them
- Source types: microphone, desktop audio, per-application audio, text-to-speech, and sound events
- Applications are picked from a list of what is running â€” by default just the ones that have actually played sound â€” rather than typed from memory
- Sources name themselves after what they capture â€” the microphone device, the running application's real name, the chosen speech voice â€” so the mixer's sliders say exactly what you are adjusting even with several of the same type in a scene
- Sources reconnect on their own if their device is unplugged, resets, or is not ready yet when Pubsplash starts. A source that is retrying reads "(reconnecting)" on its mixer strip, and a source set to a particular microphone is never silently switched to a different one
- Chat: read incoming messages in an accessible list, send outbound messages, and have chat read aloud automatically with text-to-speech (optionally spoken into the stream as well)
- Nine speech engines: SAPI 5 and Microsoft Edge need no setup, and Google Translate, OpenAI, ElevenLabs, Azure, AWS Polly, Google Cloud, and a self-hosted Star server are available once you enter their credentials on the Speech tab of Preferences. Keys are encrypted for your Windows account
- Loop-safe by design: Desktop Audio capture excludes Pubsplash's own audio, so text-to-speech and sound cues can never echo into your stream
- Built-in startup and shutdown sounds, each able to be switched off, plus audio cues for stream events (listener changes, incoming and outgoing messages) that you can send to your listeners or keep to yourself
- A fully keyboard-accessible mixer with per-source volume and mute, plus an optional per-strip volume boost for sources that are too quiet at 100%
- Empty lists say what they are: rather than leaving a screen reader with nothing to announce, a list with nothing in it holds a single row naming what belongs there ("No chats", "No sources", "No plugins")
- Mixing buses with per-source sends, so sources can be routed through shared processing (see below)
- VST2 effect chains on buses and the master output, with a screen-reader-friendly Osara-like parameter editor and shareable effect chains
- Status bar shows your stream status, quality, and duration

## Requirements

- Windows 10 or 11
- An Audio Pub account (your account must be trusted to stream)

## Getting started

1. Launch Pubsplash.
2. Open **File â†’ Configure Audio Pub**, select the site, enter your email and password, and press **Connect**.
3. Optionally open **File â†’ Set stream info** to set the stream's title, description, streaming quality (MP3 bitrate), whether the stream should be archived on the server, and whether to **record this stream** to a file on your computer. The title, description, archive, and record choices reset every time Pubsplash starts; the quality setting is saved and persists across sessions. To have archiving or recording pre-selected each launch, enable **Archive streams by default** or **Record streams by default** on the Archiving tab of **File â†’ Preferences** (`CTRL+,`). Recordings are an exact copy of the streamed MP3, saved as `recording_<yyyy-mm-dd>_<HH-MM-SS>.mp3` in the recording folder set on that same tab (your Music library by default).
4. On the **Home** tab, press **Start streaming** (`ALT+S`). If you haven't set the stream info yet, the dialog opens first â€” press **OK** to start with what's filled in (tabbing into a text field selects its contents so you can just type over the defaults), or **Cancel** to not start streaming.
5. Press **Stop streaming** (`ALT+T`) when you're done.

**Help → Open Readme** and **Help → View Changelog** open this document and the changelog in your default browser. Both are installed with Pubsplash and match the version you are running; if a copy is unavailable, Pubsplash opens the one on GitHub instead.

To record locally without going live, use **Start recording** (`ALT+R`) next to the streaming button; press **Stop recording** (`ALT+C`) to finish. It saves the same MP3 to your recording folder without connecting to the server. Recording and streaming can't run at the same time.

The **Stream overview** at the top of the Home tab is a list you can arrow through, one fact per row. The status row is always there and says whether you're streaming, recording, or both; listener and listener peak rows appear while you're streaming; and a duration row shows how long the current stream or recording has been running. The duration counts up in the background, but the row you have selected is left alone until you go to it, so nothing in this list ever reads itself out at you. It's brought up to date the moment before you'd hear it: arrow onto the row, or tab into the list, and it reads the time as it is now, once.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `F1` | Speak context-sensitive help for the focused control |
| `F6` / `SHIFT+F6` | Move to the next / previous list on the current tab, and past the last one to the tab bar |
| `ALT+S` / `ALT+T` | Start / stop streaming (Home tab) |
| `ALT+R` / `ALT+C` | Start / stop recording without streaming (Home tab) |
| `ALT+W` | Switch to the selected scene (Home tab) |
| `ALT+V` | View the focused chat message in a window (Chat tab) |
| `Escape` | Close the current dialog; in the chat input box, clear the box |
| `CTRL+,` | Open Preferences |
| `CTRL+M` | Toggle monitoring for the focused mixer strip |
| `ALT+G` / `ALT+P` | Get available voices / preview the voice (Text-to-Speech source dialog) |
| `ALT+I` / `ALT+K` | Import / remove a sound pack (Preferences, Sound packs tab) |
| `CTRL+Up` / `CTRL+Down` | Move the focused scene, source, bus, or plugin |
| `Delete` | Remove the focused scene, source, bus, send, or plugin |
| `CTRL+Tab` / `CTRL+Shift+Tab` | Next / previous parameter (plugin parameter dialog) |
| `F6` | Inside a plugin's own interface only: move focus back out to the toolbar |
| `Escape` | Close a plugin's interface window (from its toolbar; inside the plugin's own interface, `Escape` goes to the plugin) |
| `SHIFT+F10` / `Applications` | Open the context menu for the focused control (mixer volume sliders) |

In the mixer, sliders respond to arrow keys for 1% steps, `Page Up` / `Page Down` for 10% steps, and `Home` / `End` for maximum / minimum volume. `Up`, `Right`, and `Page Up` always raise the volume; `Down`, `Left`, and `Page Down` always lower it.

### Volume boost

Mixer volume sliders normally stop at 100%, which is unity gain â€” the source's own level, unchanged. If a source is still too quiet there (a microphone or a capture device that is simply low at the Windows level), open the slider's context menu with `SHIFT+F10`, the `Applications` key, or a right click, and choose **Enable volume boost**. The item shows a check mark while boost is on, and that slider then runs from 0% to 500%, amplifying the signal by up to five times.

Boost is remembered per strip â€” Master, each source, and each bus have their own setting, saved with your configuration. Turning it back off snaps the slider down to 100% if it was higher. Note that the boost is a plain gain stage with no limiter, so amplifying an already-loud source will distort it; back the slider off until it sounds clean.

## Capturing an application

Add an **Application** source on the **Scenes and Sources** tab (**Add source**, then pick "Application") to stream one program's audio while leaving the rest of your system out of the mix.

The picker that opens lists the applications you can capture, each read as its real name followed by its program file â€” "Firefox (firefox.exe)". By default it offers only the applications that have played sound since they started, which is usually a handful; that is the **Only show apps that have played sound** checkbox, and unchecking it widens the list to every open application, including ones that have not made a sound yet. Windows' own components never appear in either view, and Pubsplash never offers itself. Press **Refresh** after starting an application, or after playing something in one, to look again.

**Type a name...** enters a program name by hand, for an application that is not running yet â€” the source stays silent until that program starts and then picks it up on its own, without any further editing. Editing an Application source later reopens the picker with its application already highlighted; if that application has since closed, it appears as a "(not running)" entry at the top, so cancelling never loses the setting.

## Buses and sends

A **bus** is a shared mixing point that sources can feed and that hosts a chain of VST effects. Every bus outputs to the master mix.

Open the **Buses** tab to create and manage buses: **Add bus**, **Rename bus**, **Remove bus**, and reorder with **Move up** / **Move down** (or `CTRL+Up` / `CTRL+Down`; `Delete` removes the focused bus). Each bus appears in the Home mixer with its own volume slider and mute button, after the sources.

To route a source into a bus, select the source on the **Scenes and Sources** tab and press **Sends...**. In that dialog you can add a send to any bus, set its level, remove sends, and toggle **Send directly to master**. Leave "Send directly to master" on for aux-style routing (the source is heard directly, and the bus adds an effect such as reverb); turn it off to route the source only through its buses (insert-style, for processing a microphone with EQ or compression, for example).

## Effects (VST plugins on buses)

Each bus â€” and the master output â€” can run a chain of VST2 effects. On the **Buses** tab, select a bus (or the pinned **Master output** row) and use the effects list below it:

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

Open **File â†’ Preferences** (`CTRL+,`) and choose the **VST plugins** tab to tell Pubsplash where your plugins live. The folder list starts out with the standard Windows VST locations that exist on most machines ; add or remove folders as needed (`Delete` removes the focused folder).

Press **Scan for new plugins** to scan only files that haven't been scanned before, or **Rescan all plugins** to start over. Scans are able to be canceled at any time. If a plugin is taking too long and you suspect it's not going to scan, you can skip it. If a scan runs to completion, a cache will be written alongside Pubsplash's configuration file and these plugins will become available to use immediately.

## Text-to-speech

A **Text-to-Speech** source reads incoming chat aloud. Add one from the Sources list on the **Scenes** tab, and its dialog lets you choose the engine, the voice, and the rate, volume, and pitch.

Nine engines are available:

| Engine | Setup needed | Notes |
| --- | --- | --- |
| SAPI 5 | None | The voices already installed on Windows. The default, and the only engine that works offline. |
| Microsoft Edge | None | The Edge read-aloud voices. Needs an internet connection, but no account. |
| Google Translate | None | Free, unofficial, and rate-limited. Choose a language rather than a voice. |
| OpenAI | API key | |
| ElevenLabs | API key | |
| Azure | Subscription key and region | |
| AWS Polly | Access key ID, secret access key, and region | |
| Google Cloud | API key | |
| Star | The address of your own Star server | |

Credentials go on the **Speech** tab of **File → Preferences** (`CTRL+,`), once each, rather than on every source. They are encrypted for your Windows account, so copying `config.json` to another machine will not carry your keys with it.

That tab starts with a **Speech engine** picker, and everything after it belongs to whichever engine the picker names — so tabbing to the ElevenLabs key does not take you past OpenAI, Azure, AWS and Google first. The three engines that need no setup say so. The tab reopens on the engine you were last looking at.

Engines other than SAPI 5 and Google Translate publish their voice lists over the network, so their voice pickers start out holding only **Default voice**. The line under the picker says how many voices the selected engine has, or that they have not been fetched yet. Press **Get available voices** (`ALT+G`) to fill the list; it is remembered until you change the credentials or restart. Press **Preview voice** (`ALT+P`) to hear the current settings — this is the quickest way to find out whether a key is wrong, because it reports the reason rather than just going quiet.

Changing the engine reloads the voice list, which takes a moment for the long ones, so it happens a fraction of a second after you stop moving through the picker — or immediately when you tab out of it. Arrowing through all nine engines to reach the one you want costs nothing.

### Hearing it, and sending it

SAPI 5 speaks to you directly, whatever else is going on. The other engines produce audio that goes through the mixer like any other source, so to hear those yourself, turn on monitoring for the TTS source's mixer strip with `CTRL+M`.

**Send speech to the stream** controls whether your listeners hear it. With it unchecked, the speech stays off the stream but still reaches your monitor and any buses the source sends to.

### Cost and flood control

The Speech tab has two limits that apply to every network engine. **Longest message to speak** cuts over-long chat messages short rather than skipping them, so one wall of text cannot tie up the engine. **Shortest gap between requests** spreads out a burst of chat so a busy stream does not run up a bill or trip a rate limit. When messages still arrive faster than they can be spoken, the oldest queued ones are dropped, so what you hear stays current.

If an engine fails — a wrong key, no network, a service outage — the reason appears in the chat list rather than in a dialog box, and repeats of the same failure are held down to one a minute.

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

## Sound packs

A Sound Events source plays cues into its scene from the sound pack chosen on the **Sound packs** tab in Preferences. It can react to listener increases, listener decreases, listener-peak increases, incoming chat, and successfully sent chat messages; each of those five has its own checkbox in the source's edit dialog, and there is nothing else to set up. A stream begins with a silent listener baseline, so connecting does not play a count-change cue.

To use a pack of your own, open **File > Preferences** (`CTRL+,`) and go to the **Sound packs** tab. **Import pack** (`ALT+I`) asks for a compiled `.pspack` file and copies it into Pubsplash's own folder, so you can move or delete the file you imported from afterwards. Imported packs are listed in the **Sound pack** combo box alongside **Built-in default**; the one you choose there is used by everything — the startup and shut-down cues and every Sound Events source in every scene. Arrowing through the list is free: the pack you land on is loaded a moment after you stop, or immediately if you tab out of the list. A pack's sounds are all held in memory while it is the chosen one, so cues play without touching the disk, and they are released as soon as you change packs. **Remove pack** (`ALT+K`) deletes Pubsplash's copy of the chosen pack after asking you to confirm, and returns you to the built-in one.

A Sound Events source's cues always play on your default Windows output device, so you hear them whatever else is going on. By default they also go out to your listeners; clear **Send these sounds to the stream** in the edit dialog to keep them to yourself, and they then never reach the stream mix or a recording. The copy you hear bypasses the mixer, so the source's volume slider only affects what your listeners hear; muting the source silences the cues everywhere.

Pubsplash includes a default sound pack baked into the executable. Its startup and shutdown cues play locally on the default Windows output device; they do not enter the stream mix or local recordings. Either can be turned off under **File > Preferences** (`CTRL+,`) on the **Sound packs** tab, in the **Interface sounds** group. With the shut-down sound off, closing the window is immediate instead of waiting for the cue to finish.

Open **Tools > Sound Pack Manager** to create or edit a sound pack project. Project editing, saving, and compiling controls stay disabled until you create a project with **New** or load one with **Open**. **New** asks for a pack name and parent folder, then creates a child project folder using a Windows-safe version of the name with spaces changed to underscores. Interface packs currently support `ui_startup` and `ui_shutdown`; stream packs support `se_listener_increase`, `se_listener_decrease`, `se_listener_peak_increase`, `se_incoming_chat`, and `se_outgoing_chat`.

Development projects contain `sound-pack.toml` plus a `sounds/` directory. WAV files must be readable by Pubsplash; mono files are duplicated to stereo and non-48 kHz files are resampled during playback. In the manager, choose one Source WAV per sound or event, use **Test** to preview it, then press **Save**. Save copies the selected WAVs into the project; it does not move or delete your original files. The saved files use names such as `se_incoming_chat_01.wav`.

Compiling creates a distributable `.pspack` from the last saved project contents and bumps the project revision after a successful build. Press **Save** before **Compile** after changing Source WAV paths. There is also a command-line compiler installed next to `pubsplash.exe`: `soundpack.exe <project-directory> <output.pspack>`. `.pspack` encrypts assets at rest and authenticates their contents. Because Pubsplash must decrypt audio to play it, it is a deterrent against casual extraction rather than DRM.

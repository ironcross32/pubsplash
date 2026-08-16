# Pubsplash

Pubsplash is a Windows app for accessible live audio streaming. It sends a mix to Audiopub or a direct Icecast server and works well with screen readers such as NVDA and JAWS.

You can combine microphones, desktop audio, application audio, text-to-speech, and sound cues; adjust the mix; add VST effects; read and send Audiopub chat; and record an MP3 locally.

## Before you begin

You need:

- Windows 10 or Windows 11
- An Audiopub account trusted to stream, or Icecast source credentials
- An audio device if you plan to use a microphone

## Install

Download the newest release from the [GitHub releases page](https://github.com/ironcross32/pubsplash/releases).

These two links always point at the newest release, so they never go stale:

- [**Installer**](https://github.com/ironcross32/pubsplash/releases/latest/download/pubsplash-setup.exe)
- [**Portable ZIP**](https://github.com/ironcross32/pubsplash/releases/latest/download/pubsplash-portable.zip)

The two keep their data in different places. The installed copy uses `%LOCALAPPDATA%\pubsplash`. The portable copy uses a `user_data` folder inside the folder you unzipped. This allows Pubsplash and your user data to travel with you. Updates leave `user_data` alone.

One thing does not travel with a portable copy: saved passwords and API keys are encrypted for the Windows account that entered them, so on a different machine or a different user account they read as blank and have to be entered again. Everything else will still work.

Both kinds keep themselves up to date — see [Automatic updates](#automatic-updates). Every release is also on the [releases page](https://github.com/ironcross32/pubsplash/releases) under its version number, along with debug symbols.

## Getting started

### Connecting to a service

1. Open **File > Setup streaming services**.
2. Select the built-in **Audiopub** service, or choose **Add** for a self-hosted Audiopub instance or Icecast.
3. Enter the requested details and choose **Connect**.

For Audiopub, you need the site address, your email, and your password. The Icecast server and port are filled in for you from the site address and only need changing if the instance publishes somewhere other than the usual `live.` host on port 8000.

For Icecast, you normally need the server, port, mount point, username, and source password. The username defaults to source; the mount point may be `/` for the server root. Icecast does not provide Audiopub chat, listener counts, archiving, or an Audiopub stream page.

### 2. Add audio

Open **Scenes and Sources**. A scene is a saved collection of sources; the default scene is ready to use. Select it, choose **Add source**, and select from one of the following:

- **Microphone** - an input device.
- **Desktop Audio** - system audio. Pubsplash excludes its own audio, preventing text-to-speech and sound cues from echoing into the stream unless you want them to (see below).
- **Application** - one program, such as a browser, game, or music player. You can select a running program or type a name for one that will open later.
- **Text-to-Speech** - reads incoming Audiopub chat aloud.
- **Sound Events** - plays cues for listener and chat activity.

Each source appears as a strip in the **Home** mixer. Use its volume and mute controls to adjust it. Volume boost and monitoring are available in the context menu. Pubsplash should recover a temporarily unavailable source in most cases. While it's attempting to reconnect, the volume slider on its channel strip will reflect this.

### 3. Set stream information

Choose **File > Set stream info** and enter a title and description. You can also choose the audio quality, archiving options, and Mastodon options (See below). The title, description, archive choice, and recording choice are per-stream settings; the bitrate is remembered between sessions.

### 4. Go live

On **Home**, choose **Start streaming**. If stream information is missing, Pubsplash opens that dialog first. Choose **Stop streaming** when finished.

The stream overview reports status, duration, listeners, listener peak, and connection problems. It does not report a healthy stream until audio is actually being sent.

## Record without streaming

[press] **Start recording** on Home to save the current mix as an MP3 without connecting to a server. Once recording is underway, the button changes its state to **Stop recording**, hit that to finish.

Streaming and standalone recording cannot run at the same time. Recordings are named recording_date_time.mp3 and saved in the folder configured on **File > Preferences > Archiving**. The default is your Music library.

## The main concepts

- **Scenes** let you prepare different source combinations and switch between them.
- **Sources** produce audio. Their names describe what they capture, making several microphones or applications easier to distinguish.
- **Buses** are shared mixing points. Send multiple sources to a bus when they should share volume or effects.
- **Effects** are VST2 or VST3 plugins on a bus or the master output. Effects run from top to bottom and can be bypassed while live.
- **FX chains** can be saved in the library or exported as .pubfx files.

To route a source, select it on **Scenes and Sources**, choose **Sends...**, and select a bus. Leave **Send directly to master** enabled for a dry signal plus bus effects; disable it when the source should be heard only through its buses.

## Chat and text-to-speech

The **Chat** tab shows incoming Audiopub messages and lets you send replies. The feed reconnects automatically if it drops. **Reconnect chat** forces an immediate reconnect without interrupting your stream.

Note: The reconnect chat button is there as a means of trying to work around a server-side issue we have no control over. It may not work in all instances.

To read chat aloud, add a **Text-to-Speech** source. SAPI 5, Microsoft Edge, and Google Translate need no API credentials. OpenAI, ElevenLabs, Azure, AWS Polly, Google Cloud, and a self-hosted Star server require credentials on the **Speech** tab of Preferences. Credentials are encrypted for your Windows account.

Note: Star support should be considered inoperative at the current time.

Speech is played locally by default. Enable **Send speech to the stream** if listeners should hear it too. The Speech tab also controls message length and the delay between requests. The **API** tab shows usage for engines that have spoken during the current session.

## Keyboard access

| Shortcut | Action |
| --- | --- |
| F1 | Help for the focused control |
| F6 / Shift+F6 | Move between lists on the current tab |
| F9 / F10 | Start or stop streaming / recording |
| Ctrl+, | Open Preferences |
| Ctrl+M | Toggle monitoring for the focused mixer strip |

Use **Preferences > Keybinds** to add, change, or remove shortcuts. Global shortcuts can work while another application is focused; they must include Ctrl, Alt, or Shift, or be a function key.

Mixer sliders change by 1% with arrow keys and by 10% with Page Up or Page Down. Home and End move to maximum and minimum. A slider's context menu can enable volume boost up to 500%.

## Optional features

### Mastodon announcements

On the **Mastodon** tab of Preferences, choose **Authorize** and approve Pubsplash in your browser. You can then post when a stream starts or periodically. Templates support {title}, {description}, {url}, and {tod}. Every automated post ends with #PubsplashStreamInfo to make it easier for your followers to manage.

### Sound packs

The **Sound packs** tab controls startup, shutdown, listener, and chat sounds. You can import .pspack files, preview their events, and choose a pack. **Tools > Sound Pack Manager** creates and compiles packs; pack projects can contain WAV and Ogg Opus files.

### Automatic updates

Pubsplash checks for updates at startup by default. Change this on Preferences' **General** tab, or use **Check for updates now**. Updates are verified before installation and never interrupt an active stream or recording.

## Troubleshooting

If a source is silent, check its device or application selection and look for "(reconnecting)" in the mixer. For connection problems, verify the service credentials and consult the log.

Open **Go to > Go to Pubsplash data directory** to find the data folder. Logs are in %LOCALAPPDATA%\pubsplash\logs\. On Preferences' **Logging & debugging** tab, increase the log level temporarily or choose **Compress logs** to create a ZIP containing logs and crash dumps for a bug report. The archive does not include settings, passwords, or API keys.

## Building from source

Install Rust stable, Visual Studio 2019 or later with the Windows SDK, CMake, and Ninja. Then run:

    cargo build --release

The first build downloads the required prebuilt wxWidgets libraries.

## License

See [LICENSE](LICENSE) for licensing information.


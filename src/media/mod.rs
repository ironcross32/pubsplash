//! The Media Player source: a folder of music, played into the mixer.
//!
//! Two halves. This file is the pure one — which files count as music, what
//! order they play in — and [`player`] is the worker thread that decodes them
//! and feeds [`crate::audio::ExternalFeeds`]. The UI-side lifecycle (one worker
//! per media source in the active scene) is [`crate::ui::media`].
//!
//! A media player is an ordinary mixer source in every respect that matters: it
//! has a strip, a fader, a mute, sends, and its own monitor toggle. What is
//! special about it is the ducking, and that is not here either — the engine
//! owns it, because only the engine can see what every *other* source is doing
//! in the same block (see [`crate::audio::mixer::Ducker`]).

pub mod player;

use rand::seq::SliceRandom;
use std::path::{Path, PathBuf};

/// The file extensions the player will try, lowercased.
///
/// This is symphonia's coverage and nothing more: a file it cannot decode would
/// be picked, announced, and then produce silence for as long as the track
/// would have lasted, which is worse than never picking it. `.opus` is the
/// notable absence — symphonia 0.5 has no Opus decoder — as are `.wma` and
/// `.ape`, which it has never supported.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "m4b", "mp4", "aac", "flac", "ogg", "oga", "wav", "wave", "aif", "aiff", "aifc",
    "caf", "mka", "mp1", "mp2",
];

/// A ceiling on how many files one folder contributes, so pointing a source at
/// a drive root cannot spend the session walking it.
const MAX_FILES: usize = 50_000;

/// A ceiling on how deep the walk goes, for the same reason.
const MAX_DEPTH: usize = 12;

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.as_str()))
}

/// What the user is told a file is called: its name without the extension.
pub fn track_title(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Every playable file under `root`, including its subfolders, in a stable
/// order.
///
/// Subfolders are included because a music folder is almost always a folder of
/// album folders, and sorted because that order is what "shuffle off" means —
/// without it the order would be whatever the filesystem happened to hand back,
/// which is neither alphabetical nor stable between runs.
pub fn scan_folder(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A folder that cannot be read (permissions, a disconnected drive)
            // is skipped rather than failing the scan: the rest of the library
            // is still playable, and the user hears about an empty result
            // through the source's own label.
            Err(e) => {
                log::debug!("Media player: skipping {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                // Symlinked directories are not followed: a link back up the
                // tree would make the walk unbounded.
                Ok(kind) if kind.is_dir() && depth < MAX_DEPTH => stack.push((path, depth + 1)),
                Ok(kind) if kind.is_file() && is_supported(&path) => files.push(path),
                _ => {}
            }
            if files.len() >= MAX_FILES {
                log::warn!(
                    "Media player: {} holds more than {MAX_FILES} playable files; the rest are ignored",
                    root.display()
                );
                files.sort();
                return files;
            }
        }
    }
    files.sort();
    files
}

/// The order a folder's files play in, and where the player is up to.
///
/// Exhausting the list is not the end of anything: [`Playlist::next`] wraps,
/// reshuffling as it goes, so a source left alone plays forever.
pub struct Playlist {
    files: Vec<PathBuf>,
    /// Indices into `files`, in playing order.
    order: Vec<usize>,
    /// How far into `order` the player has got.
    position: usize,
    shuffle: bool,
}

impl Playlist {
    pub fn new(files: Vec<PathBuf>, shuffle: bool) -> Self {
        let mut playlist = Self {
            files,
            order: Vec::new(),
            position: 0,
            shuffle,
        };
        playlist.reorder(None);
        playlist
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// The files this playlist was built from, in folder order. Compared
    /// against a fresh scan so an unchanged folder does not reshuffle a pass
    /// that is already in progress.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The next file to play, wrapping (and reshuffling) at the end.
    pub fn next(&mut self) -> Option<PathBuf> {
        let picked = self.peek()?;
        self.position += 1;
        // Wrapped eagerly, the moment the pass runs out, rather than on the way
        // into the next `next()`. That is what makes [`Playlist::peek`] able to
        // answer at all: deferring the reshuffle leaves the first track of the
        // next pass genuinely undecided, and "skip to the next track" has to be
        // able to say what it is about to play before it plays it.
        if self.position >= self.order.len() {
            self.reorder(self.order.last().copied());
            self.position = 0;
        }
        Some(picked)
    }

    /// The file [`Playlist::next`] would return, without taking it.
    ///
    /// This is what the announcement on a skip is built from, so it has to be
    /// the truth rather than a guess — see the eager wrap in `next`.
    pub fn peek(&self) -> Option<PathBuf> {
        let index = *self.order.get(self.position)?;
        self.files.get(index).cloned()
    }

    /// Rebuilds the play order. `avoid_first` is the track that just finished:
    /// a fresh shuffle is allowed to start with it, which is the one repeat a
    /// listener always notices, so it is moved out of the way.
    fn reorder(&mut self, avoid_first: Option<usize>) {
        self.order = (0..self.files.len()).collect();
        if !self.shuffle {
            return;
        }
        self.order.shuffle(&mut rand::thread_rng());
        if self.order.len() > 1 && self.order.first().copied() == avoid_first {
            self.order.swap(0, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|n| PathBuf::from(format!("track{n}.mp3")))
            .collect()
    }

    #[test]
    fn only_decodable_extensions_are_picked_up() {
        assert!(is_supported(Path::new("song.mp3")));
        assert!(is_supported(Path::new("song.FLAC")), "case-insensitive");
        assert!(!is_supported(Path::new("cover.jpg")));
        assert!(!is_supported(Path::new("notes.txt")));
        // No decoder for these, so they must never be queued.
        assert!(!is_supported(Path::new("song.opus")));
        assert!(!is_supported(Path::new("song.wma")));
        assert!(!is_supported(Path::new("noextension")));
    }

    #[test]
    fn a_track_is_named_by_its_filename_without_the_extension() {
        assert_eq!(track_title(Path::new(r"C:\music\01 Hello.mp3")), "01 Hello");
    }

    /// Without shuffle the list is the folder's own order, over and over.
    #[test]
    fn in_order_playback_wraps_round() {
        let mut playlist = Playlist::new(files(3), false);
        let played: Vec<PathBuf> = (0..7).filter_map(|_| playlist.next()).collect();
        assert_eq!(
            played,
            vec![
                PathBuf::from("track0.mp3"),
                PathBuf::from("track1.mp3"),
                PathBuf::from("track2.mp3"),
                PathBuf::from("track0.mp3"),
                PathBuf::from("track1.mp3"),
                PathBuf::from("track2.mp3"),
                PathBuf::from("track0.mp3"),
            ]
        );
    }

    /// A shuffle that repeats a track before playing the others is the thing
    /// users notice and complain about, so a pass plays each file exactly once.
    #[test]
    fn a_shuffled_pass_plays_every_file_once() {
        let mut playlist = Playlist::new(files(20), true);
        let mut played: Vec<PathBuf> = (0..20).filter_map(|_| playlist.next()).collect();
        played.sort();
        let mut expected = files(20);
        expected.sort();
        assert_eq!(played, expected);
    }

    /// The one repeat a wrap can produce: the last track of a pass coming back
    /// as the first of the next.
    #[test]
    fn a_reshuffle_does_not_repeat_the_track_that_just_played() {
        for _ in 0..200 {
            let mut playlist = Playlist::new(files(4), true);
            let played: Vec<PathBuf> = (0..8).filter_map(|_| playlist.next()).collect();
            assert_ne!(played[3], played[4], "repeated across the wrap");
        }
    }

    /// What "skip to the next track" announces has to be what then plays,
    /// including across the wrap where a reshuffle happens.
    #[test]
    fn peek_is_what_next_hands_back() {
        for shuffle in [false, true] {
            let mut playlist = Playlist::new(files(4), shuffle);
            for _ in 0..12 {
                let peeked = playlist.peek();
                assert_eq!(peeked, playlist.next(), "shuffle: {shuffle}");
                assert!(peeked.is_some());
            }
        }
    }

    #[test]
    fn an_empty_playlist_has_nothing_to_peek_at() {
        assert_eq!(Playlist::new(Vec::new(), true).peek(), None);
    }

    #[test]
    fn an_empty_folder_plays_nothing_rather_than_looping() {
        let mut playlist = Playlist::new(Vec::new(), true);
        assert!(playlist.is_empty());
        assert_eq!(playlist.next(), None);
    }

    #[test]
    fn one_file_repeats_rather_than_stopping() {
        let mut playlist = Playlist::new(files(1), true);
        assert_eq!(playlist.next(), Some(PathBuf::from("track0.mp3")));
        assert_eq!(playlist.next(), Some(PathBuf::from("track0.mp3")));
    }

    #[test]
    fn a_scan_finds_music_in_subfolders_and_ignores_everything_else() {
        let root = std::env::temp_dir().join(format!("pubsplash-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("album")).unwrap();
        std::fs::write(root.join("b.mp3"), b"").unwrap();
        std::fs::write(root.join("cover.jpg"), b"").unwrap();
        std::fs::write(root.join("album").join("a.flac"), b"").unwrap();

        let found = scan_folder(&root);

        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], root.join("album").join("a.flac"), "sorted");
        assert_eq!(found[1], root.join("b.mp3"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_is_not_there_scans_to_nothing() {
        assert!(scan_folder(Path::new(r"Z:\definitely\not\here")).is_empty());
    }
}

//! Writes the encoded MP3 stream to disk while recording. The engine feeds it
//! the same bytes it hands to the Icecast sender, so a recording is an exact,
//! re-encode-free copy of the broadcast.
//!
//! A single stream session normally produces one continuous file. The `split`
//! machinery (renaming the first file to `..__001.mp3` and continuing into
//! `..__002.mp3`, ...) is implemented and ready but currently has no trigger:
//! the network layer ends the stream on a connection loss rather than
//! reconnecting, so nothing splits mid-recording yet.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub struct Recorder {
    /// The path the recording started at (the unsuffixed name).
    base_path: PathBuf,
    writer: BufWriter<File>,
    /// 0 while the recording is a single unsplit file; once a split happens it
    /// becomes the number of the part currently being written (>= 2).
    part: u32,
}

impl Recorder {
    /// Creates the recording file (and any missing parent directories).
    pub fn new(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        Ok(Self {
            base_path: path.to_path_buf(),
            writer: BufWriter::new(file),
            part: 0,
        })
    }

    /// Appends encoded bytes. Errors are logged and dropped so file trouble can
    /// never stall the 10 ms mix cadence.
    pub fn write(&mut self, bytes: &[u8]) {
        if let Err(e) = self.writer.write_all(bytes) {
            log::error!("Recording write failed: {e}");
        }
    }

    /// Flushes and closes the current file.
    pub fn finish(mut self) {
        if let Err(e) = self.writer.flush() {
            log::error!("Recording flush failed: {e}");
        }
    }

    /// Starts a new recording part. The first split renames the original
    /// `name.mp3` to `name__001.mp3` and continues into `name__002.mp3`;
    /// subsequent splits open `name__003.mp3`, and so on. Currently unused (no
    /// mid-stream reconnect exists to trigger it).
    #[allow(dead_code)]
    pub fn split(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;
        if self.part == 0 {
            // Retroactively make the single file "part 1".
            std::fs::rename(&self.base_path, part_path(&self.base_path, 1))?;
            self.part = 2;
        } else {
            self.part += 1;
        }
        let next = part_path(&self.base_path, self.part);
        self.writer = BufWriter::new(File::create(&next)?);
        Ok(())
    }
}

/// Returns a path that does not yet exist on disk: `base` if it is free,
/// otherwise `base` with the first unused `__00N` suffix (`__001`, `__002`,
/// ...). Used to avoid overwriting a previous recording when a new one would
/// land on the same name (e.g. a stop/start within the same second).
pub fn unique_path(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    let mut n = 1u32;
    loop {
        let candidate = part_path(base, n);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Inserts a `__00N` part suffix before the extension:
/// `dir/name.mp3` -> `dir/name__001.mp3`.
fn part_path(base: &Path, n: u32) -> PathBuf {
    let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned());
    let ext = base.extension().map(|s| s.to_string_lossy().into_owned());
    let file = match (stem, ext) {
        (Some(stem), Some(ext)) => format!("{stem}__{n:03}.{ext}"),
        (Some(stem), None) => format!("{stem}__{n:03}"),
        _ => format!("recording__{n:03}"),
    };
    match base.parent() {
        Some(parent) => parent.join(file),
        None => PathBuf::from(file),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_path_inserts_suffix_before_extension() {
        assert_eq!(
            part_path(Path::new(r"C:\music\My_Show_2026-07-24_15-30-00.mp3"), 1),
            PathBuf::from(r"C:\music\My_Show_2026-07-24_15-30-00__001.mp3")
        );
        assert_eq!(
            part_path(Path::new("show.mp3"), 12),
            PathBuf::from("show__012.mp3")
        );
    }

    #[test]
    fn unique_path_bumps_suffix_past_existing_files() {
        let dir = std::env::temp_dir().join(format!("pubsplash_uniq_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("show.mp3");

        // Free name: returned unchanged.
        assert_eq!(unique_path(&base), base);

        // Original taken -> __001; then __001 taken too -> __002.
        std::fs::write(&base, b"a").unwrap();
        assert_eq!(unique_path(&base), dir.join("show__001.mp3"));
        std::fs::write(dir.join("show__001.mp3"), b"b").unwrap();
        assert_eq!(unique_path(&base), dir.join("show__002.mp3"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_renames_first_file_and_continues() {
        let dir = std::env::temp_dir().join(format!("pubsplash_rec_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let base = dir.join("clip.mp3");

        let mut rec = Recorder::new(&base).unwrap();
        rec.write(b"part-one");
        rec.split().unwrap();
        rec.write(b"part-two");
        rec.finish();

        let p1 = dir.join("clip__001.mp3");
        let p2 = dir.join("clip__002.mp3");
        assert!(!base.exists(), "original name should have been renamed away");
        assert_eq!(std::fs::read(&p1).unwrap(), b"part-one");
        assert_eq!(std::fs::read(&p2).unwrap(), b"part-two");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

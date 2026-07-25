//! Dev tool: round-trips `help.toml` from the `help::tag(..)` call sites in the
//! UI source.
//!
//!   cargo run --bin gen-help
//!
//! It scans `src/ui/*.rs` for `help::tag(<widget>, "<id>", "<label>")`, then
//! merges into `help.toml`:
//!   - new ids are appended with a blank `message`,
//!   - existing ids keep their authored `message` (only `label`/`file` refresh),
//!   - ids gone from the source move to a trailing `[[stale]]` section so a
//!     rename never loses authored text.
//! Entries are sorted by id within each section for stable diffs.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

const HEADER: &str = "\
# Context-sensitive help messages, spoken when F1 is pressed on a control.
#
# This file is round-tripped by the `gen-help` dev tool:
#   cargo run --bin gen-help
# It rescans the UI source for `help::tag(..)` calls, adds any new controls
# (with a blank `message` for you to fill in), refreshes each control's `label`
# and `file`, and preserves everything you have already written. Controls that no
# longer exist in the source are moved to a `[[stale]]` section rather than
# deleted, so a rename never loses your text.
#
# Fill in `message` for each control. A blank message falls back to a generic
# \"No help available for this control.\" at runtime. `label` and `file` are
# context for you and are ignored by the app.
";

#[derive(Serialize, Deserialize, Default)]
struct HelpFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    control: Vec<Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stale: Vec<Entry>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Entry {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    message: String,
}

/// A `help::tag` call found in the source.
#[derive(Clone)]
struct Scanned {
    id: String,
    label: String,
    file: String,
}

fn main() {
    let root = env!("CARGO_MANIFEST_DIR");
    let ui_dir = Path::new(root).join("src").join("ui");
    let out_path = Path::new(root).join("help.toml");

    let scanned = scan_dir(&ui_dir);
    println!("Found {} help::tag call sites.", scanned.len());

    let existing: HelpFile = match fs::read_to_string(&out_path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("warning: could not parse existing help.toml ({e}); starting fresh");
            HelpFile::default()
        }),
        Err(_) => HelpFile::default(),
    };

    let merged = merge(scanned, existing);

    let body = toml::to_string_pretty(&merged).expect("serialize help.toml");
    fs::write(&out_path, format!("{HEADER}\n{body}")).expect("write help.toml");

    println!(
        "Wrote {}: {} control(s), {} stale.",
        out_path.display(),
        merged.control.len(),
        merged.stale.len()
    );
    let unfilled = merged
        .control
        .iter()
        .filter(|e| e.message.trim().is_empty())
        .count();
    if unfilled > 0 {
        println!("{unfilled} control(s) still need a message.");
    }
}

/// Reads every `.rs` file under `dir` and extracts the `help::tag` calls.
fn scan_dir(dir: &Path) -> Vec<Scanned> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("cannot read {}: {e}", dir.display());
            return out;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let file = format!("src/ui/{}", path.file_name().unwrap().to_string_lossy());
        out.extend(scan_source(&content, &file));
    }
    out
}

/// Extracts `help::tag(<widget>, "id", "label")` calls from one source string.
/// The widget argument never contains a string literal, so the first two string
/// literals after `help::tag(` are the id and the label.
fn scan_source(content: &str, file: &str) -> Vec<Scanned> {
    let mut out = Vec::new();
    let needle = "help::tag(";
    let mut search = 0;
    while let Some(rel) = content[search..].find(needle) {
        let after = search + rel + needle.len();
        if let Some((id, id_end)) = next_string(&content[after..]) {
            if let Some((label, _)) = next_string(&content[after + id_end..]) {
                out.push(Scanned {
                    id,
                    label,
                    file: file.to_string(),
                });
            }
        }
        search = after;
    }
    out
}

/// Reads the next double-quoted string literal in `s`, honoring `\"` escapes.
/// Returns the unescaped contents and the index just past the closing quote.
fn next_string(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'"' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    i += 1; // opening quote
    let mut value = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                let c = bytes[i + 1] as char;
                value.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                i += 2;
            }
            b'"' => return Some((value, i + 1)),
            b => {
                value.push(b as char);
                i += 1;
            }
        }
    }
    None
}

/// Merges scanned call sites with the previously authored file.
fn merge(scanned: Vec<Scanned>, existing: HelpFile) -> HelpFile {
    // Authored messages by id, from both sections (so a stale->live revival
    // recovers its text).
    let mut messages: BTreeMap<String, String> = BTreeMap::new();
    for e in existing.control.iter().chain(existing.stale.iter()) {
        if !e.message.trim().is_empty() {
            messages.insert(e.id.clone(), e.message.clone());
        }
    }

    // De-duplicate scanned ids (the mixer tags the same id on every strip).
    let mut live: BTreeMap<String, Scanned> = BTreeMap::new();
    for s in scanned {
        live.entry(s.id.clone()).or_insert(s);
    }

    let control: Vec<Entry> = live
        .values()
        .map(|s| Entry {
            id: s.id.clone(),
            label: s.label.clone(),
            file: s.file.clone(),
            message: messages.get(&s.id).cloned().unwrap_or_default(),
        })
        .collect();

    // Anything previously present but no longer scanned, with authored text, is
    // preserved as stale.
    let stale: Vec<Entry> = existing
        .control
        .iter()
        .chain(existing.stale.iter())
        .filter(|e| !live.contains_key(&e.id) && !e.message.trim().is_empty())
        .map(|e| {
            let mut e = e.clone();
            e.message = messages.get(&e.id).cloned().unwrap_or_default();
            (e.id.clone(), e)
        })
        .collect::<BTreeMap<String, Entry>>()
        .into_values()
        .collect();

    HelpFile { control, stale }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(id: &str, label: &str) -> Scanned {
        Scanned {
            id: id.into(),
            label: label.into(),
            file: "src/ui/home.rs".into(),
        }
    }

    fn entry(id: &str, msg: &str) -> Entry {
        Entry {
            id: id.into(),
            label: "old".into(),
            file: "src/ui/home.rs".into(),
            message: msg.into(),
        }
    }

    #[test]
    fn preserves_authored_message_and_refreshes_label() {
        let existing = HelpFile {
            control: vec![entry("tab.home.streamButton", "Starts the stream.")],
            stale: vec![],
        };
        let scanned = vec![sc("tab.home.streamButton", "Start/stop streaming")];
        let merged = merge(scanned, existing);
        assert_eq!(merged.control.len(), 1);
        assert_eq!(merged.control[0].message, "Starts the stream.");
        assert_eq!(merged.control[0].label, "Start/stop streaming");
        assert!(merged.stale.is_empty());
    }

    #[test]
    fn new_id_gets_blank_message() {
        let merged = merge(vec![sc("tab.chat.sendButton", "Send")], HelpFile::default());
        assert_eq!(merged.control.len(), 1);
        assert_eq!(merged.control[0].message, "");
    }

    #[test]
    fn removed_id_with_text_becomes_stale() {
        let existing = HelpFile {
            control: vec![entry("tab.home.oldButton", "Legacy help.")],
            stale: vec![],
        };
        let merged = merge(vec![sc("tab.home.newButton", "New")], existing);
        assert_eq!(merged.control.len(), 1);
        assert_eq!(merged.control[0].id, "tab.home.newButton");
        assert_eq!(merged.stale.len(), 1);
        assert_eq!(merged.stale[0].id, "tab.home.oldButton");
        assert_eq!(merged.stale[0].message, "Legacy help.");
    }

    #[test]
    fn removed_id_without_text_is_dropped() {
        let existing = HelpFile {
            control: vec![entry("tab.home.oldButton", "")],
            stale: vec![],
        };
        let merged = merge(vec![], existing);
        assert!(merged.control.is_empty());
        assert!(merged.stale.is_empty());
    }

    #[test]
    fn revived_stale_id_recovers_text() {
        let existing = HelpFile {
            control: vec![],
            stale: vec![entry("tab.home.button", "Recovered help.")],
        };
        let merged = merge(vec![sc("tab.home.button", "Button")], existing);
        assert_eq!(merged.control.len(), 1);
        assert_eq!(merged.control[0].message, "Recovered help.");
        assert!(merged.stale.is_empty());
    }

    #[test]
    fn scan_source_extracts_id_and_label() {
        let src = r#"
            help::tag(&slider, "tab.home.mixer.strip.volume", "Source volume");
            super::help::tag(&self.btn, "tab.home.streamButton", "Start/stop streaming");
        "#;
        let found = scan_source(src, "src/ui/home.rs");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "tab.home.mixer.strip.volume");
        assert_eq!(found[0].label, "Source volume");
        assert_eq!(found[1].id, "tab.home.streamButton");
    }
}

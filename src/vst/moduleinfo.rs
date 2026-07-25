//! Reads a VST3 bundle's `Contents\Resources\moduleinfo.json` (present since
//! VST SDK 3.7.2). When it parses, the plugin never has to be loaded to be
//! identified — the scan helper is only spawned as a fallback.

use super::types::ScanOutput;
use std::path::Path;

/// Outcome of looking for moduleinfo.json in a bundle:
/// - `None`: absent or unreadable — fall back to loading via the helper.
/// - `Some(Ok)`: identified without loading.
/// - `Some(Err)`: present and valid, but the module has no audio classes.
pub fn scan_bundle(bundle: &Path) -> Option<Result<ScanOutput, String>> {
    let path = bundle
        .join("Contents")
        .join("Resources")
        .join("moduleinfo.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            log::debug!("Unparseable {}: {e}", path.display());
            return None;
        }
    };
    Some(parse(&value))
}

fn parse(value: &serde_json::Value) -> Result<ScanOutput, String> {
    let str_of = |v: &serde_json::Value| v.as_str().unwrap_or_default().trim().to_string();
    let mut out = ScanOutput {
        name: str_of(&value["Name"]),
        vendor: str_of(&value["Factory Info"]["Vendor"]),
        version: str_of(&value["Version"]),
        unique_id: None,
        class_ids: Vec::new(),
    };
    let mut first_class_name = String::new();
    if let Some(classes) = value["Classes"].as_array() {
        for class in classes {
            if class["Category"].as_str() != Some("Audio Module Class") {
                continue;
            }
            let cid = str_of(&class["CID"]);
            if !cid.is_empty() {
                out.class_ids.push(cid);
            }
            if first_class_name.is_empty() {
                first_class_name = str_of(&class["Name"]);
            }
        }
    }
    if out.class_ids.is_empty() {
        return Err("no audio module classes (moduleinfo.json)".to_string());
    }
    if out.name.is_empty() {
        out.name = first_class_name;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "Name": "Great Reverb",
      "Version": "2.1.0",
      "Factory Info": {
        "Vendor": "Example Audio",
        "URL": "https://example.com",
        "E-Mail": "info@example.com",
        "Flags": { "Unicode": true }
      },
      "Classes": [
        {
          "CID": "5CAF624A29A24C0DA2A2ABCDE0FF6162",
          "Category": "Audio Module Class",
          "Name": "Great Reverb",
          "Version": "2.1.0",
          "Sub Categories": ["Fx", "Reverb"]
        },
        {
          "CID": "AAAA624A29A24C0DA2A2ABCDE0FF6162",
          "Category": "Component Controller Class",
          "Name": "Great Reverb Controller"
        }
      ]
    }"#;

    #[test]
    fn parses_audio_module_classes() {
        let value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let out = parse(&value).unwrap();
        assert_eq!(out.name, "Great Reverb");
        assert_eq!(out.vendor, "Example Audio");
        assert_eq!(out.version, "2.1.0");
        assert_eq!(out.class_ids, vec!["5CAF624A29A24C0DA2A2ABCDE0FF6162"]);
    }

    #[test]
    fn no_audio_classes_is_an_error() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{ "Name": "X", "Classes": [] }"#).unwrap();
        assert!(parse(&value).is_err());
    }
}

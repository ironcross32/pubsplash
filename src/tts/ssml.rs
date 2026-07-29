//! SSML construction for the engines that take it (Azure and Edge).
//!
//! Chat messages are arbitrary user text and go straight into an XML document,
//! so escaping here is not cosmetic: an unescaped `<` turns a message into
//! malformed SSML and the request fails, and a crafted one would let a
//! listener inject prosody or `<audio>` tags into someone else's stream.

/// Escapes text for an XML text node or attribute value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not legal in XML 1.0 at all, and some
            // chat clients emit them. Tab, newline, and return are fine.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A signed percentage, the unit SSML `prosody` uses for rate and volume.
pub fn percent(value: i32) -> String {
    if value >= 0 {
        format!("+{value}%")
    } else {
        format!("{value}%")
    }
}

/// A signed hertz offset, the unit Edge uses for pitch.
pub fn hertz(value: i32) -> String {
    if value >= 0 {
        format!("+{value}Hz")
    } else {
        format!("{value}Hz")
    }
}

/// Wraps `text` in a complete SSML document for `voice`.
///
/// `language` has to be a real BCP-47 tag — the services reject the document
/// without one — but it does not have to match the voice, which carries its
/// own locale.
pub fn document(language: &str, voice: &str, text: &str, prosody: &str) -> String {
    format!(
        concat!(
            r#"<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="{}">"#,
            r#"<voice name="{}"><prosody {}>{}</prosody></voice></speak>"#
        ),
        escape(language),
        escape(voice),
        prosody,
        escape(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_in_chat_text_cannot_escape_the_document() {
        let hostile = r#"</prosody></voice><audio src="http://evil/x.mp3"/>"#;
        let doc = document("en-US", "en-US-AriaNeural", hostile, "rate=\"+0%\"");
        assert!(!doc.contains("<audio"), "{doc}");
        assert!(doc.contains("&lt;/prosody&gt;"), "{doc}");
        // Exactly one closing tag of each, the ones we wrote.
        assert_eq!(doc.matches("</voice>").count(), 1);
        assert_eq!(doc.matches("</speak>").count(), 1);
    }

    #[test]
    fn the_five_xml_entities_are_escaped() {
        assert_eq!(escape(r#"a&b<c>d"e'f"#), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }

    #[test]
    fn illegal_control_characters_become_spaces_but_whitespace_survives() {
        assert_eq!(escape("a\u{0}b\u{7}c"), "a b c");
        assert_eq!(escape("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn a_hostile_voice_name_cannot_add_attributes() {
        let doc = document("en-US", r#"x" onerror="y"#, "hello", "");
        assert_eq!(doc.matches("name=").count(), 1, "{doc}");
    }

    #[test]
    fn signed_units_carry_an_explicit_plus() {
        assert_eq!(percent(0), "+0%");
        assert_eq!(percent(50), "+50%");
        assert_eq!(percent(-25), "-25%");
        assert_eq!(hertz(10), "+10Hz");
        assert_eq!(hertz(-10), "-10Hz");
    }
}

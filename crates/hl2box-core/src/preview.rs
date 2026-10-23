//! Rules for what the text viewer is allowed to open, and how.
//!
//! The viewer never loads a binary or an oversized file into the UI. Text
//! extensions are whitelisted, sizes are capped, and content is sniffed so a
//! mislabelled binary (a NUL byte in the first kilobytes) is refused instead of
//! being shown as garbage.

/// Largest file the viewer will open.
pub const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

const TEXT_EXT: [&str; 17] = [
    "lua", "txt", "json", "vmt", "vdf", "properties", "cfg", "res", "cur", "fnt", "svg", "xml",
    "md", "ini", "log", "kv", "nut",
];

/// Whether a file name has a text extension worth opening.
pub fn is_text_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    TEXT_EXT.contains(&ext.as_str())
}

/// Sniff the first bytes: NUL means binary, and at least 95% of the sample
/// must be valid UTF-8 or printable ASCII/whitespace.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.is_empty() {
        return true;
    }
    if sample.contains(&0) {
        return false;
    }
    let text = String::from_utf8_lossy(sample);
    let bad = text
        .chars()
        .filter(|c| *c == '\u{FFFD}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
        .count();
    bad * 20 <= text.chars().count()
}

/// Decode for display: strip a UTF-8 BOM, normalise line endings, and replace
/// invalid sequences instead of failing.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Full gate used by the viewer before it reads anything.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    TooLarge(u64),
    NotText,
}

pub fn check_size(size: u64) -> Result<(), Refusal> {
    if size > MAX_PREVIEW_BYTES {
        Err(Refusal::TooLarge(size))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert!(is_text_name("lua/autorun/x.LUA"));
        assert!(is_text_name("info.txt"));
        assert!(!is_text_name("model.mdl"));
        assert!(!is_text_name("noext"));
    }

    #[test]
    fn sniffing() {
        assert!(looks_like_text(b"print('hi')\nfine\n"));
        assert!(looks_like_text("中文注释\n".as_bytes()));
        assert!(!looks_like_text(&[0x89, 0x50, 0x4E, 0x47, 0, 0, 0, 0x0D]));
        assert!(looks_like_text(b""));
    }

    #[test]
    fn decode_handles_bom_and_crlf() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"a\r\nb\rc");
        assert_eq!(decode_text(&bytes), "a\nb\nc");
    }

    #[test]
    fn size_gate() {
        assert_eq!(check_size(10), Ok(()));
        assert_eq!(check_size(MAX_PREVIEW_BYTES + 1), Err(Refusal::TooLarge(MAX_PREVIEW_BYTES + 1)));
    }
}

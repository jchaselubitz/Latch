//! tmux `send-keys` names, translated to bytes.
//!
//! The Conversation Hub and `latch send-keys` were written against tmux's key
//! vocabulary, so the daemon accepts the same names. A name that is not in the
//! table is sent literally, exactly as `send-keys` does. Cursor and keypad
//! keys honor the modes the child has set: a TUI that enabled application
//! cursor keys expects `ESC O A`, not `ESC [ A`.

/// Modes that change a key's encoding.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyModes {
    /// DECCKM.
    pub application_cursor_keys: bool,
    /// DECKPAM.
    pub application_keypad: bool,
}

/// Encodes one key name.
pub fn encode(name: &str, modes: KeyModes) -> Vec<u8> {
    if let Some(bytes) = named(name, modes) {
        return bytes;
    }
    if let Some(rest) = name.strip_prefix("C-").or_else(|| name.strip_prefix("^")) {
        if rest.len() == 1 {
            let c = rest.as_bytes()[0].to_ascii_lowercase();
            if c.is_ascii_lowercase() {
                return vec![c - b'a' + 1];
            }
            return match c {
                b'@' | b' ' => vec![0],
                b'[' => vec![0x1b],
                b'\\' => vec![0x1c],
                b']' => vec![0x1d],
                b'^' => vec![0x1e],
                b'_' | b'/' => vec![0x1f],
                b'?' => vec![0x7f],
                _ => name.as_bytes().to_vec(),
            };
        }
    }
    if let Some(rest) = name.strip_prefix("M-") {
        let mut out = vec![0x1b];
        out.extend(encode(rest, modes));
        return out;
    }
    name.as_bytes().to_vec()
}

fn named(name: &str, modes: KeyModes) -> Option<Vec<u8>> {
    let cursor = |letter: u8| {
        if modes.application_cursor_keys {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    let keypad = |app: &[u8], normal: &[u8]| {
        if modes.application_keypad {
            app.to_vec()
        } else {
            normal.to_vec()
        }
    };
    Some(match name {
        "Enter" | "KPEnter" => vec![b'\r'],
        "Escape" => vec![0x1b],
        "Tab" => vec![b'\t'],
        "BTab" => b"\x1b[Z".to_vec(),
        "Space" => vec![b' '],
        "BSpace" => vec![0x7f],
        "DC" => b"\x1b[3~".to_vec(),
        "IC" => b"\x1b[2~".to_vec(),
        "Up" => cursor(b'A'),
        "Down" => cursor(b'B'),
        "Right" => cursor(b'C'),
        "Left" => cursor(b'D'),
        "Home" => cursor(b'H'),
        "End" => cursor(b'F'),
        "PPage" | "PageUp" => b"\x1b[5~".to_vec(),
        "NPage" | "PageDown" => b"\x1b[6~".to_vec(),
        "F1" => b"\x1bOP".to_vec(),
        "F2" => b"\x1bOQ".to_vec(),
        "F3" => b"\x1bOR".to_vec(),
        "F4" => b"\x1bOS".to_vec(),
        "F5" => b"\x1b[15~".to_vec(),
        "F6" => b"\x1b[17~".to_vec(),
        "F7" => b"\x1b[18~".to_vec(),
        "F8" => b"\x1b[19~".to_vec(),
        "F9" => b"\x1b[20~".to_vec(),
        "F10" => b"\x1b[21~".to_vec(),
        "F11" => b"\x1b[23~".to_vec(),
        "F12" => b"\x1b[24~".to_vec(),
        "KP/" => keypad(b"\x1bOo", b"/"),
        "KP*" => keypad(b"\x1bOj", b"*"),
        "KP-" => keypad(b"\x1bOm", b"-"),
        "KP+" => keypad(b"\x1bOk", b"+"),
        "KP." => keypad(b"\x1bOn", b"."),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_literals_encode_like_tmux() {
        let plain = KeyModes::default();
        assert_eq!(encode("Enter", plain), b"\r");
        assert_eq!(encode("C-c", plain), vec![3]);
        assert_eq!(encode("C-C", plain), vec![3]);
        assert_eq!(encode("M-x", plain), b"\x1bx");
        assert_eq!(encode("Up", plain), b"\x1b[A");
        assert_eq!(encode("y", plain), b"y");
        assert_eq!(encode("hello", plain), b"hello");
        let app = KeyModes {
            application_cursor_keys: true,
            application_keypad: false,
        };
        assert_eq!(encode("Up", app), b"\x1bOA");
    }
}

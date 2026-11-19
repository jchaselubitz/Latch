//! Lowercase hexadecimal, the one text encoding Latch uses for digests, keys,
//! tokens, and path keys.

/// Encodes `bytes` as lowercase hex.
pub(crate) fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    text
}

/// Decodes strictly lowercase hex, or `None` for an odd length or any other
/// character. Uppercase is refused so a value has exactly one spelling.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_byte() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&bytes)), Some(bytes));
        assert_eq!(encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn refuses_odd_lengths_uppercase_and_non_hex() {
        assert_eq!(decode("abc"), None);
        assert_eq!(decode("AB"), None);
        assert_eq!(decode("zz"), None);
        assert_eq!(decode(""), Some(Vec::new()));
    }
}

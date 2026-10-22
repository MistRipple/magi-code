/// Lowercase hexadecimal with two digits per byte, including leading zeroes.
pub fn hex_lower(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)] as char);
        encoded.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::hex_lower;

    #[test]
    fn hexadecimal_preserves_zeroes_and_lowercase() {
        assert_eq!(hex_lower([]), "");
        assert_eq!(
            hex_lower([0, 1, 15, 16, 127, 128, 254, 255]),
            "00010f107f80feff"
        );
    }
}

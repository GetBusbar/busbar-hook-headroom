use std::fmt::{self, Write as _};
use std::str;

pub(crate) fn debug_lossy(mut bytes: &[u8], formatter: &mut fmt::Formatter) -> fmt::Result {
    formatter.write_char('"')?;

    while !bytes.is_empty() {
        let from_utf8_result = str::from_utf8(bytes);
        let valid = match from_utf8_result {
            Ok(valid) => valid,
            Err(utf8_error) => {
                let valid_up_to = utf8_error.valid_up_to();
                unsafe { str::from_utf8_unchecked(&bytes[..valid_up_to]) }
            }
        };

        let mut written = 0;
        for (i, ch) in valid.char_indices() {
            let esc = ch.escape_debug();
            if esc.len() != 1 && ch != '\'' {
                formatter.write_str(&valid[written..i])?;
                for ch in esc {
                    formatter.write_char(ch)?;
                }
                written = i + ch.len_utf8();
            }
        }
        formatter.write_str(&valid[written..])?;

        match from_utf8_result {
            Ok(_valid) => break,
            Err(utf8_error) => {
                let end_of_broken = if let Some(error_len) = utf8_error.error_len() {
                    valid.len() + error_len
                } else {
                    bytes.len()
                };
                for b in &bytes[valid.len()..end_of_broken] {
                    write!(formatter, "\\x{:02x}", b)?;
                }
                bytes = &bytes[end_of_broken..];
            }
        }
    }

    formatter.write_char('"')
}

use crate::{InputError, XllError, XllResult};

pub(crate) const EXCEL_STRING_LIMIT: usize = 32_767;

pub(crate) fn encode_bounded(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<Vec<u16>> {
    let length = bounded_len(text, argument, limit)?;
    let mut units = Vec::with_capacity(length);
    units.extend(text.encode_utf16());
    Ok(units)
}

pub(crate) fn encode_counted(
    text: &str,
    argument: &'static str,
    limit: usize,
) -> XllResult<Vec<u16>> {
    let length = bounded_len(text, argument, limit)?;
    let mut units = Vec::with_capacity(length + 1);
    units.push(length as u16);
    units.extend(text.encode_utf16());
    Ok(units)
}

fn bounded_len(text: &str, argument: &'static str, limit: usize) -> XllResult<usize> {
    let mut length = 0_usize;
    for character in text.chars() {
        length += character.len_utf16();
        if length > limit {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit,
                    actual: length,
                },
            ));
        }
    }
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_encoding_uses_one_final_buffer() {
        assert_eq!(
            encode_counted("価格", "test", EXCEL_STRING_LIMIT).unwrap(),
            [2, 0x4fa1, 0x683c]
        );
    }

    #[test]
    fn bounded_encoding_stops_at_the_first_unit_over_the_limit() {
        assert!(matches!(
            encode_bounded("😀😀", "test", 3),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 3,
                    actual: 4
                },
                ..
            })
        ));
    }
}

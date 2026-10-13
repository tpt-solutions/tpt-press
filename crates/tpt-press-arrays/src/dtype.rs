//! Element types and `.npy` headers.
//!
//! An element type is written as a NumPy `descr` string, such as `<f4` for
//! little-endian 32-bit floats. Only little-endian numbers and single-byte
//! types are supported. Big-endian data is refused rather than converted.

use crate::error::{Error, Result};

/// Magic bytes at the start of every `.npy` file.
const NPY_MAGIC: &[u8] = b"\x93NUMPY";

/// An element type: its `descr` string and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dtype {
    descr: String,
    size: usize,
}

impl Dtype {
    /// Parses a NumPy `descr` string, such as `<f4`, `<i8`, `|u1` or `|b1`.
    /// A bare `f4` is taken as little-endian. A single-byte type needs no prefix.
    pub fn parse(text: &str) -> Result<Self> {
        let descr = match text.as_bytes().first() {
            Some(b'<') | Some(b'|') => text.to_owned(),
            Some(b'>') => {
                return Err(Error::Unsupported {
                    message: "big-endian data is not supported",
                })
            }
            _ => format!("<{text}"),
        };
        let mut chars = descr.chars();
        let prefix = chars.next();
        let kind = chars.next();
        // The first two characters are ASCII here, so byte 2 is a character boundary.
        let digits = descr.get(2..).unwrap_or("");
        let size: usize = digits
            .parse()
            .map_err(|_| Error::InvalidDtype(text.to_owned()))?;
        let valid = match (prefix, kind) {
            (Some('<'), Some('f')) => matches!(size, 2 | 4 | 8),
            (Some('<'), Some('i' | 'u')) => matches!(size, 1 | 2 | 4 | 8),
            (Some('|'), Some('i' | 'u' | 'b')) => size == 1,
            _ => false,
        };
        if !valid {
            return Err(Error::InvalidDtype(text.to_owned()));
        }
        Ok(Self { descr, size })
    }

    /// The NumPy `descr` string, such as `<f4`.
    pub fn descr(&self) -> &str {
        &self.descr
    }

    /// Size of one element, in bytes. One of 1, 2, 4 or 8.
    pub fn size(&self) -> usize {
        self.size
    }
}

/// What a `.npy` file holds: its element type, shape, and its exact header bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpyHeader {
    /// Element type.
    pub dtype: Dtype,
    /// Array shape, in C order.
    pub shape: Vec<u64>,
    /// The header bytes, from the magic to the end of the padding. Stored
    /// verbatim so that unpacking reproduces the file exactly.
    pub raw: Vec<u8>,
}

/// Whether `bytes` starts with the `.npy` magic.
pub fn is_npy(bytes: &[u8]) -> bool {
    bytes.starts_with(NPY_MAGIC)
}

/// Reads a `.npy` header from the start of `bytes`. Returns the header and the offset of the data.
pub fn parse_npy(bytes: &[u8]) -> Result<(NpyHeader, usize)> {
    if !is_npy(bytes) {
        return Err(Error::Format {
            message: "not a .npy file: the magic bytes are missing".to_owned(),
        });
    }
    let version = *bytes.get(6).ok_or(Error::Truncated)?;
    let (len_field, start) = match version {
        1 => (2usize, 10usize),
        2 | 3 => (4usize, 12usize),
        _ => {
            return Err(Error::Format {
                message: format!("unsupported .npy version {version}"),
            })
        }
    };
    let len_bytes = bytes.get(8..8 + len_field).ok_or(Error::Truncated)?;
    let header_len = if len_field == 2 {
        u16::from_le_bytes([len_bytes[0], len_bytes[1]]) as usize
    } else {
        u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize
    };
    let end = start
        .checked_add(header_len)
        .ok_or(Error::Corrupt("npy header length overflows"))?;
    let text_bytes = bytes.get(start..end).ok_or(Error::Truncated)?;
    let text = std::str::from_utf8(text_bytes).map_err(|_| Error::Format {
        message: "the .npy header is not text".to_owned(),
    })?;

    if dict_value(text, "fortran_order") == Some("True") {
        return Err(Error::Unsupported {
            message: "Fortran-ordered arrays are not supported; save with C order",
        });
    }
    let descr = dict_value(text, "descr").ok_or_else(|| Error::Format {
        message: "the .npy header has no 'descr' entry".to_owned(),
    })?;
    let dtype = Dtype::parse(descr.trim_matches('\''))?;
    let shape = parse_shape(text)?;
    Ok((
        NpyHeader {
            dtype,
            shape,
            raw: bytes[..end].to_vec(),
        },
        end,
    ))
}

/// The raw text after `'key':`, up to the next comma or closing brace. Quoted values keep their quotes.
fn dict_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("'{key}':");
    let after = &text[text.find(&needle)? + needle.len()..];
    let after = after.trim_start();
    if let Some(rest) = after.strip_prefix('\'') {
        let close = rest.find('\'')? + 2;
        return Some(&after[..close]);
    }
    let end = after.find([',', '}'])?;
    Some(after[..end].trim())
}

fn parse_shape(text: &str) -> Result<Vec<u64>> {
    let needle = "'shape':";
    let after = text
        .find(needle)
        .map(|i| &text[i + needle.len()..])
        .ok_or_else(|| Error::Format {
            message: "the .npy header has no 'shape' entry".to_owned(),
        })?;
    let open = after.find('(').ok_or_else(|| Error::Format {
        message: "the .npy shape is not a tuple".to_owned(),
    })?;
    let close = after.find(')').ok_or_else(|| Error::Format {
        message: "the .npy shape is not a tuple".to_owned(),
    })?;
    after[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u64>().map_err(|_| Error::Format {
                message: format!("bad dimension `{s}` in the .npy shape"),
            })
        })
        .collect()
}

/// Builds a `.npy` version 1.0 file. Used by the tests.
#[cfg(test)]
pub(crate) fn npy_bytes(descr: &str, shape: &[u64], fortran: bool, data: &[u8]) -> Vec<u8> {
    let dims: Vec<String> = shape.iter().map(u64::to_string).collect();
    let tuple = match dims.len() {
        1 => format!("({},)", dims[0]),
        _ => format!("({})", dims.join(", ")),
    };
    let order = if fortran { "True" } else { "False" };
    let mut text = format!("{{'descr': '{descr}', 'fortran_order': {order}, 'shape': {tuple}, }}");
    // Pad with spaces so that the data starts at a multiple of 64, ending in a newline.
    while !(10 + text.len() + 1).is_multiple_of(64) {
        text.push(' ');
    }
    text.push('\n');
    let mut out = NPY_MAGIC.to_vec();
    out.extend_from_slice(&[1, 0]);
    out.extend_from_slice(&(text.len() as u16).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_descr_strings() {
        assert_eq!(Dtype::parse("<f4").unwrap().size(), 4);
        assert_eq!(Dtype::parse("f8").unwrap().descr(), "<f8");
        assert_eq!(Dtype::parse("|u1").unwrap().size(), 1);
        assert!(Dtype::parse(">f4").is_err());
        assert!(Dtype::parse("<f3").is_err());
        assert!(Dtype::parse("|f4").is_err());
        assert!(Dtype::parse("<U4").is_err());
    }

    #[test]
    fn reads_a_npy_header() {
        let data = [0u8; 2 * 3 * 4];
        let file = npy_bytes("<f4", &[2, 3], false, &data);
        let (header, offset) = parse_npy(&file).unwrap();
        assert_eq!(header.shape, vec![2, 3]);
        assert_eq!(header.dtype.descr(), "<f4");
        assert_eq!(offset, file.len() - data.len());
        assert_eq!(header.raw, file[..offset].to_vec());
        assert_eq!(offset % 64, 0);
    }

    #[test]
    fn refuses_fortran_order_and_bad_files() {
        let file = npy_bytes("<i2", &[4], true, &[0u8; 8]);
        assert!(matches!(parse_npy(&file), Err(Error::Unsupported { .. })));
        assert!(parse_npy(b"not npy").is_err());
    }
}

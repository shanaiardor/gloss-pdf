//! 从 PDF 原始字节里补读 `/ModDate`。
//!
//! pdfium-render 0.9.4 的 `PdfDocumentMetadataTagType::ModificationDate` 查询的是
//! 非标准键 `ModificationDate`（PDF 规范里是 `ModDate`），所以高层 API 永远拿不到
//! 修改时间。这里只针对这个字段做一次最小限度的后备解析：从文件尾部找 `/ModDate`，
//! 读它后面的字符串或十六进制字符串；不追求完整 PDF 解析，读不到就按缺失处理。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 只读文件尾部这么多字节；Info 字典通常在 xref 之前，靠近文件末尾。
const TAIL_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn read_modification_date(path: &Path) -> Option<String> {
    let bytes = read_candidate_bytes(path)?;
    modification_date_from_bytes(&bytes)
}

fn read_candidate_bytes(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length > TAIL_BYTES {
        file.seek(SeekFrom::End(-(TAIL_BYTES as i64))).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn modification_date_from_bytes(bytes: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"/ModDate";
    let mut found = None;
    let mut start = 0;
    while let Some(offset) = find(&bytes[start..], KEY) {
        let after = start + offset + KEY.len();
        let boundary = bytes.get(after).copied();
        if matches!(boundary, Some(b' ' | b'\t' | b'\r' | b'\n' | b'(' | b'<'))
            && let Some(value) = parse_value(&bytes[after..])
            && !value.is_empty()
        {
            found = Some(value);
        }
        start = after;
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_value(bytes: &[u8]) -> Option<String> {
    let mut index = 0;
    while let Some(byte) = bytes.get(index) {
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' | b'\0' => index += 1,
            b'%' => {
                while bytes.get(index).is_some_and(|byte| *byte != b'\n') {
                    index += 1;
                }
            }
            b'(' => return parse_literal(&bytes[index..]),
            b'<' if bytes.get(index + 1) != Some(&b'<') => return parse_hex(&bytes[index..]),
            _ => return None,
        }
    }
    None
}

fn parse_literal(bytes: &[u8]) -> Option<String> {
    let mut output = Vec::new();
    let mut depth = 1usize;
    let mut index = 1;
    while let Some(byte) = bytes.get(index).copied() {
        match byte {
            b'\\' => {
                index += 1;
                let escaped = *bytes.get(index)?;
                match escaped {
                    b'n' => output.push(b'\n'),
                    b'r' => output.push(b'\r'),
                    b't' => output.push(b'\t'),
                    b'b' => output.push(0x08),
                    b'f' => output.push(0x0c),
                    b'(' | b')' | b'\\' => output.push(escaped),
                    b'\r' => {
                        if bytes.get(index + 1) == Some(&b'\n') {
                            index += 1;
                        }
                    }
                    b'\n' => {}
                    b'0'..=b'7' => {
                        let mut value = (escaped - b'0') as u32;
                        for _ in 0..2 {
                            match bytes.get(index + 1) {
                                Some(next @ b'0'..=b'7') => {
                                    value = value * 8 + (next - b'0') as u32;
                                    index += 1;
                                }
                                _ => break,
                            }
                        }
                        output.push(value as u8);
                    }
                    other => output.push(other),
                }
            }
            b'(' => {
                depth += 1;
                output.push(byte);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(decode_text(&output));
                }
                output.push(byte);
            }
            other => output.push(other),
        }
        index += 1;
    }
    None
}

fn parse_hex(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|byte| *byte == b'>')?;
    let digits: Vec<u8> = bytes[1..end]
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let mut output = Vec::with_capacity(digits.len().div_ceil(2));
    let mut index = 0;
    while index < digits.len() {
        let high = hex_value(digits[index])?;
        let low = digits
            .get(index + 1)
            .map_or(Some(0), |byte| hex_value(*byte))?;
        output.push(high << 4 | low);
        index += 2;
    }
    Some(decode_text(&output))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_text(bytes: &[u8]) -> String {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_matches('\0').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_literal_modification_date() {
        let bytes = b"<</Title(T)/ModDate(D:20240203040506Z)>>";
        assert_eq!(
            modification_date_from_bytes(bytes).as_deref(),
            Some("D:20240203040506Z")
        );
    }

    #[test]
    fn reads_escaped_and_hex_strings() {
        let escaped = br"/ModDate (D:2024\05001\051)";
        assert_eq!(
            modification_date_from_bytes(escaped).as_deref(),
            Some("D:2024(01)")
        );

        let hex =
            b"/ModDate <FEFF0044003A00320030003200340030003200300033003000340030003500300036005A>";
        assert_eq!(
            modification_date_from_bytes(hex).as_deref(),
            Some("D:20240203040506Z")
        );
    }

    #[test]
    fn missing_or_empty_values_are_ignored() {
        assert_eq!(modification_date_from_bytes(b"<</Title(T)>>"), None);
        assert_eq!(modification_date_from_bytes(b"/ModDate ()"), None);
        assert_eq!(
            modification_date_from_bytes(b"/ModificationDate(D:2024)"),
            None
        );
        assert_eq!(modification_date_from_bytes(b"/ModDateX(D:2024)"), None);
    }

    #[test]
    fn the_last_occurrence_wins() {
        let bytes = b"/ModDate(D:1) /ModDate(D:2)";
        assert_eq!(modification_date_from_bytes(bytes).as_deref(), Some("D:2"));
    }
}

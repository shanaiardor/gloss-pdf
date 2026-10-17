//! 字体加载：WOFF 解包、cmap 覆盖范围，用于按字符挑选 CSS 字体列表里的字体。
//!
//! GPUI 的自定义字体只能按 family 命中，没有真正的逐字回退；这里先读出每个
//! 字体覆盖的码位，渲染前把文本切成"每个字符用列表里第一个覆盖它的字体"。

use std::collections::HashMap;
use std::io::Read;

/// 已加载的字体集合。
#[derive(Default)]
pub struct FontLibrary {
    faces: Vec<(String, Vec<u8>)>,
    coverage: HashMap<String, Vec<(u32, u32)>>,
}

impl FontLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入一个 TTF/OTF/WOFF 字体；返回是否成功解析。
    pub fn add(&mut self, family: &str, bytes: &[u8]) -> bool {
        let key = normalize(family);
        if self.coverage.contains_key(&key) {
            return true;
        }
        let Some(sfnt) = to_sfnt(bytes) else {
            log::warn!("跳过不支持的字体格式: {family}");
            return false;
        };
        if let Some(ranges) = cmap_ranges(&sfnt) {
            self.coverage.insert(key, ranges);
        }
        self.faces.push((family.to_string(), sfnt));
        true
    }

    /// `None` 表示这是系统字体（没有覆盖范围信息），按可用处理。
    pub fn covers(&self, family: &str, ch: char) -> Option<bool> {
        let key = normalize(family);
        let ranges = self.coverage.get(&key)?;
        let code = ch as u32;
        let found = ranges
            .binary_search_by(|(start, end)| {
                if code < *start {
                    std::cmp::Ordering::Greater
                } else if code > *end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok();
        Some(found)
    }

    pub fn faces(&self) -> &[(String, Vec<u8>)] {
        &self.faces
    }

    pub fn glyph_ranges(&self, family: &str) -> usize {
        self.coverage
            .get(&normalize(family))
            .map(Vec::len)
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// 交给 UI 注册的字体字节。
    pub fn bytes(&self) -> Vec<Vec<u8>> {
        self.faces.iter().map(|(_, bytes)| bytes.clone()).collect()
    }
}

fn normalize(family: &str) -> String {
    family.trim().trim_matches(['"', '\'']).to_ascii_lowercase()
}

/// TTF/OTF 原样返回；WOFF 解包成 sfnt；WOFF2 暂不支持。
fn to_sfnt(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(b"wOFF") {
        return woff_to_sfnt(bytes);
    }
    if bytes.starts_with(b"wOF2") {
        return None;
    }
    if bytes.starts_with(&[0x00, 0x01, 0x00, 0x00])
        || bytes.starts_with(b"OTTO")
        || bytes.starts_with(b"true")
        || bytes.starts_with(b"ttcf")
    {
        return Some(bytes.to_vec());
    }
    None
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn woff_to_sfnt(bytes: &[u8]) -> Option<Vec<u8>> {
    let flavor = u32_at(bytes, 4)?;
    let num_tables = u16_at(bytes, 12)? as usize;
    if num_tables == 0 || num_tables > 4096 {
        return None;
    }

    struct WoffTable {
        tag: [u8; 4],
        checksum: u32,
        data: Vec<u8>,
    }

    let mut tables = Vec::with_capacity(num_tables);
    for index in 0..num_tables {
        let base = 44 + index * 20;
        let tag: [u8; 4] = bytes.get(base..base + 4)?.try_into().ok()?;
        let offset = u32_at(bytes, base + 4)? as usize;
        let comp_length = u32_at(bytes, base + 8)? as usize;
        let orig_length = u32_at(bytes, base + 12)? as usize;
        let checksum = u32_at(bytes, base + 16)?;
        if orig_length > 64 * 1024 * 1024 {
            return None;
        }
        let raw = bytes.get(offset..offset.checked_add(comp_length)?)?;
        let data = if comp_length < orig_length {
            zlib_decode(raw, orig_length)?
        } else {
            raw.to_vec()
        };
        if data.len() != orig_length {
            return None;
        }
        tables.push(WoffTable {
            tag,
            checksum,
            data,
        });
    }
    tables.sort_by_key(|table| table.tag);

    let table_count = tables.len() as u16;
    let entry_selector = (table_count as f32).log2().floor() as u16;
    let search_range = (1u16 << entry_selector).saturating_mul(16);
    let range_shift = table_count.saturating_mul(16).saturating_sub(search_range);

    let mut output = Vec::new();
    output.extend_from_slice(&flavor.to_be_bytes());
    output.extend_from_slice(&table_count.to_be_bytes());
    output.extend_from_slice(&search_range.to_be_bytes());
    output.extend_from_slice(&entry_selector.to_be_bytes());
    output.extend_from_slice(&range_shift.to_be_bytes());

    let directory_start = output.len();
    output.resize(directory_start + tables.len() * 16, 0);

    let mut data_offset = output.len();
    let mut head_offset = None;
    for (index, table) in tables.iter().enumerate() {
        let record = directory_start + index * 16;
        output[record..record + 4].copy_from_slice(&table.tag);
        output[record + 4..record + 8].copy_from_slice(&table.checksum.to_be_bytes());
        output[record + 8..record + 12].copy_from_slice(&(data_offset as u32).to_be_bytes());
        output[record + 12..record + 16].copy_from_slice(&(table.data.len() as u32).to_be_bytes());
        if &table.tag == b"head" {
            head_offset = Some(data_offset);
        }
        output.extend_from_slice(&table.data);
        while output.len() % 4 != 0 {
            output.push(0);
        }
        data_offset = output.len();
    }

    if let Some(head) = head_offset {
        if head + 12 > output.len() {
            return None;
        }
        // 先清零 checkSumAdjustment，再重算整个字体。
        output[head + 8..head + 12].fill(0);
        let adjustment = 0xB1B0_AFBAu32.wrapping_sub(checksum(&output));
        output[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
        let head_length = tables
            .iter()
            .find(|table| &table.tag == b"head")
            .map(|table| table.data.len())?;
        let head_sum = checksum(&output[head..head + head_length]);
        for index in 0..tables.len() {
            let record = directory_start + index * 16;
            if &output[record..record + 4] == b"head" {
                output[record + 4..record + 8].copy_from_slice(&head_sum.to_be_bytes());
                break;
            }
        }
    }

    Some(output)
}

fn zlib_decode(input: &[u8], expected: usize) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(expected.min(16 * 1024 * 1024));
    flate2::read::ZlibDecoder::new(input)
        .take(expected as u64 + 1)
        .read_to_end(&mut output)
        .ok()?;
    Some(output)
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in bytes.chunks(4) {
        let mut padded = [0u8; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(padded));
    }
    sum
}

fn cmap_ranges(sfnt: &[u8]) -> Option<Vec<(u32, u32)>> {
    let cmap = table(sfnt, b"cmap")?;
    let num_tables = u16_at(cmap, 2)? as usize;
    let mut format12 = None;
    let mut format4 = None;
    for index in 0..num_tables {
        let base = 4 + index * 8;
        let platform = u16_at(cmap, base)?;
        let encoding = u16_at(cmap, base + 2)?;
        let offset = u32_at(cmap, base + 4)? as usize;
        let subtable = cmap.get(offset..)?;
        match u16_at(subtable, 0)? {
            12 if platform == 3 && encoding == 10 || platform == 0 => {
                format12.get_or_insert(subtable);
            }
            4 if platform == 3 && encoding == 1 || platform == 0 => {
                format4.get_or_insert(subtable);
            }
            _ => {}
        }
    }

    let ranges = if let Some(table) = format12 {
        cmap_format12(table)?
    } else if let Some(table) = format4 {
        cmap_format4(table)?
    } else {
        return None;
    };
    Some(merge_ranges(ranges))
}

fn table<'a>(sfnt: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let num_tables = u16_at(sfnt, 4)? as usize;
    for index in 0..num_tables {
        let record = 12 + index * 16;
        if sfnt.get(record..record + 4)? != &tag[..] {
            continue;
        }
        let offset = u32_at(sfnt, record + 8)? as usize;
        let length = u32_at(sfnt, record + 12)? as usize;
        return sfnt.get(offset..offset.checked_add(length)?);
    }
    None
}

fn cmap_format12(table: &[u8]) -> Option<Vec<(u32, u32)>> {
    let groups = u32_at(table, 12)? as usize;
    if groups > 1_000_000 {
        return None;
    }
    let mut ranges = Vec::with_capacity(groups);
    for index in 0..groups {
        let base = 16 + index * 12;
        let start = u32_at(table, base)?;
        let end = u32_at(table, base + 4)?;
        if start <= end {
            ranges.push((start, end));
        }
    }
    Some(ranges)
}

fn cmap_format4(table: &[u8]) -> Option<Vec<(u32, u32)>> {
    let seg_count = u16_at(table, 6)? as usize / 2;
    let end_codes = 14;
    let start_codes = end_codes + seg_count * 2 + 2;
    let mut ranges = Vec::new();
    for index in 0..seg_count {
        let end = u16_at(table, end_codes + index * 2)? as u32;
        let start = u16_at(table, start_codes + index * 2)? as u32;
        if start <= end && end != 0xFFFF {
            ranges.push((start, end));
        }
    }
    Some(ranges)
}

fn merge_ranges(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= last_end.saturating_add(1) => {
                *last_end = (*last_end).max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_ranges_combines_adjacent() {
        assert_eq!(
            merge_ranges(vec![(10, 20), (21, 30), (40, 40)]),
            vec![(10, 30), (40, 40)]
        );
    }

    #[test]
    fn family_names_are_case_insensitive() {
        let mut library = FontLibrary::new();
        library.coverage.insert(
            normalize("WL-Special"),
            vec![(0x2776, 0x2776), (0xFF5E, 0xFF5E)],
        );
        assert_eq!(library.covers("wl-special", '❶'), Some(true));
        assert_eq!(library.covers("WL-SPECIAL", '～'), Some(true));
        assert_eq!(library.covers("wl-special", 'a'), Some(false));
        assert_eq!(library.covers("unknown", 'a'), None);
    }
}

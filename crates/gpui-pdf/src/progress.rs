//! 阅读进度：当前页、页内滚动比例、缩放和适应模式。
//!
//! 进度跟着「最近阅读」条目一起写进 `recent.json`，下次打开同一个文件时由
//! `Viewer::restore_progress` 恢复。这里只放与 UI 无关的数据和换算。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::layout::FitMode;

/// 一次阅读快照，`page` 是 0 基页码。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadingProgress {
    pub page: usize,
    /// 保存进度时的总页数；与当前文件页数不一致说明文件已经变过。
    #[serde(default)]
    pub pages: usize,
    #[serde(default)]
    pub zoom: f32,
    /// 上次的缩放方式；`None` 只出现在没有这个字段的旧版本记录里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<FitMode>,
    /// 页内横向滚动比例，0 表示最左。
    #[serde(default)]
    pub scroll_x: f32,
    /// 页内纵向滚动比例，0 表示顶部。
    #[serde(default)]
    pub scroll_y: f32,
    /// Unix 秒。
    #[serde(default)]
    pub updated_at: u64,
}

impl ReadingProgress {
    /// 是否仍然是同一个阅读位置（忽略保存时间）。
    ///
    /// 滚动采样每一两秒跑一次，浮点尾巴和保存时间不应该导致重复写盘。
    pub fn same_position(&self, other: &Self) -> bool {
        self.page == other.page
            && self.pages == other.pages
            && self.fit == other.fit
            && (self.zoom - other.zoom).abs() < 0.0001
            && (self.scroll_x - other.scroll_x).abs() < 0.002
            && (self.scroll_y - other.scroll_y).abs() < 0.002
    }

    /// 0..=1 的阅读比例，用于首页卡片。
    pub fn fraction(&self) -> f32 {
        if self.pages == 0 {
            return 0.0;
        }
        let within_page = self.scroll_y.clamp(0.0, 1.0);
        ((self.page as f32 + within_page) / self.pages as f32).clamp(0.0, 1.0)
    }

    /// 0..=100 的阅读百分比。
    pub fn percent(&self) -> u32 {
        (self.fraction() * 100.0).round() as u32
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_combines_page_and_scroll() {
        let progress = ReadingProgress {
            page: 2,
            pages: 10,
            scroll_y: 0.5,
            ..Default::default()
        };
        assert!((progress.fraction() - 0.25).abs() < f32::EPSILON);
        assert_eq!(progress.percent(), 25);

        let at_end = ReadingProgress {
            page: 9,
            pages: 10,
            scroll_y: 1.0,
            ..Default::default()
        };
        assert_eq!(at_end.fraction(), 1.0);
        assert_eq!(at_end.percent(), 100);

        let empty = ReadingProgress::default();
        assert_eq!(empty.fraction(), 0.0);
    }

    #[test]
    fn same_position_ignores_timestamp_and_float_noise() {
        let first = ReadingProgress {
            page: 1,
            pages: 4,
            zoom: 1.25,
            fit: None,
            scroll_x: 0.0,
            scroll_y: 0.5001,
            updated_at: 10,
        };
        let second = ReadingProgress {
            updated_at: 99,
            scroll_y: 0.5004,
            ..first
        };
        assert!(first.same_position(&second));

        let moved = ReadingProgress {
            page: 2,
            updated_at: 99,
            ..second
        };
        assert!(!first.same_position(&moved));
    }
}

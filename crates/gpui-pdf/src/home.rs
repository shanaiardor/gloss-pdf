//! 首页状态：最近阅读记录、搜索过滤、侧栏选中项。
//!
//! 最近阅读持久化在用户数据目录（Windows: `%APPDATA%\gpui-pdf\recent.json`），
//! 每条记录同时保存最后阅读位置（页码、页内滚动和缩放），但不复制 PDF。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::progress::{ReadingProgress, unix_now};

/// 最多保留的最近阅读条目。
const MAX_RECENTS: usize = 18;

/// 左侧导航项。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HomeNav {
    #[default]
    Home,
    Recent,
    Favorites,
    Tools,
    Settings,
}

impl HomeNav {
    pub fn label(self) -> &'static str {
        match self {
            HomeNav::Home => "主页",
            HomeNav::Recent => "最近",
            HomeNav::Favorites => "收藏",
            HomeNav::Tools => "工具",
            HomeNav::Settings => "设置",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            HomeNav::Home => "⌂",
            HomeNav::Recent => "◷",
            HomeNav::Favorites => "☆",
            // 显式文本变体，避免 Windows 把扳手/齿轮渲染成彩色 emoji。
            HomeNav::Tools => "\u{2692}\u{fe0e}",
            HomeNav::Settings => "\u{2699}\u{fe0e}",
        }
    }

    /// 尚未实现的页面在点击时的提示。
    pub fn placeholder_message(self) -> Option<&'static str> {
        match self {
            HomeNav::Home => None,
            HomeNav::Recent => Some("最近阅读已经在首页展示"),
            HomeNav::Favorites => Some("收藏功能即将上线"),
            HomeNav::Tools => Some("工具功能即将上线"),
            HomeNav::Settings => Some("设置功能即将上线"),
        }
    }
}

/// 一条最近阅读记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentDocument {
    pub path: PathBuf,
    pub title: String,
    /// Unix 秒。
    pub opened_at: u64,
    /// 上次读到的位置；旧版本记录没有这个字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ReadingProgress>,
}

/// 首页控制器状态。
#[derive(Default)]
pub struct HomeState {
    pub nav: HomeNav,
    pub query: String,
    pub search_focused: bool,
    /// 当前展开的“...”菜单（最近阅读中的绝对下标）。
    pub menu_open: Option<usize>,
    pub recents: Vec<RecentDocument>,
    /// `None` 表示不落到磁盘（测试或无法确定用户目录）。
    storage: Option<PathBuf>,
}

impl HomeState {
    /// 从用户数据目录读取最近阅读。
    pub fn load() -> Self {
        let storage = recent_file_path();
        let mut recents: Vec<RecentDocument> = storage
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();

        // 文件被移动或删除后不再展示死条目。
        recents.retain(|recent| recent.path.is_file());
        recents.sort_by(|left, right| right.opened_at.cmp(&left.opened_at));
        recents.truncate(MAX_RECENTS);

        Self {
            nav: HomeNav::Home,
            query: String::new(),
            search_focused: false,
            menu_open: None,
            recents,
            storage,
        }
    }

    /// 记录一次成功打开的文档。
    pub fn record(&mut self, path: PathBuf, title: String) {
        self.record_at(path, title, unix_now());
    }

    fn record_at(&mut self, path: PathBuf, title: String, now: u64) {
        // 重新打开同一个文件时保留上次的阅读位置，等恢复成功后再覆盖。
        let progress = self
            .recents
            .iter()
            .find(|recent| same_path(&recent.path, &path))
            .and_then(|recent| recent.progress);

        self.recents
            .retain(|recent| !same_path(&recent.path, &path));
        self.recents.insert(
            0,
            RecentDocument {
                path,
                title,
                opened_at: now,
                progress,
            },
        );
        self.recents.truncate(MAX_RECENTS);
        self.menu_open = None;
        self.flush();
    }

    /// 某个文件上次读到的位置。
    pub fn progress_for(&self, path: &Path) -> Option<ReadingProgress> {
        self.recents
            .iter()
            .find(|recent| same_path(&recent.path, path))
            .and_then(|recent| recent.progress)
    }

    /// 更新某个文件的阅读位置（只改内存，由调用方决定何时 `flush`）。
    pub fn set_progress(&mut self, path: &Path, progress: ReadingProgress) -> bool {
        let Some(recent) = self
            .recents
            .iter_mut()
            .find(|recent| same_path(&recent.path, path))
        else {
            return false;
        };
        recent.progress = Some(progress);
        true
    }

    /// 从最近阅读移除一条。
    pub fn remove(&mut self, index: usize) {
        if index < self.recents.len() {
            self.recents.remove(index);
            self.menu_open = None;
            self.flush();
        }
    }

    /// 过滤后的条目下标（保持时间倒序）。
    pub fn filtered(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.recents
            .iter()
            .enumerate()
            .filter(|(_, recent)| query.is_empty() || recent.title.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect()
    }

    /// 当前搜索是否真的在过滤。
    pub fn is_filtering(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// 把最近阅读（含阅读进度）写回用户数据目录。
    pub fn flush(&self) {
        let Some(path) = self.storage.as_deref() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!("无法创建最近阅读目录 {}: {error}", parent.display());
            return;
        }
        match serde_json::to_string_pretty(&self.recents) {
            Ok(json) => {
                if let Err(error) = std::fs::write(path, json) {
                    log::warn!("无法写入最近阅读 {}: {error}", path.display());
                }
            }
            Err(error) => log::warn!("无法序列化最近阅读: {error}"),
        }
    }
}

/// 相对时间文案，例如「刚刚」「2 小时前」「昨天」。
pub fn relative_time(opened_at: u64) -> String {
    relative_time_at(opened_at, unix_now())
}

fn relative_time_at(opened_at: u64, now: u64) -> String {
    let elapsed = now.saturating_sub(opened_at);
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    if elapsed < MINUTE {
        "刚刚".to_string()
    } else if elapsed < HOUR {
        format!("{} 分钟前", elapsed / MINUTE)
    } else if elapsed < DAY {
        format!("{} 小时前", elapsed / HOUR)
    } else if elapsed < 2 * DAY {
        "昨天".to_string()
    } else if elapsed < 7 * DAY {
        format!("{} 天前", elapsed / DAY)
    } else {
        format_date(opened_at)
    }
}

fn format_date(timestamp: u64) -> String {
    let (year, month, day) = civil_from_days((timestamp / 86_400) as i64);
    format!("{year}-{month:02}-{day:02}")
}

/// Howard Hinnant 的 days → civil date 算法（UTC）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    // Windows 路径大小写不敏感；这里也兼容用户从不同大小写路径打开同一文件。
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn recent_file_path() -> Option<PathBuf> {
    let directory = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("gpui-pdf"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(PathBuf::from).map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("gpui-pdf")
        })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|path| path.join(".local").join("share"))
            })
            .map(|path| path.join("gpui-pdf"))
    }?;

    Some(directory.join("recent.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::FitMode;

    fn home_without_storage() -> HomeState {
        HomeState {
            storage: None,
            ..Default::default()
        }
    }

    #[test]
    fn recent_entries_are_deduplicated_and_ordered_newest_first() {
        let mut home = home_without_storage();
        let first = PathBuf::from("C:/docs/a.pdf");
        let second = PathBuf::from("C:/docs/b.pdf");

        home.record_at(first.clone(), "a.pdf".into(), 100);
        home.record_at(second.clone(), "b.pdf".into(), 200);
        home.record_at(first, "a.pdf".into(), 300);

        assert_eq!(home.recents.len(), 2);
        assert_eq!(home.recents[0].title, "a.pdf");
        assert_eq!(home.recents[0].opened_at, 300);
        assert_eq!(home.recents[1].title, "b.pdf");
    }

    #[test]
    fn progress_is_kept_when_reopening_and_removed_with_the_entry() {
        let mut home = home_without_storage();
        let path = PathBuf::from("C:/docs/a.pdf");
        home.record_at(path.clone(), "a.pdf".into(), 100);

        let progress = ReadingProgress {
            page: 3,
            pages: 10,
            zoom: 1.5,
            fit: None,
            scroll_x: 0.0,
            scroll_y: 0.5,
            updated_at: 200,
        };
        assert!(home.set_progress(&path, progress));
        assert_eq!(home.progress_for(&path), Some(progress));

        // 再次打开时 opened_at 会更新，但阅读位置保留。
        home.record_at(path.clone(), "a.pdf".into(), 300);
        assert_eq!(home.progress_for(&path), Some(progress));

        home.remove(0);
        assert_eq!(home.progress_for(&path), None);
    }

    #[test]
    fn legacy_recent_json_without_progress_still_parses() {
        let legacy = r#"[
            {"path":"C:/docs/a.pdf","title":"a.pdf","opened_at":1700000000}
        ]"#;
        let recents: Vec<RecentDocument> = serde_json::from_str(legacy).unwrap();
        assert_eq!(recents.len(), 1);
        assert!(recents[0].progress.is_none());
    }

    #[test]
    fn legacy_progress_without_fit_still_parses() {
        let legacy = r#"{"page":1,"pages":5,"zoom":1.2,"scroll_x":0.0,"scroll_y":0.3,"updated_at":10}"#;
        let progress: ReadingProgress = serde_json::from_str(legacy).unwrap();
        assert_eq!(progress.fit, None);
        assert!((progress.zoom - 1.2).abs() < f32::EPSILON);
    }

    #[test]
    fn recent_json_round_trips_progress() {
        let progress = ReadingProgress {
            page: 2,
            pages: 8,
            zoom: 1.25,
            fit: Some(FitMode::Page),
            scroll_x: 0.1,
            scroll_y: 0.6,
            updated_at: 42,
        };
        let recents = vec![RecentDocument {
            path: PathBuf::from("C:/docs/a.pdf"),
            title: "a.pdf".into(),
            opened_at: 10,
            progress: Some(progress),
        }];
        let json = serde_json::to_string(&recents).unwrap();
        let parsed: Vec<RecentDocument> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0].progress, Some(progress));
    }

    #[test]
    fn search_filters_by_file_name() {
        let mut home = home_without_storage();
        home.record_at(PathBuf::from("C:/docs/alpha.pdf"), "Alpha.pdf".into(), 10);
        home.record_at(PathBuf::from("C:/docs/beta.pdf"), "Beta.pdf".into(), 20);

        home.query = "alp".into();
        let filtered = home.filtered();
        assert_eq!(filtered.len(), 1);
        assert_eq!(home.recents[filtered[0]].title, "Alpha.pdf");

        home.query = "不存在".into();
        assert!(home.filtered().is_empty());
    }

    #[test]
    fn relative_time_labels_match_ranges() {
        let now = 10 * 86_400 + 12 * 3_600;
        assert_eq!(relative_time_at(now - 10, now), "刚刚");
        assert_eq!(relative_time_at(now - 5 * 60, now), "5 分钟前");
        assert_eq!(relative_time_at(now - 3 * 3_600, now), "3 小时前");
        assert_eq!(relative_time_at(now - 30 * 3_600, now), "昨天");
        assert_eq!(relative_time_at(now - 4 * 86_400, now), "4 天前");
    }

    #[test]
    fn date_format_handles_epoch() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_700_000_000), "2023-11-14");
    }
}

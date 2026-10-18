//! ui 无关的数据模型：页面尺寸、字符包围盒、链接、目录项、高亮矩形。

use serde::{Deserialize, Serialize};

/// PDF 坐标下的矩形（单位 pt，原点在页面左下角，y 向上）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RectPt {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl RectPt {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    pub fn center(&self) -> (f32, f32) {
        (
            (self.left + self.right) * 0.5,
            (self.bottom + self.top) * 0.5,
        )
    }
}

/// 一个字符及其在页面上的包围盒。
#[derive(Clone, Debug)]
pub struct CharBox {
    pub ch: char,
    pub rect: RectPt,
}

/// 链接目标。
#[derive(Clone, Debug)]
pub enum LinkTarget {
    Uri(String),
    Page(usize),
}

/// 页面上的一个链接热区。
#[derive(Clone, Debug)]
pub struct LinkBox {
    pub rect: RectPt,
    pub target: LinkTarget,
}

/// 页面尺寸，单位 pt。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// 展平后的目录项。
#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub page: usize,
    pub depth: usize,
}

/// PDF 文档元数据；缺失的字段为 `None`，空字符串在读取时已被过滤。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
}

impl DocumentMetadata {
    /// 标题优先用 PDF 元数据，缺失或空白时回退到调用方给的名称（通常是文件名）。
    pub fn title_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(fallback)
    }

    /// 设置面板按固定顺序展示的字段。
    pub fn fields(&self) -> [(&'static str, Option<&str>); 8] {
        [
            ("标题", self.title.as_deref()),
            ("作者", self.author.as_deref()),
            ("主题", self.subject.as_deref()),
            ("关键词", self.keywords.as_deref()),
            ("创建者", self.creator.as_deref()),
            ("生成器", self.producer.as_deref()),
            ("创建时间", self.creation_date.as_deref()),
            ("修改时间", self.modification_date.as_deref()),
        ]
    }
}

/// 全文搜索时某一页的命中数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageHits {
    pub page: usize,
    pub count: usize,
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn missing_or_blank_title_falls_back_to_the_file_name() {
        let mut metadata = DocumentMetadata::default();
        assert_eq!(metadata.title_or("论文.pdf"), "论文.pdf");

        metadata.title = Some("   ".into());
        assert_eq!(metadata.title_or("论文.pdf"), "论文.pdf");

        metadata.title = Some("  A Study  ".into());
        assert_eq!(metadata.title_or("论文.pdf"), "A Study");
    }

    #[test]
    fn empty_metadata_still_lists_every_field() {
        let metadata = DocumentMetadata::default();
        let fields = metadata.fields();
        assert_eq!(fields.len(), 8);
        assert!(fields.iter().all(|(_, value)| value.is_none()));
        assert_eq!(fields[0].0, "标题");
        assert_eq!(fields[7].0, "修改时间");
    }
}

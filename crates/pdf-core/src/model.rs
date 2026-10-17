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

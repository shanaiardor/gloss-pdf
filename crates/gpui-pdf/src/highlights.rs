//! 高亮批注：内存模型 + 与 PDF 同名的 sidecar JSON 持久化。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pdf_core::RectPt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Highlight {
    pub page: usize,
    pub rects: Vec<RectPt>,
}

#[derive(Default)]
pub struct HighlightStore {
    pub items: Vec<Highlight>,
}

impl HighlightStore {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn add(&mut self, page: usize, rects: Vec<RectPt>) {
        if !rects.is_empty() {
            self.items.push(Highlight { page, rects });
        }
    }

    pub fn for_page(&self, page: usize) -> impl Iterator<Item = &Highlight> {
        self.items.iter().filter(move |highlight| highlight.page == page)
    }

    /// 读取 sidecar；文件不存在不是错误。
    pub fn load(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        match serde_json::from_str::<Vec<Highlight>>(&text) {
            Ok(items) => {
                self.items = items;
                log::info!("loaded {} highlights from {}", self.items.len(), path.display());
            }
            Err(error) => log::warn!("failed to parse {}: {error}", path.display()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.items)?;
        std::fs::write(path, json)
            .with_context(|| format!("无法写入 {}", path.display()))?;
        log::info!("saved {} highlights to {}", self.items.len(), path.display());
        Ok(())
    }
}

/// `<pdf 文件名>.highlights.json`，与 PDF 放在同一目录。
pub fn sidecar_path(pdf: &Path) -> PathBuf {
    let name = pdf
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "document.pdf".to_string());
    pdf.with_file_name(format!("{name}.highlights.json"))
}

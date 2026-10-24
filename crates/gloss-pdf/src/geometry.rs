//! 几何小工具：把逐字符矩形合并成按行的高亮块。

use pdf_core::RectPt;

/// 合并同一行内相邻/重叠的矩形，减少覆盖层元素数量。
pub fn merge_rects(rects: Vec<RectPt>) -> Vec<RectPt> {
    let mut sorted = rects;
    sorted.sort_by(|a, b| {
        b.bottom
            .partial_cmp(&a.bottom)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.left
                    .partial_cmp(&b.left)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut merged: Vec<RectPt> = Vec::new();
    for rect in sorted {
        if let Some(last) = merged.last_mut()
            && (last.bottom - rect.bottom).abs() < 0.5
            && (last.top - rect.top).abs() < 0.5
            && rect.left <= last.right + 0.5
        {
            last.right = last.right.max(rect.right);
            continue;
        }
        merged.push(rect);
    }
    merged
}

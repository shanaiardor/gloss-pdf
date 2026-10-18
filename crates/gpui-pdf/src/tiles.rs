//! 页面瓦片切分：只把视口覆盖到的瓦片交给 PDFium 渲染。
//!
//! 整页在 400% 缩放下的位图可能有几十 MB，一次性渲染整页既慢又占内存。
//! 这里按固定像素尺寸把整页切成网格，由 `app/render.rs` 用当前滚动偏移
//! 找出与视口相交的瓦片；覆盖层仍使用整页点坐标，所以文字、链接、高亮
//! 不需要知道瓦片的存在。

use pdf_core::PageSize;

/// 瓦片边长（像素）。512 对 A4 在 100% 下约 2×2 块，400% 下约 5×7 块。
pub const TILE_SIZE: u32 = 512;

/// 一个瓦片在整页像素坐标系里的位置与尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRect {
    pub col: u32,
    pub row: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: u32,
    pub height: u32,
}

/// 某一页在给定缩放下的瓦片网格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileGrid {
    pub full_width: u32,
    pub full_height: u32,
    pub columns: u32,
    pub rows: u32,
}

impl TileGrid {
    pub fn new(page: PageSize, scale: f32) -> Self {
        let full_width = (page.width * scale).round().max(1.0) as u32;
        let full_height = (page.height * scale).round().max(1.0) as u32;
        Self {
            full_width,
            full_height,
            columns: full_width.div_ceil(TILE_SIZE),
            rows: full_height.div_ceil(TILE_SIZE),
        }
    }

    /// 与页面像素矩形相交的瓦片。矩形按左上角为原点，超出页面的部分会被夹住。
    pub fn tiles_for_rect(
        &self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) -> Vec<TileRect> {
        let left = left.max(0.0);
        let top = top.max(0.0);
        let right = right.min(self.full_width as f32);
        let bottom = bottom.min(self.full_height as f32);
        if !left.is_finite()
            || !top.is_finite()
            || !right.is_finite()
            || !bottom.is_finite()
            || right <= left
            || bottom <= top
        {
            return Vec::new();
        }

        let tile = TILE_SIZE as f32;
        let col_start = (left / tile).floor() as u32;
        let row_start = (top / tile).floor() as u32;
        let col_end = ((right / tile).ceil() as u32).min(self.columns);
        let row_end = ((bottom / tile).ceil() as u32).min(self.rows);

        let mut tiles = Vec::with_capacity(
            ((col_end.saturating_sub(col_start)) * (row_end.saturating_sub(row_start))) as usize,
        );
        for row in row_start..row_end {
            for col in col_start..col_end {
                let origin_x = (col * TILE_SIZE) as i32;
                let origin_y = (row * TILE_SIZE) as i32;
                tiles.push(TileRect {
                    col,
                    row,
                    origin_x,
                    origin_y,
                    width: (self.full_width - col * TILE_SIZE).min(TILE_SIZE),
                    height: (self.full_height - row * TILE_SIZE).min(TILE_SIZE),
                });
            }
        }
        tiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4() -> PageSize {
        PageSize {
            width: 595.0,
            height: 842.0,
        }
    }

    #[test]
    fn a4_at_four_hundred_percent_is_split_into_a_grid() {
        let grid = TileGrid::new(a4(), 4.0 * 1.25);
        assert_eq!(grid.full_width, 2975);
        assert_eq!(grid.full_height, 4210);
        assert_eq!(grid.columns, 6);
        assert_eq!(grid.rows, 9);
    }

    #[test]
    fn only_tiles_intersecting_the_rect_are_returned() {
        let grid = TileGrid::new(a4(), 4.0);
        // 视口只覆盖页面中间一小块：不应把整页 5×7 个瓦片都请求出来。
        let tiles = grid.tiles_for_rect(700.0, 1000.0, 1500.0, 1700.0);
        assert!(tiles.len() < (grid.columns * grid.rows) as usize);
        assert_eq!(tiles.len(), 6);
        for tile in tiles {
            assert!(tile.origin_x < 1500 && tile.origin_x + tile.width as i32 > 700);
            assert!(tile.origin_y < 1700 && tile.origin_y + tile.height as i32 > 1000);
        }
    }

    #[test]
    fn edge_tiles_are_clipped_to_the_page() {
        let page = PageSize {
            width: 600.0,
            height: 600.0,
        };
        let grid = TileGrid::new(page, 1.0);
        let tiles = grid.tiles_for_rect(0.0, 0.0, 600.0, 600.0);
        assert_eq!(tiles.len(), 4);
        assert_eq!(tiles[0].width, TILE_SIZE);
        assert_eq!(tiles[3].width, 600 - TILE_SIZE);
        assert_eq!(tiles[3].height, 600 - TILE_SIZE);
    }

    #[test]
    fn empty_or_out_of_page_rects_request_nothing() {
        let grid = TileGrid::new(a4(), 1.0);
        assert!(grid.tiles_for_rect(10.0, 10.0, 10.0, 10.0).is_empty());
        assert!(grid.tiles_for_rect(-100.0, -100.0, -10.0, -10.0).is_empty());
        assert!(grid.tiles_for_rect(9999.0, 0.0, 10_000.0, 10.0).is_empty());
    }
}

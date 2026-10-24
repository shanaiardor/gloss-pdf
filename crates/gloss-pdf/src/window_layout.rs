//! 主窗口跨显示器工作区选择与保存的几何恢复。
//!
//! 这里只做纯计算，不依赖 GPUI，方便直接写单元测试。

/// 屏幕矩形（逻辑像素）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    /// 四个分量都是有限数且宽高为正；`NaN`/`0` 尺寸算无效几何。
    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
    }

    /// 两块矩形的重叠面积；不相交时为 0。
    pub fn overlap_area(self, other: RectF) -> f32 {
        let width = self.right().min(other.right()) - self.x.max(other.x);
        let height = self.bottom().min(other.bottom()) - self.y.max(other.y);
        if width <= 0.0 || height <= 0.0 {
            0.0
        } else {
            width * height
        }
    }

    pub fn area(self) -> f32 {
        self.width * self.height
    }
}

/// 在若干显示器工作区里挑与窗口框重叠最多的那个。
///
/// 副屏拔掉、分辨率变小之后旧坐标可能一点都不重叠，这时返回 `None`，
/// 由调用方退回主显示器。
pub fn best_work_area(window: RectF, works: &[RectF]) -> Option<RectF> {
    works
        .iter()
        .filter(|work| work.is_finite())
        .map(|work| (*work, window.overlap_area(*work)))
        .filter(|(_, overlap)| *overlap > 0.0)
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(work, _)| work)
}

/// 把上次保存的窗口框恢复到当前工作区。
///
/// 尺寸夹在最小窗口尺寸与工作区之间；位置先保留（副屏、负坐标都合法），
/// 只有窗口几乎全在工作区外时才改成居中，最后再夹进工作区——保证标题栏
/// 一定在屏幕上，不会出现“窗口在屏幕外打不开”的困境。
pub fn restore_window_bounds(saved: RectF, work: RectF, min_size: (f32, f32)) -> RectF {
    let width = clamp_extent(saved.width, min_size.0, work.width);
    let height = clamp_extent(saved.height, min_size.1, work.height);

    let overlap_ratio = if saved.is_finite() && work.is_finite() {
        saved.overlap_area(work) / saved.area()
    } else {
        0.0
    };
    let keep_origin = saved.is_finite() && overlap_ratio >= 0.5;

    let (x, y) = if keep_origin {
        (saved.x, saved.y)
    } else {
        (
            work.x + (work.width - width) / 2.0,
            work.y + (work.height - height) / 2.0,
        )
    };

    let x = clamp_axis(x, work.x, work.right() - width, work.x);
    let y = clamp_axis(y, work.y, work.bottom() - height, work.y);
    RectF::new(x, y, width, height)
}

/// 把长度夹进 `[min, max]`；工作区比最小尺寸还小时以工作区为准。
fn clamp_extent(value: f32, min: f32, max: f32) -> f32 {
    if !value.is_finite() {
        return min.min(max).max(0.0);
    }
    let min = min.min(max);
    value.clamp(min, max.max(min))
}

/// 夹在工作区内；工作区比窗口还小时退回工作区左上角，避免出现 NaN。
fn clamp_axis(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if max >= min {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RectF = RectF {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };
    const MIN_WINDOW: (f32, f32) = (720.0, 480.0);

    #[test]
    fn saved_bounds_are_restored_verbatim_when_they_fit() {
        let saved = RectF::new(240.0, 120.0, 1000.0, 700.0);
        assert_eq!(restore_window_bounds(saved, WORK, MIN_WINDOW), saved);
    }

    #[test]
    fn negative_coordinates_from_a_left_hand_monitor_are_kept() {
        let left_work = RectF::new(-1920.0, 0.0, 1920.0, 1080.0);
        let saved = RectF::new(-1800.0, 100.0, 1000.0, 700.0);
        assert_eq!(best_work_area(saved, &[left_work, WORK]), Some(left_work));
        assert_eq!(restore_window_bounds(saved, left_work, MIN_WINDOW), saved);
    }

    #[test]
    fn unplugged_monitor_falls_back_to_the_remaining_work_area() {
        let saved = RectF::new(1800.0, 100.0, 1000.0, 700.0);
        assert_eq!(best_work_area(saved, &[WORK]), Some(WORK));

        let restored = restore_window_bounds(saved, WORK, MIN_WINDOW);
        assert_eq!(restored.x, (1920.0 - 1000.0) / 2.0);
        assert_eq!(restored.y, (1080.0 - 700.0) / 2.0);
    }

    #[test]
    fn window_outside_every_display_has_no_work_area() {
        let saved = RectF::new(4000.0, 100.0, 1000.0, 700.0);
        assert_eq!(best_work_area(saved, &[WORK]), None);
    }

    #[test]
    fn mostly_visible_window_keeps_its_position_but_is_pulled_inside() {
        let saved = RectF::new(1400.0, 100.0, 1000.0, 700.0);
        let restored = restore_window_bounds(saved, WORK, MIN_WINDOW);
        assert_eq!(restored.x, 1920.0 - 1000.0);
        assert_eq!(restored.y, 100.0);
        assert_eq!((restored.width, restored.height), (1000.0, 700.0));
    }

    #[test]
    fn half_offscreen_window_is_centered() {
        let saved = RectF::new(1700.0, 100.0, 1000.0, 700.0);
        let restored = restore_window_bounds(saved, WORK, MIN_WINDOW);
        assert_eq!(restored.x, (1920.0 - 1000.0) / 2.0);
        assert_eq!(restored.y, (1080.0 - 700.0) / 2.0);
    }

    #[test]
    fn oversized_and_tiny_sizes_are_clamped() {
        let huge = restore_window_bounds(RectF::new(0.0, 0.0, 5000.0, 4000.0), WORK, MIN_WINDOW);
        assert_eq!((huge.width, huge.height), (1920.0, 1080.0));
        assert_eq!((huge.x, huge.y), (0.0, 0.0));

        let tiny = restore_window_bounds(RectF::new(100.0, 100.0, 10.0, 10.0), WORK, MIN_WINDOW);
        assert_eq!((tiny.width, tiny.height), MIN_WINDOW);
    }

    #[test]
    fn degenerate_saved_bounds_fall_back_to_the_center() {
        let saved = RectF::new(f32::NAN, 0.0, 0.0, 0.0);
        let restored = restore_window_bounds(saved, WORK, MIN_WINDOW);
        assert!(restored.is_finite());
        assert_eq!((restored.width, restored.height), MIN_WINDOW);
        assert_eq!(
            (restored.x, restored.y),
            ((1920.0 - 720.0) / 2.0, (1080.0 - 480.0) / 2.0)
        );
    }

    #[test]
    fn work_area_smaller_than_the_minimum_size_wins() {
        let work = RectF::new(0.0, 0.0, 480.0, 320.0);
        let restored = restore_window_bounds(RectF::new(0.0, 0.0, 900.0, 700.0), work, MIN_WINDOW);
        assert_eq!((restored.width, restored.height), (480.0, 320.0));
        assert_eq!((restored.x, restored.y), (0.0, 0.0));
    }
}

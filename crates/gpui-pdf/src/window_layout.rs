//! 无边框浮窗（词典窗口）的初始位置与边缘吸附。
//!
//! 这里只做纯计算，不依赖 GPUI，方便直接写单元测试；真正移动窗口在
//! `window_move.rs` 里按平台实现。

/// 浮窗离显示器工作区边缘留出的空隙。
pub const SCREEN_MARGIN: f32 = 12.0;
/// 拖到主窗口边缘多近时自动吸附。
pub const SNAP_DISTANCE: f32 = 24.0;

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
}

/// 浮窗默认位置：优先贴主窗口右侧，放不下再放左侧，最后夹在工作区内。
///
/// `gap` 是浮窗与主窗口之间留的空隙；`work` 是当前显示器的工作区。
pub fn adjacent_origin(main: RectF, size: (f32, f32), work: RectF, gap: f32) -> (f32, f32) {
    let (width, height) = size;

    let mut x = main.right() + gap;
    if x + width > work.right() - SCREEN_MARGIN {
        x = main.x - width - gap;
    }
    let x = clamp_axis(
        x,
        work.x + SCREEN_MARGIN,
        work.right() - width - SCREEN_MARGIN,
        work.x,
    );
    let y = clamp_axis(
        main.y,
        work.y + SCREEN_MARGIN,
        work.bottom() - height - SCREEN_MARGIN,
        work.y,
    );

    (x, y)
}

/// 把浮窗左上角吸附到主窗口或工作区边缘，并保证不出工作区。
///
/// 主窗口优先：浮窗靠近主窗口左右两侧时贴成 0 间距，同时把上下位置对齐主窗口。
/// 没有贴主窗口时再吸附工作区四边。最后统一夹进工作区。
pub fn snap_origin(
    origin: (f32, f32),
    size: (f32, f32),
    main: Option<RectF>,
    work: RectF,
) -> (f32, f32) {
    let (width, height) = size;
    let (mut x, mut y) = origin;

    let mut attached_to_main = false;
    if let Some(main) = main
        && let Some(target) = snap_axis(x, [main.right(), main.x - width], SNAP_DISTANCE)
    {
        x = target;
        attached_to_main = true;
        if let Some(target) = snap_axis(y, [main.y, main.bottom() - height], SNAP_DISTANCE) {
            y = target;
        }
    }

    if !attached_to_main
        && let Some(target) = snap_axis(
            x,
            [work.x + SCREEN_MARGIN, work.right() - width - SCREEN_MARGIN],
            SNAP_DISTANCE,
        )
    {
        x = target;
    }
    if let Some(target) = snap_axis(
        y,
        [
            work.y + SCREEN_MARGIN,
            work.bottom() - height - SCREEN_MARGIN,
        ],
        SNAP_DISTANCE,
    ) {
        y = target;
    }

    x = clamp_axis(
        x,
        work.x + SCREEN_MARGIN,
        work.right() - width - SCREEN_MARGIN,
        work.x,
    );
    y = clamp_axis(
        y,
        work.y + SCREEN_MARGIN,
        work.bottom() - height - SCREEN_MARGIN,
        work.y,
    );

    (x, y)
}

/// 在若干候选坐标里选最近的吸附点；返回 `None` 表示都不够近。
fn snap_axis(value: f32, targets: [f32; 2], threshold: f32) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for target in targets {
        let distance = (value - target).abs();
        if distance <= threshold && best.is_none_or(|(_, best)| distance < best) {
            best = Some((target, distance));
        }
    }
    best.map(|(target, _)| target)
}

/// 夹在工作区内；工作区比浮窗还小时退回工作区左上角，避免出现 NaN。
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
    const SIZE: (f32, f32) = (380.0, 720.0);

    fn main_window() -> RectF {
        RectF::new(100.0, 100.0, 800.0, 600.0)
    }

    #[test]
    fn adjacent_prefers_the_right_side() {
        let (x, y) = adjacent_origin(main_window(), SIZE, WORK, 12.0);
        assert_eq!(x, 912.0);
        assert_eq!(y, 100.0);
    }

    #[test]
    fn adjacent_falls_back_to_the_left_side() {
        let main = RectF::new(1100.0, 100.0, 800.0, 600.0);
        let (x, y) = adjacent_origin(main, SIZE, WORK, 12.0);
        // 右侧放不下：1100 + 800 + 12 + 380 超出工作区。
        assert_eq!(x, 1100.0 - 380.0 - 12.0);
        assert_eq!(y, 100.0);
    }

    #[test]
    fn adjacent_clamps_on_a_small_screen() {
        let main = RectF::new(100.0, 100.0, 800.0, 600.0);
        let work = RectF::new(0.0, 0.0, 900.0, 800.0);
        let (x, y) = adjacent_origin(main, (500.0, 700.0), work, 12.0);
        assert_eq!(x, SCREEN_MARGIN);
        assert_eq!(y, 800.0 - 700.0 - SCREEN_MARGIN);
    }

    #[test]
    fn snaps_flush_to_the_main_window_right_edge() {
        let main = main_window();
        // 主窗口右边 900，浮窗左边 916：进入吸附距离。
        let (x, y) = snap_origin((916.0, 120.0), SIZE, Some(main), WORK);
        assert_eq!(x, 900.0);
        // 上下也对齐主窗口。
        assert_eq!(y, 100.0);
    }

    #[test]
    fn snaps_flush_to_the_main_window_left_edge() {
        let main = RectF::new(500.0, 100.0, 800.0, 600.0);
        let (x, y) = snap_origin((104.0, 90.0), SIZE, Some(main), WORK);
        assert_eq!(x, 500.0 - 380.0);
        assert_eq!(y, 100.0);
    }

    #[test]
    fn does_not_snap_when_far_away() {
        let main = main_window();
        let (x, y) = snap_origin((1200.0, 300.0), SIZE, Some(main), WORK);
        assert_eq!((x, y), (1200.0, 300.0));
    }

    #[test]
    fn snaps_to_work_area_edges() {
        let (x, y) = snap_origin((1516.0, 10.0), SIZE, None, WORK);
        assert_eq!(x, WORK.right() - SIZE.0 - SCREEN_MARGIN);
        assert_eq!(y, SCREEN_MARGIN);
    }

    #[test]
    fn clamps_inside_the_work_area() {
        let (x, y) = snap_origin((3000.0, -200.0), SIZE, None, WORK);
        assert_eq!(x, WORK.right() - SIZE.0 - SCREEN_MARGIN);
        assert_eq!(y, SCREEN_MARGIN);
    }

    #[test]
    fn small_work_area_keeps_the_origin_inside() {
        let work = RectF::new(0.0, 0.0, 300.0, 200.0);
        let (x, y) = snap_origin((150.0, 100.0), SIZE, None, work);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn main_window_wins_over_the_work_area() {
        let main = RectF::new(0.0, 0.0, 1200.0, 800.0);
        let (x, _) = snap_origin((1210.0, 100.0), SIZE, Some(main), WORK);
        assert_eq!(x, 1200.0);
    }
}

//! 无边框浮窗（词典窗口）的初始位置、边缘吸附与跟随主窗口。
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

    // 留着原位置的前提：窗口有一半以上还落在工作区里。低于这个比例说明
    // 屏幕配置变了（副屏拔掉、分辨率调小），继续留在原处用户基本看不见。
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

/// 浮窗贴在主窗口的哪一侧。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// 浮窗是否正贴着主窗口的左右两侧，贴在哪边。
///
/// 判据是「竖直方向与主窗口有交集」且「水平间距在 `-2 ..= SNAP_DISTANCE`
/// 之间」：初始摆放留了 12px 空隙，拖动吸附后是 0 间距，两种都算贴着。
/// 用户把浮窗拖远之后就不再跟随，拖回来又会自动恢复——不需要额外的
/// 「已吸附」状态。
pub fn attached_side(main: RectF, float: RectF) -> Option<Side> {
    if !main.is_finite() || !float.is_finite() {
        return None;
    }
    if main.y.max(float.y) >= main.bottom().min(float.bottom()) {
        return None;
    }
    let close = |gap: f32| (-2.0..=SNAP_DISTANCE).contains(&gap);
    let right_gap = float.x - main.right();
    let left_gap = main.x - float.right();
    if close(right_gap) {
        Some(Side::Right)
    } else if close(left_gap) {
        Some(Side::Left)
    } else {
        None
    }
}

/// 浮窗与主窗口之间的吸附锚点：贴哪边、水平间距、竖直偏移。
///
/// 跟随主窗口时一直沿用同一份锚点（而不是每次从几何里重算），所以浮窗被
/// 工作区边缘夹住之后，主窗口移回来时浮窗还能回到原来的相对位置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    /// 贴主窗口的哪一侧。
    pub side: Side,
    /// 与主窗口之间的水平间距（吸附后 0，初始摆放 12）。
    pub gap: f32,
    /// 与主窗口顶边的竖直偏移。
    pub y_offset: f32,
}

impl Anchor {
    /// 从当前两窗口的几何里读出锚点；没贴着主窗口时为 `None`。
    pub fn from_geometry(main: RectF, float: RectF) -> Option<Anchor> {
        let side = attached_side(main, float)?;
        let gap = match side {
            Side::Right => float.x - main.right(),
            Side::Left => main.x - float.right(),
        };
        Some(Anchor {
            side,
            gap,
            y_offset: float.y - main.y,
        })
    }

    /// 主窗口变成 `main` 之后，浮窗左上角应该在哪（夹进工作区）。
    pub fn place(self, main: RectF, size: (f32, f32), work: RectF) -> (f32, f32) {
        let (width, height) = size;
        let x = match self.side {
            Side::Right => main.right() + self.gap,
            Side::Left => main.x - width - self.gap,
        };
        clamp_inside(RectF::new(x, main.y + self.y_offset, width, height), work)
    }
}

/// 把矩形夹进工作区；工作区比矩形还小时以工作区左上角为准。
fn clamp_inside(rect: RectF, work: RectF) -> (f32, f32) {
    let x = clamp_axis(rect.x, work.x, work.right() - rect.width, rect.x);
    let y = clamp_axis(rect.y, work.y, work.bottom() - rect.height, rect.y);
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

    // ---------- 主窗口几何恢复 ----------

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
        // 窗口存的是已拔掉的右屏坐标，只剩 120px 和主屏相交。
        let saved = RectF::new(1800.0, 100.0, 1000.0, 700.0);
        assert_eq!(best_work_area(saved, &[WORK]), Some(WORK));

        // 还在屏幕外就居中，而不是继续停在看不见的地方。
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
        // 740/1000 还在屏内：位置意图保留，右侧越界部分夹回工作区。
        let saved = RectF::new(1400.0, 100.0, 1000.0, 700.0);
        let restored = restore_window_bounds(saved, WORK, MIN_WINDOW);
        assert_eq!(restored.x, 1920.0 - 1000.0);
        assert_eq!(restored.y, 100.0);
        assert_eq!((restored.width, restored.height), (1000.0, 700.0));
    }

    #[test]
    fn half_offscreen_window_is_centered() {
        // 只剩 22% 在屏内：当作“显示器配置变了”，居中放回。
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

    fn float_at(x: f32, y: f32) -> RectF {
        RectF::new(x, y, SIZE.0, 400.0)
    }

    #[test]
    fn floats_flush_with_the_main_window_follow_it() {
        let main = main_window();
        // 贴右侧（拖动吸附后 0 间距）。
        assert_eq!(
            attached_side(main, float_at(900.0, 100.0)),
            Some(Side::Right)
        );
        // 初始摆放留的 12px 空隙也算贴着。
        assert_eq!(
            attached_side(main, float_at(912.0, 150.0)),
            Some(Side::Right)
        );
        // 贴左侧。
        assert_eq!(
            attached_side(main, float_at(-280.0, 100.0)),
            Some(Side::Left)
        );
    }

    #[test]
    fn floats_away_from_the_main_window_do_not_follow_it() {
        let main = main_window();
        // 水平方向已经被拖远。
        assert_eq!(attached_side(main, float_at(1400.0, 100.0)), None);
        // 竖直方向和主窗口没有交集。
        assert_eq!(attached_side(main, float_at(900.0, 800.0)), None);
        // 盖在主窗口上面（两侧都是负间距）也不算贴着。
        assert_eq!(attached_side(main, float_at(300.0, 200.0)), None);
        // 退化几何不跟随。
        assert_eq!(
            attached_side(main, RectF::new(f32::NAN, 0.0, 0.0, 0.0)),
            None
        );
    }

    #[test]
    fn following_keeps_the_offset_between_the_windows() {
        let main = main_window();
        let anchor = Anchor::from_geometry(main, float_at(900.0, 100.0)).unwrap();
        assert_eq!(
            anchor,
            Anchor {
                side: Side::Right,
                gap: 0.0,
                y_offset: 0.0
            }
        );

        // 主窗口往右下移 50/30，浮窗跟着走、间距不变。
        let moved = RectF::new(150.0, 130.0, 800.0, 600.0);
        assert_eq!(anchor.place(moved, SIZE, WORK), (950.0, 130.0));

        // 主窗口只变宽（不移动），浮窗仍然贴着新的右边缘。
        let wider = RectF::new(100.0, 100.0, 900.0, 600.0);
        assert_eq!(anchor.place(wider, SIZE, WORK), (1000.0, 100.0));

        // 12px 空隙的初始摆放保留空隙，上下错开的偏移也保留。
        let spaced = Anchor::from_geometry(main, float_at(912.0, 160.0)).unwrap();
        assert_eq!(spaced.gap, 12.0);
        assert_eq!(spaced.y_offset, 60.0);
        let moved_down = RectF::new(200.0, 100.0, 800.0, 600.0);
        assert_eq!(spaced.place(moved_down, SIZE, WORK), (1012.0, 160.0));
    }

    #[test]
    fn following_a_left_hand_float_mirrors_the_geometry() {
        let previous = main_window();
        let float = float_at(-280.0, 100.0);
        let anchor = Anchor::from_geometry(previous, float).unwrap();
        assert_eq!(anchor.side, Side::Left);

        let main = RectF::new(150.0, 180.0, 800.0, 600.0);
        // 左侧副屏的工作区是负坐标的，浮窗也应该跟着过去。
        let wide_work = RectF::new(-1920.0, 0.0, 3840.0, 1080.0);
        assert_eq!(anchor.place(main, SIZE, wide_work), (-230.0, 180.0));
    }

    #[test]
    fn following_stops_at_the_work_area_edges() {
        let anchor = Anchor::from_geometry(main_window(), float_at(900.0, 100.0)).unwrap();

        // 主窗口拖到屏幕外时，浮窗被夹在工作区里。
        let off_screen = RectF::new(5000.0, 100.0, 800.0, 600.0);
        assert_eq!(
            anchor.place(off_screen, SIZE, WORK),
            (1920.0 - SIZE.0, 100.0)
        );

        // 夹住之后锚点没有变，主窗口拖回来时浮窗回到原来的相对位置。
        let back = RectF::new(100.0, 100.0, 800.0, 600.0);
        assert_eq!(anchor.place(back, SIZE, WORK), (900.0, 100.0));
    }

    #[test]
    fn a_float_where_the_user_left_it_has_no_anchor() {
        // 用户把浮窗拖到屏幕另一头，就不再跟随主窗口。
        assert!(Anchor::from_geometry(main_window(), float_at(1400.0, 100.0)).is_none());
        // 拖回主窗口边上又恢复吸附。
        assert_eq!(
            Anchor::from_geometry(main_window(), float_at(900.0, 100.0)).map(|anchor| anchor.side),
            Some(Side::Right)
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

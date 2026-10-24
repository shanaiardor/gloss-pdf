//! 浮动胶囊（顶部按钮条 + 底部按钮条）的自动隐身判定。
//!
//! 需求：鼠标离开胶囊一段时间后让它隐身，鼠标回到那块位置、或阅读参数（缩放、
//! 翻页、适应模式、面板开关……）变化时立刻现身。
//!
//! 本模块刻意**不依赖 GPUI**：这里只有坐标、几何带和一个计时状态机，因此可以在
//! 没有窗口的单测里把「进带 / 离开 / 参数变更 / 定时器到期 / 过期定时器」全部
//! 走一遍。GPUI 那边只负责三件事：把鼠标坐标喂进来、按 `visible` 决定画不画、
//! 按 `countdown` 排一个定时器再回来报到期。

use std::time::Duration;

use crate::layout::FitMode;

/// 鼠标离开感应带后，胶囊继续显示的时长。
///
/// 想更「粘手」就往大调（比如 1800ms），想要更利落的隐身就往小调（1200ms）。
pub const HIDE_DELAY: Duration = Duration::from_millis(1500);

/// 顶部感应带高度（逻辑像素）。
///
/// 顶部胶囊是 `top: 8px` + 高 38px，占到 46px；上面还压着 46px 的拖拽条，
/// 所以往大取一点，从页面往上推鼠标时一定先碰到带子。
pub const TOP_BAND_HEIGHT: f32 = 56.0;

/// 底部感应带高度（逻辑像素）。
///
/// 底部胶囊是 `bottom: 16px` + 高 38px，从窗口底边算起占 54px。
pub const BOTTOM_BAND_HEIGHT: f32 = 64.0;

/// 顶部/底部感应带的高度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bands {
    pub top: f32,
    pub bottom: f32,
}

impl Default for Bands {
    fn default() -> Self {
        Self::STANDARD
    }
}

impl Bands {
    /// 默认感应带：贴着胶囊的实际占位，略放大一圈。
    pub const STANDARD: Self = Self {
        top: TOP_BAND_HEIGHT,
        bottom: BOTTOM_BAND_HEIGHT,
    };

    #[allow(dead_code)] // 单元测试构造自定义感应带；实际代码只用 STANDARD。
    pub const fn new(top: f32, bottom: f32) -> Self {
        Self { top, bottom }
    }
}

/// 鼠标落在哪条感应带上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Top,
    Bottom,
}

/// 鼠标是否在感应带上；`viewport` 是窗口内容区尺寸（逻辑像素）。
///
/// 判定全部按「一半开一半闭」处理，避免两条带子的边界重复计数：
/// 顶部带是 `0 ..= top`（含 0 和带子下沿），底部带是 `bottom_line ..= height`。
/// 坐标或尺寸是 NaN/无穷时一律当作「不在带内」——宁可让胶囊多显示一秒，
/// 也不要因为一个脏坐标把它藏起来。
pub fn band_at(mouse: (f32, f32), viewport: (f32, f32), bands: Bands) -> Option<Band> {
    let (x, y) = mouse;
    let (width, height) = viewport;
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return None;
    }
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    // 窗口被拖得比两条带子加起来还矮时，各让一半，别让整屏都算带子。
    let half = height / 2.0;
    let top = bands.top.clamp(0.0, half);
    let bottom = bands.bottom.clamp(0.0, half);
    if x < 0.0 || x > width || y < 0.0 || y > height {
        return None;
    }
    if y <= top {
        return Some(Band::Top);
    }
    if y >= height - bottom {
        return Some(Band::Bottom);
    }
    None
}

/// 参数摘要：只有这些量变化才值得让胶囊现身。
///
/// 只放「用户一眼能看出变了」的量。新增这类状态（比如新的面板开关）时要同步
/// 加进来，否则它变化时胶囊不会自动显示。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeSignature {
    pub zoom: f32,
    pub current_page: usize,
    pub page_count: usize,
    pub fit_mode: FitMode,
    pub outline_open: bool,
    pub highlight_panel_open: bool,
    pub settings_open: bool,
    pub page_input_open: bool,
    pub search_open: bool,
    pub highlight_count: usize,
    pub dark: bool,
}

/// 参数摘要是否变了。
///
/// 直接比结构体即可：所有字段都参与了「胶囊内容/状态变化」的判断。
/// `zoom` 用 `f32` 比较，NaN 会一直判为「变了」，正好退化成「永远显示」，安全。
pub fn is_parameter_change(previous: &ChromeSignature, current: &ChromeSignature) -> bool {
    previous != current
}

/// 可见性判定：处于强制显示状态，或隐藏倒计时还没跑完，就该显示。
pub fn should_stay_visible(forced: bool, countdown_finished: bool) -> bool {
    forced || !countdown_finished
}

/// 一次判定需要的输入。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromeInput {
    /// 鼠标是否落在顶部/底部感应带里（胶囊本身也在带内）。
    pub mouse_in_band: bool,
    /// 参数摘要刚变过（缩放、翻页、适应模式、面板开关……）。
    pub parameters_changed: bool,
    /// 有正在进行的交互：页码输入框打开、中键平移、拖选文字等。
    pub interacting: bool,
    /// 这次输入来自真实的鼠标移动（每帧 `render` 里的兜底结算为 `false`）。
    pub pointer_moved: bool,
}

/// 判定结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromeUpdate {
    /// 这一帧胶囊是否可见。
    pub visible: bool,
    /// `Some(generation)` 表示需要（重新）启动隐藏倒计时，把代数令牌交给定时器。
    pub countdown: Option<u64>,
    /// 可见性相对上一次变了，调用方需要请求重绘。
    pub changed: bool,
}

/// 胶囊可见性状态机。
///
/// 三个字段分工：`forced` 记录上一次是否处于「强制显示」状态（鼠标在带内/正在
/// 交互/参数刚变），`armed` 记录是否有一个带当前代数的倒计时在跑，
/// `countdown_finished` 记录那个倒计时是否已经到期。
pub struct ChromeVisibility {
    bands: Bands,
    signature: Option<ChromeSignature>,
    visible: bool,
    forced: bool,
    armed: bool,
    countdown_finished: bool,
    /// 是否已经见过一次鼠标移动：应用刚起来时先亮着，别一开屏就藏。
    pointer_seen: bool,
    /// 代数计数器：每次重新计时 +1，过期的定时器靠它被丢掉。
    generation: u64,
}

impl Default for ChromeVisibility {
    fn default() -> Self {
        Self::new()
    }
}

impl ChromeVisibility {
    pub fn new() -> Self {
        Self::with_bands(Bands::STANDARD)
    }

    pub fn with_bands(bands: Bands) -> Self {
        Self {
            bands,
            signature: None,
            // 阅读器刚打开时先亮着，让人看见胶囊在哪。
            visible: true,
            forced: false,
            armed: false,
            countdown_finished: false,
            pointer_seen: false,
            generation: 1,
        }
    }

    pub fn bands(&self) -> Bands {
        self.bands
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 采样参数摘要，返回是否与上一次不同。
    ///
    /// 首次采样只记录下来，不算变化：刚打开文档不该被当成「参数变更」。
    pub fn observe_parameters(&mut self, signature: ChromeSignature) -> bool {
        match self.signature {
            None => {
                self.signature = Some(signature);
                false
            }
            Some(previous) if is_parameter_change(&previous, &signature) => {
                self.signature = Some(signature);
                true
            }
            Some(_) => false,
        }
    }

    /// 按本帧输入结算可见性。
    ///
    /// 幂等：同一帧里被调多次（鼠标回调 + 每帧 `render`）不会重复排定时器。
    pub fn update(&mut self, input: ChromeInput) -> ChromeUpdate {
        let was_visible = self.visible;
        self.pointer_seen |= input.pointer_moved;
        let forced = input.mouse_in_band || input.interacting || input.parameters_changed;
        self.forced = forced;

        if forced {
            self.countdown_finished = false;
            self.visible = true;
            // 人手就在胶囊上/正在交互：作废在跑的倒计时，别让它 1.5s 后把胶囊藏掉。
            if self.armed {
                self.armed = false;
                self.generation = self.generation.wrapping_add(1);
            }
            // 「参数变了但鼠标不在带内」这一种要当场起身计时；鼠标在带内时不计时，
            // 等人走了再由下面的 `!armed` 分支接手。
            if !input.mouse_in_band && !input.interacting {
                return self.arm_countdown(was_visible);
            }
            return ChromeUpdate {
                visible: true,
                countdown: None,
                changed: !was_visible,
            };
        }

        // 已经隐身了：不动，等鼠标回到带内或参数变化。
        if !self.visible {
            return ChromeUpdate {
                visible: false,
                countdown: None,
                changed: false,
            };
        }

        if self.armed {
            if !self.countdown_finished {
                return ChromeUpdate {
                    visible: true,
                    countdown: None,
                    changed: false,
                };
            }
            self.armed = false;
            self.visible = should_stay_visible(false, self.countdown_finished);
            return ChromeUpdate {
                visible: false,
                countdown: None,
                changed: was_visible,
            };
        }

        // 还没见过鼠标移动（应用刚起来、人还没碰鼠标）：先亮着，让人看见胶囊在哪。
        if !self.pointer_seen {
            return ChromeUpdate {
                visible: self.visible,
                countdown: None,
                changed: false,
            };
        }

        // 显示中但没在计时（刚从带内或交互中出来）：现在开始计时。
        self.arm_countdown(was_visible)
    }

    /// 隐藏定时器到期。返回 `true` 表示「刚隐藏，调用方要重绘」。
    ///
    /// 代数不匹配说明期间又显示过，这是过期的定时器：直接丢掉，否则刚现身
    /// 的胶囊会被上一个计时器立刻藏回去。
    pub fn on_countdown_elapsed(&mut self, generation: u64) -> bool {
        if !self.armed || generation != self.generation {
            return false;
        }
        self.armed = false;
        self.countdown_finished = true;
        let was_visible = self.visible;
        // 能走到这里说明这一轮倒计时还有效（`forced` 的分支会作废代数），
        // 所以直接藏；不能再去看 `forced`——它是上一帧的快照，参数变更那一帧
        // 恰好排了计时，隔一个周期才发现 `forced` 已经变回 false。
        self.visible = false;
        was_visible != self.visible
    }

    /// 起身新一轮倒计时：显示着，并给出新的代数令牌。
    fn arm_countdown(&mut self, was_visible: bool) -> ChromeUpdate {
        self.armed = true;
        self.countdown_finished = false;
        self.generation = self.generation.wrapping_add(1);
        self.visible = true;
        ChromeUpdate {
            visible: true,
            countdown: Some(self.generation),
            changed: !was_visible,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Band, Bands, ChromeInput, ChromeSignature, ChromeVisibility, HIDE_DELAY, band_at,
        is_parameter_change, should_stay_visible,
    };
    use crate::layout::FitMode;

    const VIEWPORT: (f32, f32) = (1000.0, 800.0);

    fn signature() -> ChromeSignature {
        ChromeSignature {
            zoom: 1.0,
            current_page: 0,
            page_count: 10,
            fit_mode: FitMode::Free,
            outline_open: false,
            highlight_panel_open: false,
            settings_open: false,
            page_input_open: false,
            search_open: false,
            highlight_count: 0,
            dark: false,
        }
    }

    fn input(mouse_in_band: bool, parameters_changed: bool, interacting: bool) -> ChromeInput {
        ChromeInput {
            mouse_in_band,
            parameters_changed,
            interacting,
            pointer_moved: true,
        }
    }

    // ---------- 感应带 ----------

    #[test]
    fn band_hits_top_bottom_and_misses_the_middle() {
        let bands = Bands::STANDARD;
        assert_eq!(band_at((500.0, 0.0), VIEWPORT, bands), Some(Band::Top));
        assert_eq!(band_at((500.0, 20.0), VIEWPORT, bands), Some(Band::Top));
        assert_eq!(band_at((500.0, 400.0), VIEWPORT, bands), None);
        assert_eq!(band_at((500.0, 800.0), VIEWPORT, bands), Some(Band::Bottom));
        assert_eq!(band_at((500.0, 780.0), VIEWPORT, bands), Some(Band::Bottom));
    }

    #[test]
    fn band_edges_are_inclusive_once() {
        let bands = Bands::STANDARD;
        // 顶部带下沿还算顶部，再多一个像素就掉到中间了。
        assert_eq!(
            band_at((10.0, super::TOP_BAND_HEIGHT), VIEWPORT, bands),
            Some(Band::Top)
        );
        assert_eq!(
            band_at((10.0, super::TOP_BAND_HEIGHT + 0.5), VIEWPORT, bands),
            None
        );
        // 底部带上沿同理。
        let bottom_line = VIEWPORT.1 - super::BOTTOM_BAND_HEIGHT;
        assert_eq!(
            band_at((10.0, bottom_line), VIEWPORT, bands),
            Some(Band::Bottom)
        );
        assert_eq!(band_at((10.0, bottom_line - 0.5), VIEWPORT, bands), None);
        // 窗口外侧不算带内。
        assert_eq!(band_at((-1.0, 10.0), VIEWPORT, bands), None);
        assert_eq!(band_at((1001.0, 10.0), VIEWPORT, bands), None);
        assert_eq!(band_at((10.0, -1.0), VIEWPORT, bands), None);
    }

    #[test]
    fn band_rejects_nan_and_degenerate_viewports() {
        let bands = Bands::STANDARD;
        assert_eq!(band_at((f32::NAN, 10.0), VIEWPORT, bands), None);
        assert_eq!(band_at((10.0, f32::NAN), VIEWPORT, bands), None);
        assert_eq!(band_at((10.0, f32::INFINITY), VIEWPORT, bands), None);
        assert_eq!(band_at((10.0, 10.0), (f32::NAN, 800.0), bands), None);
        assert_eq!(band_at((10.0, 10.0), (0.0, 800.0), bands), None);
        assert_eq!(band_at((10.0, 10.0), (1000.0, -1.0), bands), None);
        // 窗口比带子还矮时各让一半，中间仍然是「不在带内」。
        let tiny = (400.0, 40.0);
        assert_eq!(band_at((10.0, 10.0), tiny, bands), Some(Band::Top));
        assert_eq!(band_at((10.0, 20.0), tiny, bands), Some(Band::Top));
        assert_eq!(band_at((10.0, 30.0), tiny, bands), Some(Band::Bottom));
    }

    #[test]
    fn custom_bands_are_respected() {
        let bands = Bands::new(10.0, 20.0);
        assert_eq!(band_at((0.0, 10.0), VIEWPORT, bands), Some(Band::Top));
        assert_eq!(band_at((0.0, 11.0), VIEWPORT, bands), None);
        assert_eq!(band_at((0.0, 790.0), VIEWPORT, bands), Some(Band::Bottom));
        assert_eq!(band_at((0.0, 779.0), VIEWPORT, bands), None);
    }

    // ---------- 参数签名 ----------

    #[test]
    fn signature_detects_parameter_changes() {
        let base = signature();
        assert!(!is_parameter_change(&base, &signature()));

        let mut zoomed = signature();
        zoomed.zoom = 1.25;
        assert!(is_parameter_change(&base, &zoomed));

        let mut turned = signature();
        turned.current_page = 3;
        assert!(is_parameter_change(&base, &turned));

        let mut fitted = signature();
        fitted.fit_mode = FitMode::Width;
        assert!(is_parameter_change(&base, &fitted));

        let mut panel = signature();
        panel.settings_open = true;
        assert!(is_parameter_change(&base, &panel));

        let mut page_input = signature();
        page_input.page_input_open = true;
        assert!(is_parameter_change(&base, &page_input));

        let mut highlighted = signature();
        highlighted.highlight_count = 2;
        assert!(is_parameter_change(&base, &highlighted));

        let mut dark = signature();
        dark.dark = true;
        assert!(is_parameter_change(&base, &dark));
    }

    // ---------- 可见性 ----------

    #[test]
    fn should_stay_visible_truth_table() {
        assert!(should_stay_visible(true, true));
        assert!(should_stay_visible(true, false));
        assert!(should_stay_visible(false, false));
        assert!(!should_stay_visible(false, true));
    }

    #[test]
    fn starts_visible_and_arms_the_countdown_once_mouse_leaves() {
        let mut chrome = ChromeVisibility::new();
        assert!(chrome.is_visible());

        // 鼠标在带内：显示着，不排定时器。
        let update = chrome.update(input(true, false, false));
        assert!(update.visible);
        assert_eq!(update.countdown, None);

        // 鼠标离开：仍然显示，但开始计时。
        let update = chrome.update(input(false, false, false));
        assert!(update.visible);
        assert!(!update.changed);
        let generation = update.countdown.expect("离开感应带要开始计时");

        // 倒计时期间重复判定不会重复排定时器（幂等）。
        assert_eq!(chrome.update(input(false, false, false)).countdown, None);

        // 定时器到期：隐藏，并请求重绘。
        assert!(chrome.on_countdown_elapsed(generation));
        assert!(!chrome.is_visible());
        // 到期后再判一次仍然是隐藏，且不需要再重绘。
        let update = chrome.update(input(false, false, false));
        assert!(!update.visible);
        assert!(!chrome.on_countdown_elapsed(generation));
    }

    #[test]
    fn capsules_stay_until_the_mouse_has_moved_once() {
        // 应用刚起来、人还没碰鼠标：每帧 `render` 的兜底结算不该把胶囊藏掉。
        let mut chrome = ChromeVisibility::new();
        let render_only = ChromeInput {
            pointer_moved: false,
            ..input(false, false, false)
        };
        for _ in 0..10 {
            let update = chrome.update(render_only);
            assert!(update.visible);
            assert_eq!(update.countdown, None);
            assert!(!update.changed);
        }

        // 鼠标一动（哪怕不在带内）才开始计时。
        let generation = chrome
            .update(input(false, false, false))
            .countdown
            .expect("见过鼠标移动后离开感应带要开始计时");
        assert!(chrome.on_countdown_elapsed(generation));
        assert!(!chrome.is_visible());
    }

    #[test]
    fn parameter_change_before_the_first_mouse_move_still_shows_and_times_out() {
        // 鼠标没动但参数变了（例如键盘翻页）：照常亮出来，并且当场起身计时。
        let mut chrome = ChromeVisibility::new();
        let render_only = ChromeInput {
            pointer_moved: false,
            ..input(false, false, false)
        };
        chrome.update(render_only);
        let generation = chrome
            .update(ChromeInput {
                parameters_changed: true,
                ..render_only
            })
            .countdown
            .expect("参数变更后鼠标不在带内要开始计时");
        assert!(chrome.is_visible());
        assert!(chrome.on_countdown_elapsed(generation));
        assert!(!chrome.is_visible());
    }

    #[test]
    fn returning_to_the_band_shows_immediately_and_cancels_the_timer() {
        let mut chrome = ChromeVisibility::new();
        let generation = chrome.update(input(false, false, false)).countdown.unwrap();

        // 回到感应带：立刻显示（可见性变了要重绘），计时作废。
        let update = chrome.update(input(true, false, false));
        assert!(update.visible);
        assert_eq!(update.countdown, None);
        assert!(!update.changed, "本来就显示着，不需要重绘");

        // 过期定时器被丢掉，胶囊不会被藏。
        assert!(!chrome.on_countdown_elapsed(generation));
        assert!(chrome.is_visible());

        // 再离开会拿到新的代数，旧代数永远无效。
        let next = chrome.update(input(false, false, false)).countdown.unwrap();
        assert_ne!(next, generation);
        assert!(!chrome.on_countdown_elapsed(generation));
        assert!(chrome.on_countdown_elapsed(next));
    }

    #[test]
    fn hidden_capsule_shows_when_mouse_returns() {
        let mut chrome = ChromeVisibility::new();
        let generation = chrome.update(input(false, false, false)).countdown.unwrap();
        assert!(chrome.on_countdown_elapsed(generation));
        assert!(!chrome.is_visible());

        let update = chrome.update(input(true, false, false));
        assert!(update.visible);
        assert!(update.changed, "从隐藏变可见要请求重绘");
        assert_eq!(update.countdown, None);
    }

    #[test]
    fn parameter_change_shows_and_restarts_the_countdown() {
        let mut chrome = ChromeVisibility::new();
        let generation = chrome.update(input(false, false, false)).countdown.unwrap();
        assert!(chrome.on_countdown_elapsed(generation));
        assert!(!chrome.is_visible());

        // 鼠标不在带内，但参数变了：显示 + 当场重新计时。
        let update = chrome.update(input(false, true, false));
        assert!(update.visible);
        assert!(update.changed);
        let fresh = update.countdown.expect("参数变更后要重新计时");
        assert_ne!(fresh, generation);
        assert!(!chrome.on_countdown_elapsed(generation));
        assert!(chrome.is_visible());
    }

    #[test]
    fn signature_sampling_ignores_the_first_sample() {
        let mut chrome = ChromeVisibility::new();
        assert!(!chrome.observe_parameters(signature()));
        assert!(!chrome.observe_parameters(signature()));

        let mut zoomed = signature();
        zoomed.zoom = 2.0;
        assert!(chrome.observe_parameters(zoomed));
        assert!(!chrome.observe_parameters(zoomed));
    }

    #[test]
    fn interaction_blocks_hiding() {
        let mut chrome = ChromeVisibility::new();
        let generation = chrome.update(input(false, false, false)).countdown.unwrap();

        // 页码输入框打开（或正在平移/拖选）：计时作废，继续显示。
        let update = chrome.update(input(false, false, true));
        assert!(update.visible);
        assert_eq!(update.countdown, None);
        assert!(!chrome.on_countdown_elapsed(generation));
        assert!(chrome.is_visible());

        // 输入框收起、鼠标也不在带内：重新计时，到期才藏。
        let next = chrome.update(input(false, false, false)).countdown.unwrap();
        assert!(chrome.on_countdown_elapsed(next));
        assert!(!chrome.is_visible());
    }

    #[test]
    fn hide_delay_is_in_the_documented_range() {
        // 1.2–1.8s 之间都能接受，常量本身留在这里提醒「可调」。
        assert!(HIDE_DELAY >= std::time::Duration::from_millis(1200));
        assert!(HIDE_DELAY <= std::time::Duration::from_millis(1800));
    }
}

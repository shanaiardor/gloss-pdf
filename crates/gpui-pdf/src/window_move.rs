//! 移动无边框浮窗。
//!
//! GPUI 没有公开的“设置窗口位置”API：Windows 下只能取到 HWND 后自己调
//! `SetWindowPos`。其他平台沿用系统原生拖拽，不编译这个模块。
//!
//! # 为什么不开在本线程直接调
//!
//! `SetWindowPos` 对同一线程的窗口是同步投递：`WM_WINDOWPOSCHANGING`、
//! `WM_WINDOWPOSCHANGED`、`WM_MOVE` 都在它返回之前跑完 wndproc。GPUI 的
//! `WM_MOVE` 处理是
//! `handle.update(&mut async_cx, |_, window, cx| window.bounds_changed(cx))`，
//! 也就是重新借一次 `App`（失败时打印 `[ERROR gpui::window] RefCell already
//! borrowed`）。而调用者的位置几乎总在借里：主窗口拖动时我们是在
//! `observe_window_bounds` 的回调里，浮窗自己拖动时是在 `on_drag_move` 里，
//! 两者都持有 `&mut App`。回调里 `cx.defer` / `cx.spawn` 也救不了：延迟效果和
//! 前台任务都是拿着 `&mut App` 再执行闭包的，同线程的 `WM_MOVE` 依旧嵌在借用
//! 里面。
//!
//! 所以这里把 `SetWindowPos` 交给一个不拥有窗口的线程：跨线程投递的
//! `WM_MOVE` 只能等窗口线程自己取消息时才处理，那时借用早就释放了。
//! 队列只保留最后一次请求，拖动时既不会积压，也不会看到过时的位置。

use std::sync::{Condvar, Mutex, OnceLock};

use gpui::{Pixels, Point, Window};

/// 一次待执行的移动（设备像素坐标，已按窗口缩放换算）。
#[derive(Clone, Copy)]
struct MoveRequest {
    hwnd: isize,
    x: i32,
    y: i32,
}

/// 最近的移动请求：后来的覆盖先来的。
static PENDING: Mutex<Option<MoveRequest>> = Mutex::new(None);
/// `PENDING` 有值了就唤醒线程。
static READY: Condvar = Condvar::new();
/// 线程只需要起一次。
static WORKER: OnceLock<()> = OnceLock::new();

/// 把窗口左上角移动到全局坐标 `origin`（逻辑像素）。
///
/// 只入队、不等结果：调用者通常正处在窗口回调里，不能在这里碰 `SetWindowPos`。
pub(crate) fn move_window(window: &Window, origin: Point<Pixels>) {
    use raw_window_handle::RawWindowHandle;

    // `Window` 自己也定义了同名的 `window_handle`（返回 GPUI 句柄），
    // 这里必须显式走 raw-window-handle 的 trait。
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };

    let scale = window.scale_factor();
    let request = MoveRequest {
        hwnd: handle.hwnd.get(),
        x: (f32::from(origin.x) * scale).round() as i32,
        y: (f32::from(origin.y) * scale).round() as i32,
    };

    ensure_worker();

    let mut pending = PENDING.lock().unwrap_or_else(|error| error.into_inner());
    *pending = Some(request);
    drop(pending);
    READY.notify_one();
}

fn ensure_worker() {
    WORKER.get_or_init(|| {
        let spawned = std::thread::Builder::new()
            .name("dictionary-window-move".to_string())
            .spawn(worker_loop);
        if let Err(error) = spawned {
            log::warn!("无法创建移动词典窗口的线程: {error}");
        }
    });
}

fn worker_loop() {
    loop {
        let request = {
            let mut pending = PENDING.lock().unwrap_or_else(|error| error.into_inner());
            loop {
                if let Some(request) = pending.take() {
                    break request;
                }
                pending = READY
                    .wait(pending)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };
        apply(request);
    }
}

fn apply(request: MoveRequest) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    let hwnd = HWND(request.hwnd as *mut _);
    // 窗口可能已经被关掉了，这时 `SetWindowPos` 只会报错，不必打扰用户。
    if let Err(error) = unsafe {
        SetWindowPos(
            hwnd,
            None,
            request.x,
            request.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    } {
        log::warn!("移动词典窗口失败: {error}");
    }
}

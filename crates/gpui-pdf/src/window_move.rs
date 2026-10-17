//! 移动无边框浮窗。
//!
//! GPUI 没有公开的“设置窗口位置”API：Windows 下只能取到 HWND 后自己调
//! `SetWindowPos`。其他平台沿用系统原生拖拽，不编译这个模块。

use gpui::{Pixels, Point, Window};

/// 把窗口左上角移动到全局坐标 `origin`（逻辑像素）。
pub(crate) fn move_window(window: &mut Window, origin: Point<Pixels>) {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    // `Window` 自己也定义了同名的 `window_handle`（返回 GPUI 句柄），
    // 这里必须显式走 raw-window-handle 的 trait。
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };

    let scale = window.scale_factor();
    let x = (f32::from(origin.x) * scale).round() as i32;
    let y = (f32::from(origin.y) * scale).round() as i32;
    let hwnd = HWND(handle.hwnd.get() as *mut _);

    // SAFETY: HWND 来自当前窗口；只改位置，不改大小、Z 序，也不激活窗口。
    let result = unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    if let Err(error) = result {
        log::warn!("移动词典窗口失败: {error}");
    }
}

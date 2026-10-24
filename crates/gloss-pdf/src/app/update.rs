//! 更新状态：把 `update` 引擎的事件翻译成设置面板直接能画的状态。
//!
//! 网络与下载都在 worker 线程里跑，这里只维护状态机；真正的安装动作是启动
//! 安装器后退出应用，交给 Inno Setup 覆盖文件。

use std::path::PathBuf;

use gpui::Context;

use super::AppState;
use crate::update::{self, Command, Event, UpdateInfo};

#[derive(Clone, Debug, Default)]
pub enum UpdatePhase {
    #[default]
    Idle,
    Checking,
    UpToDate {
        version: String,
    },
    Available(UpdateInfo),
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    Downloaded(PathBuf),
    Failed(String),
}

impl UpdatePhase {
    /// 检查或下载进行中；这时不再重复发命令。
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            UpdatePhase::Checking | UpdatePhase::Downloading { .. }
        )
    }

    /// 进度百分比；不知道总大小时返回 `None`。
    pub fn percent(&self) -> Option<u8> {
        let UpdatePhase::Downloading { downloaded, total } = self else {
            return None;
        };
        let total = (*total).filter(|total| *total > 0)?;
        Some(((*downloaded).min(total) * 100 / total) as u8)
    }
}

impl AppState {
    pub(crate) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.update_phase.is_busy() {
            return;
        }
        log::info!("checking for updates");
        self.update_phase = UpdatePhase::Checking;
        if self.update_engine.commands.send(Command::Check).is_err() {
            self.update_phase = UpdatePhase::Failed("更新线程不可用".to_string());
        }
        cx.notify();
    }

    pub(crate) fn download_update(&mut self, cx: &mut Context<Self>) {
        let UpdatePhase::Available(info) = self.update_phase.clone() else {
            return;
        };
        self.update_phase = UpdatePhase::Downloading {
            downloaded: 0,
            total: None,
        };
        if self
            .update_engine
            .commands
            .send(Command::Download { info })
            .is_err()
        {
            self.update_phase = UpdatePhase::Failed("更新线程不可用".to_string());
        }
        cx.notify();
    }

    pub(crate) fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdatePhase::Downloaded(path) = self.update_phase.clone() else {
            return;
        };
        match update::launch_installer(&path) {
            // 安装器要覆盖正在运行的 exe，必须先退出；延后到当前事件处理结束再退，
            // 避免在按钮回调的借用期间直接 quit 触发 GPUI 的 fail-fast。
            Ok(()) => {
                self.flush_progress();
                self.settings.save();
                cx.defer(|cx| cx.quit());
            }
            Err(error) => {
                self.update_phase = UpdatePhase::Failed(format!("{error:#}"));
                cx.notify();
            }
        }
    }

    pub(crate) fn handle_update_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Checking => self.update_phase = UpdatePhase::Checking,
            Event::UpToDate { version } => {
                log::info!("already up to date ({version})");
                self.update_phase = UpdatePhase::UpToDate { version };
            }
            Event::Available(info) => {
                log::info!("update available: {}", info.version);
                self.set_status(format!("发现新版本 {}", info.version), cx);
                self.update_phase = UpdatePhase::Available(info);
            }
            // 第一个进度事件随后就到，这里不用改状态。
            Event::Downloading => {}
            Event::Progress { downloaded, total } => {
                if matches!(self.update_phase, UpdatePhase::Downloading { .. }) {
                    self.update_phase = UpdatePhase::Downloading { downloaded, total };
                }
            }
            Event::Downloaded(path) => {
                self.update_phase = UpdatePhase::Downloaded(path);
                self.set_status("更新已下载，重启即可安装", cx);
            }
            Event::Failed(message) => {
                self.update_phase = UpdatePhase::Failed(message);
            }
        }
        cx.notify();
    }

    /// 启动时自动检查更新的开关。
    pub(crate) fn toggle_auto_update_check(&mut self, cx: &mut Context<Self>) {
        self.settings.current.auto_check_updates = !self.settings.current.auto_check_updates;
        self.settings.save();
        cx.notify();
    }
}

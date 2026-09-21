//! A one-time guide for explicit manual launches. Its marker is separate from
//! user settings, so showing help never rewrites a protected configuration.

use std::{fs::OpenOptions, io, path::Path};

use windows::{
    Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{IDOK, MB_ICONINFORMATION, MB_OK, MessageBoxW},
    },
    core::{PCWSTR, w},
};

use super::config_path::default_config_path;
use crate::app::diagnostics;

pub(super) fn show_once(owner: HWND, shortcut: &str, registered: bool) {
    let marker = match default_config_path() {
        Ok(path) => path.with_file_name("welcome-v1.seen"),
        Err(error) => {
            diagnostics::event(format_args!("welcome.path_failed error={error}"));
            return;
        }
    };
    let result = offer_once(&marker, || {
        let shortcut = if registered {
            format!("按 {shortcut} 开始取色，或点击系统托盘中的滴管图标。")
        } else {
            format!("{shortcut} 当前注册失败。请点击托盘图标取色，或右键打开设置更换快捷键。")
        };
        let text = format!(
            "Color Picker 已在系统托盘运行。\n\n{shortcut}\n\n左键确认颜色；右键或 Esc 取消。\n向上滚动可冻结并放大，选取精确像素。\n取色后可复制 HEX、RGB、CSS RGB 或 HSL。\n\n右键托盘图标可打开设置或退出；图标也可能在托盘的隐藏区域中。\n此指引仅显示一次，登录 Windows 时启动不会弹出。"
        );
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let shown = unsafe {
            MessageBoxW(
                Some(owner),
                PCWSTR(text.as_ptr()),
                w!("欢迎使用 Color Picker"),
                MB_OK | MB_ICONINFORMATION,
            )
        } == IDOK;
        if shown {
            diagnostics::event(format_args!("welcome.acknowledged"));
        }
        shown
    });
    if let Err(error) = result {
        diagnostics::event(format_args!("welcome.marker_failed error={error}"));
    }
}

fn offer_once(marker: &Path, show: impl FnOnce() -> bool) -> io::Result<()> {
    if marker.try_exists()? || !show() {
        return Ok(());
    }
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Never truncate an existing marker, and only remember acknowledged help.
    match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_or_suppressed_guide_does_not_consume_the_first_manual_launch() {
        let directory = std::env::temp_dir().join(format!(
            "color-picker-welcome-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let marker = directory.join("welcome-v1.seen");
        offer_once(&marker, || false).unwrap();
        assert!(!marker.exists());
        offer_once(&marker, || true).unwrap();
        assert!(marker.is_file());
        offer_once(&marker, || {
            panic!("acknowledged help must not appear again")
        })
        .unwrap();
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}

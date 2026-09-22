//! The main thread owns both the persisted settings and the accepted hotkey.
//! A replacement key is not published until its config has been saved.
use windows::Win32::Foundation::HWND;

use super::{
    config_path,
    hotkey::{DEFAULT_HOTKEY_ID, HotkeyGuard},
};
use crate::app::{
    config::{Config, ConfigRevision, ConfigStore},
    diagnostics,
    i18n::{set_language, tr},
};

pub struct SettingsRuntime {
    pub config: Config,
    pub notice: Option<String>,
    pub save_allowed: bool,
    store: Option<ConfigStore>,
    revision: Option<ConfigRevision>,
    hotkey: Option<HotkeyGuard>,
    next_id: i32,
}

impl SettingsRuntime {
    pub fn load(hwnd: HWND) -> Self {
        let config_path = config_path::default_config_path();
        let (store, config, notice, save_allowed, revision) = match config_path {
            Ok(path) => {
                diagnostics::event(format_args!("config.path path={}", path.display()));
                let store = ConfigStore::new(path);
                let loaded = store.load();
                (
                    Some(store),
                    loaded.config,
                    loaded.warning,
                    loaded.save_allowed,
                    loaded.revision,
                )
            }
            Err(error) => (
                None,
                Config::default(),
                Some(crate::tr_format!(
                    "无法找到配置目录，当前使用默认设置且禁止保存。重启后重试：{error}",
                    "The settings folder could not be found. Defaults are in use and saving is disabled. Restart to retry: {error}"
                )),
                false,
                None,
            ),
        };
        set_language(config.language);
        let mut runtime = Self {
            config,
            notice,
            save_allowed,
            store,
            revision,
            hotkey: None,
            next_id: DEFAULT_HOTKEY_ID + 1,
        };
        match HotkeyGuard::register_config(hwnd, DEFAULT_HOTKEY_ID, &runtime.config.hotkey) {
            Ok(guard) => {
                diagnostics::event(format_args!(
                    "hotkey.registered chord={} id={} norepeat=true",
                    runtime.config.hotkey.label(),
                    guard.id()
                ));
                runtime.hotkey = Some(guard);
            }
            Err(error) => {
                diagnostics::event(format_args!(
                    "hotkey.registration_failed chord={} error={error}",
                    runtime.config.hotkey.label()
                ));
                let warning = crate::tr_format!(
                    "{} 注册失败，仍可从托盘取色；可在设置中换键或点击应用重试。{error}",
                    "{} could not be registered. You can still pick from the tray; change the shortcut in Settings or click Apply to retry. {error}",
                    runtime.config.hotkey.label()
                );
                runtime.notice = Some(match runtime.notice.take() {
                    Some(previous) => format!("{previous}\n{warning}"),
                    None => warning,
                });
            }
        }
        runtime
    }

    pub fn hotkey_id(&self) -> Option<i32> {
        self.hotkey.as_ref().map(HotkeyGuard::id)
    }

    /// Publish the new ID before dropping the returned retired registration.
    pub fn apply(&mut self, hwnd: HWND, config: Config) -> Result<Option<HotkeyGuard>, String> {
        config.validate().map_err(|error| error.to_string())?;
        if !self.save_allowed {
            return Err(self.notice.clone().unwrap_or_else(|| {
                tr("配置文件不可写。", "The settings file is not writable.").into()
            }));
        }
        let store = self.store.as_ref().ok_or(tr(
            "配置目录不可用。",
            "The settings folder is unavailable.",
        ))?;
        let expected = self.revision.as_ref().ok_or(tr(
            "配置版本不可用，请重新启动程序后重试。",
            "The loaded settings revision is unavailable. Restart the app and try again.",
        ))?;
        // The old key stays registered on both registration and save failures.
        // Each attempted ID is fresh; delayed WM_HOTKEY for a retired ID is ignored.
        let replacement = if config.hotkey != self.config.hotkey || self.hotkey.is_none() {
            let id = self.next_id;
            if id > 0xbfff {
                return Err(tr("本次运行已用尽快捷键标识，请重启程序后重试。", "Shortcut registrations are exhausted for this session. Restart the app and try again.").into());
            }
            self.next_id += 1;
            Some(
                HotkeyGuard::register_config(hwnd, id, &config.hotkey).map_err(|error| {
                    crate::tr_format!(
                        "快捷键 {} 不可用，原设置未更改：{error}",
                        "Shortcut {} is unavailable. Your previous settings are unchanged: {error}",
                        config.hotkey.label()
                    )
                })?,
            )
        } else {
            None
        };
        // On failure replacement drops here, unregistering only the temporary key.
        let revision = store
            .save_if_unchanged(&config, expected)
            .map_err(|error| {
                crate::tr_format!(
                    "保存失败，原设置未更改：{error}",
                    "Could not save. Your previous settings are unchanged: {error}"
                )
            })?;
        self.revision = Some(revision);
        self.config = config;
        set_language(self.config.language);
        let old_hotkey = replacement.and_then(|guard| self.hotkey.replace(guard));
        self.notice = None;
        diagnostics::event(format_args!(
            "config.applied hotkey={} default_format={:?} auto_copy={} border_width_dip={} background_transparency_percent={}",
            self.config.hotkey.label(),
            self.config.default_format,
            self.config.auto_copy_on_pick,
            self.config.appearance.border_width_dip,
            self.config.appearance.background_transparency_percent
        ));
        Ok(old_hotkey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::config::HotkeyConfig, core::format::ColorFormat};
    use std::{
        os::windows::fs::OpenOptionsExt,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
        },
        core::w,
    };

    struct TestWindow(HWND);
    impl TestWindow {
        fn new() -> Self {
            Self(unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("ColorPicker isolated settings transaction test"),
                    WINDOW_STYLE::default(),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    None,
                    None,
                )
                .expect("create the test's invisible message-only HWND")
            })
        }
    }
    impl Drop for TestWindow {
        fn drop(&mut self) {
            let _ = unsafe { DestroyWindow(self.0) };
        }
    }

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let tick = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "color-picker-settings-transaction-{}-{tick}",
                std::process::id(),
            ));
            // Refuse an existing path rather than adopt/delete anyone's files.
            std::fs::create_dir(&path).expect("create isolated test directory");
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn chord(key: &str) -> HotkeyConfig {
        HotkeyConfig {
            ctrl: true,
            alt: true,
            shift: true,
            key: key.into(),
        }
    }

    #[test]
    #[ignore = "temporarily registers Ctrl+Alt+Shift+F9/F10/F11; requires those chords to be unused"]
    fn hotkey_conflict_and_save_failure_preserve_the_old_transaction() {
        use crate::app::i18n::{Language, language};
        set_language(Language::SimplifiedChinese);
        let directory = TestDirectory::new();
        let owner = TestWindow::new();
        let probe = TestWindow::new();
        let path = directory.0.join("config.json");
        let store = ConfigStore::new(path.clone());
        let original = Config {
            language: Language::SimplifiedChinese,
            hotkey: chord("F9"),
            ..Config::default()
        };
        store.save(&original).unwrap();
        let original_bytes = std::fs::read(&path).unwrap();

        // Fail clearly if an external application owns any selected chord.
        // Never synthesize input, unregister someone else's key, or use the
        // user's actual configuration directory.
        let old = HotkeyGuard::register_config(owner.0, 101, &original.hotkey)
            .expect("Ctrl+Alt+Shift+F9 must be free before this test");
        let blocker = HotkeyGuard::register_config(probe.0, 201, &chord("F10"))
            .expect("Ctrl+Alt+Shift+F10 must be free before this test");
        let free_f11 = HotkeyGuard::register_config(probe.0, 202, &chord("F11"))
            .expect("Ctrl+Alt+Shift+F11 must be free before this test");
        drop(free_f11);
        let mut runtime = SettingsRuntime {
            config: original.clone(),
            notice: None,
            save_allowed: true,
            revision: store.load().revision,
            store: Some(store),
            hotkey: Some(old),
            next_id: 1000,
        };
        let conflicting = Config {
            language: Language::English,
            hotkey: chord("F10"),
            ..original.clone()
        };

        // A real RegisterHotKey conflict changes neither disk nor accepted ID.
        let error = runtime.apply(owner.0, conflicting.clone()).unwrap_err();
        assert!(error.contains("快捷键"), "{error}");
        assert_eq!(language(), Language::SimplifiedChinese);
        assert_eq!(runtime.config, original);
        assert_eq!(runtime.hotkey_id(), Some(101));
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
        assert!(HotkeyGuard::register_config(probe.0, 203, &original.hotkey).is_err());
        drop(blocker);

        // The new key can register, but a read-only share denies rename. The
        // temporary hotkey must be released while the old one remains owned.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let error = runtime.apply(owner.0, conflicting).unwrap_err();
        assert!(error.contains("保存失败"), "{error}");
        assert_eq!(language(), Language::SimplifiedChinese);
        assert_eq!(runtime.config, original);
        assert_eq!(runtime.hotkey_id(), Some(101));
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
        assert!(HotkeyGuard::register_config(probe.0, 204, &original.hotkey).is_err());
        let released_temporary = HotkeyGuard::register_config(probe.0, 205, &chord("F10"))
            .expect("failed save must release its temporary hotkey");
        drop(released_temporary);
        drop(lock);

        // An external valid edit after load must roll back a newly registered
        // shortcut just like an I/O failure, retaining the accepted revision.
        let external = Config {
            default_format: ColorFormat::CssRgb,
            ..original.clone()
        };
        runtime.store.as_ref().unwrap().save(&external).unwrap();
        let accepted_revision = runtime.revision.clone();
        let error = runtime
            .apply(
                owner.0,
                Config {
                    hotkey: chord("F10"),
                    ..original.clone()
                },
            )
            .unwrap_err();
        assert!(error.contains("外部修改"), "{error}");
        assert_eq!(runtime.config, original);
        assert_eq!(runtime.revision, accepted_revision);
        assert_eq!(runtime.hotkey_id(), Some(101));
        assert_eq!(runtime.store.as_ref().unwrap().load().config, external);
        drop(
            HotkeyGuard::register_config(probe.0, 210, &chord("F10"))
                .expect("conflicting revision must release its temporary hotkey"),
        );
        // Restore the exact accepted bytes to continue the successful-save test.
        std::fs::write(&path, &original_bytes).unwrap();

        let updated = Config {
            language: Language::English,
            hotkey: chord("F11"),
            default_format: ColorFormat::Hsl,
            auto_copy_on_pick: true,
            ..original.clone()
        };
        let retired = runtime
            .apply(owner.0, updated.clone())
            .unwrap()
            .expect("successful replacement returns the old registration");
        assert_eq!(runtime.config, updated);
        assert_eq!(language(), Language::English);
        assert_ne!(runtime.hotkey_id(), Some(101));
        assert_eq!(runtime.store.as_ref().unwrap().load().config, updated);
        assert_eq!(
            runtime.store.as_ref().unwrap().load().revision,
            runtime.revision
        );
        // The host can publish hotkey_id() before dropping this retired guard.
        assert!(HotkeyGuard::register_config(probe.0, 206, &original.hotkey).is_err());
        drop(retired);
        let released_old = HotkeyGuard::register_config(probe.0, 207, &original.hotkey)
            .expect("retiring the old guard must release the old chord");
        drop(released_old);
        assert!(HotkeyGuard::register_config(probe.0, 208, &updated.hotkey).is_err());
        drop(runtime);
        let released_current = HotkeyGuard::register_config(probe.0, 209, &updated.hotkey)
            .expect("runtime destruction must release the current chord");
        drop(released_current);
    }
}

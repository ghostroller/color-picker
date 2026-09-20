use color_picker::{
    app::config::{Config, ConfigStore, HotkeyConfig},
    core::format::ColorFormat,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

struct TempDirectory(PathBuf);
impl TempDirectory {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "color-picker-config-test-{}-{time}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("isolated test directory: {error}"),
            }
        }
        panic!("could not create unique isolated test directory");
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.json")
    }
}
impl Drop for TempDirectory {
    fn drop(&mut self) {
        // The directory was created exclusively by this test, never adopted.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_config_defaults_and_valid_changes_round_trip() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    let loaded = store.load();
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.warning.is_none() && loaded.save_allowed);
    assert_eq!(loaded.config.hotkey.label(), "Ctrl + Alt + C");
    assert_eq!(loaded.config.hotkey.virtual_key().unwrap(), u32::from(b'C'));
    assert_eq!(loaded.config.default_format, ColorFormat::Hex);
    assert!(!loaded.config.auto_copy_on_pick);
    store.save(&loaded.config).unwrap();
    let changed = Config {
        hotkey: HotkeyConfig {
            ctrl: true,
            alt: false,
            shift: true,
            key: "F11".into(),
        },
        default_format: ColorFormat::CssRgb,
        auto_copy_on_pick: true,
        ..Config::default()
    };
    store.save(&changed).unwrap();
    assert_eq!(store.load().config, changed);
    assert_eq!(changed.hotkey.virtual_key().unwrap(), 0x7a);
    assert!(
        std::fs::read_to_string(store.path())
            .unwrap()
            .contains("\"css_rgb\"")
    );
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn unsupported_keys_and_strict_json_are_rejected() {
    for key in ["F12", "F0", "F01", "c", "Ctrl+C", " ", "", "ESC"] {
        assert!(
            HotkeyConfig {
                key: key.into(),
                ..HotkeyConfig::default()
            }
            .validate()
            .is_err(),
            "{key}"
        );
    }
    assert!(
        HotkeyConfig {
            ctrl: false,
            alt: false,
            shift: true,
            key: "C".into()
        }
        .validate()
        .is_err()
    );
    for key in ["A", "Z", "0", "9", "F1", "F10", "F11"] {
        assert!(
            HotkeyConfig {
                key: key.into(),
                ..HotkeyConfig::default()
            }
            .validate()
            .is_ok()
        );
    }
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value["surprise"] = true.into();
    assert!(serde_json::from_value::<Config>(value).is_err());
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value["hotkey"]["win"] = true.into();
    assert!(serde_json::from_value::<Config>(value).is_err());
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value["default_format"] = "unknown".into();
    assert!(serde_json::from_value::<Config>(value).is_err());
}

#[test]
fn damaged_and_future_files_are_preserved_on_load_and_save() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    for original in [
        "{broken",
        "{\"schema_version\":2,\"new_schema_field\":true}",
    ] {
        std::fs::write(store.path(), original).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warning.is_some());
        assert!(!loaded.save_allowed);
        assert!(store.save(&Config::default()).is_err());
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), original);
    }
}

#[test]
#[cfg(windows)]
fn failed_replace_preserves_old_config_and_removes_temporary_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    let original = Config::default();
    store.save(&original).unwrap();
    let original_bytes = std::fs::read(store.path()).unwrap();
    // Allow readers but prevent replacing this test's file while the handle is
    // held. No user file, permission changes, or live hotkey is involved.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(store.path())
        .unwrap();
    let updated = Config {
        auto_copy_on_pick: true,
        ..original.clone()
    };
    assert!(store.save(&updated).is_err());
    assert_eq!(std::fs::read(store.path()).unwrap(), original_bytes);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    drop(lock);
    store.save(&updated).unwrap();
    assert_eq!(store.load().config, updated);
}

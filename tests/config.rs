use color_picker::{
    app::config::{AppearanceConfig, Config, ConfigError, ConfigStore, HotkeyConfig},
    app::i18n::Language,
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
fn editing_session_rejects_external_changes_and_accepts_a_reloaded_revision() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    store.save(&Config::default()).unwrap();
    let loaded = store.load();
    let expected = loaded.revision.unwrap();
    let draft = Config {
        quick_pick: true,
        ..loaded.config
    };
    let external = Config {
        default_format: ColorFormat::Hsl,
        ..Config::default()
    };
    store.save(&external).unwrap();
    let external_bytes = std::fs::read(store.path()).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            store.save_if_unchanged(&draft, &expected),
            Err(ConfigError::ChangedSinceLoad(_))
        ));
        assert_eq!(std::fs::read(store.path()).unwrap(), external_bytes);
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    }
    let reloaded = store.load();
    let revision = store
        .save_if_unchanged(&draft, reloaded.revision.as_ref().unwrap())
        .unwrap();
    assert_eq!(store.load().revision.as_ref(), Some(&revision));
    // Consecutive Apply operations must carry forward the successful revision.
    store.save_if_unchanged(&external, &revision).unwrap();
    assert_eq!(store.load().config, external);
}

#[test]
fn editing_revision_detects_external_creation_deletion_and_byte_only_edits() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    let missing = store.load().revision.unwrap();
    let config = Config::default();
    let created = store.save_if_unchanged(&config, &missing).unwrap();
    assert!(matches!(
        store.save_if_unchanged(&config, &missing),
        Err(ConfigError::ChangedSinceLoad(_))
    ));
    let mut reformatted = std::fs::read(store.path()).unwrap();
    reformatted.push(b' ');
    std::fs::write(store.path(), &reformatted).unwrap();
    assert!(matches!(
        store.save_if_unchanged(&config, &created),
        Err(ConfigError::ChangedSinceLoad(_))
    ));
    assert_eq!(std::fs::read(store.path()).unwrap(), reformatted);
    let present = store.load().revision.unwrap();
    std::fs::remove_file(store.path()).unwrap();
    assert!(matches!(
        store.save_if_unchanged(&config, &present),
        Err(ConfigError::ChangedSinceLoad(_))
    ));
    assert!(!store.path().exists());
}

#[test]
fn invalid_external_configuration_remains_protected_with_a_revision() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    let revision = store.load().revision.unwrap();
    let invalid = b"{ invalid json }";
    std::fs::write(store.path(), invalid).unwrap();
    assert!(matches!(
        store.save_if_unchanged(&Config::default(), &revision),
        Err(ConfigError::ExistingFileProtected { .. })
    ));
    assert!(store.load().revision.is_none());
    assert_eq!(std::fs::read(store.path()).unwrap(), invalid);
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
    assert!(!loaded.config.quick_pick);
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
        quick_pick: true,
        language: Language::English,
        appearance: AppearanceConfig {
            border_width_dip: 4,
            background_transparency_percent: 65,
        },
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
fn previous_schema_one_configs_default_missing_preferences() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    let mut previous = serde_json::to_value(Config::default()).unwrap();
    previous.as_object_mut().unwrap().remove("appearance");
    previous.as_object_mut().unwrap().remove("quick_pick");
    previous.as_object_mut().unwrap().remove("language");
    std::fs::write(store.path(), serde_json::to_vec(&previous).unwrap()).unwrap();
    let loaded = store.load();
    assert!(loaded.save_allowed && loaded.warning.is_none());
    assert_eq!(loaded.config.appearance, AppearanceConfig::default());
    assert!(!loaded.config.quick_pick);
    assert_eq!(loaded.config.language, Language::default());

    previous["appearance"] = serde_json::json!({"border_width_dip": 5});
    let partial: Config = serde_json::from_value(previous).unwrap();
    assert_eq!(partial.appearance.border_width_dip, 5);
    assert_eq!(partial.appearance.background_transparency_percent, 35);
    store.save(&partial).unwrap();
    assert_eq!(store.load().config, partial);
}

#[test]
fn appearance_limits_reject_invalid_changes_and_protect_existing_files() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    let original = Config::default();
    store.save(&original).unwrap();
    let original_bytes = std::fs::read(store.path()).unwrap();
    for appearance in [
        AppearanceConfig {
            border_width_dip: 7,
            ..AppearanceConfig::default()
        },
        AppearanceConfig {
            background_transparency_percent: 81,
            ..AppearanceConfig::default()
        },
    ] {
        let invalid = Config {
            appearance,
            ..original.clone()
        };
        assert!(store.save(&invalid).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), original_bytes);
    }
    for appearance in [
        AppearanceConfig {
            border_width_dip: 0,
            background_transparency_percent: 0,
        },
        AppearanceConfig {
            border_width_dip: 6,
            background_transparency_percent: 80,
        },
    ] {
        assert!(appearance.validate().is_ok());
    }

    let invalid_on_disk = Config {
        appearance: AppearanceConfig {
            background_transparency_percent: 81,
            ..AppearanceConfig::default()
        },
        ..original.clone()
    };
    let invalid_bytes = serde_json::to_vec(&invalid_on_disk).unwrap();
    std::fs::write(store.path(), &invalid_bytes).unwrap();
    let loaded = store.load();
    assert!(!loaded.save_allowed && loaded.warning.is_some());
    assert_eq!(loaded.config, original);
    assert!(store.save(&original).is_err());
    assert_eq!(std::fs::read(store.path()).unwrap(), invalid_bytes);
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
    value["appearance"]["blur"] = 50.into();
    assert!(serde_json::from_value::<Config>(value).is_err());
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value["default_format"] = "unknown".into();
    assert!(serde_json::from_value::<Config>(value).is_err());
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value["language"] = "fr".into();
    assert!(serde_json::from_value::<Config>(value).is_err());
}

#[test]
fn both_interface_languages_round_trip_without_changing_other_preferences() {
    let directory = TempDirectory::new();
    let store = ConfigStore::new(directory.path());
    for language in [Language::English, Language::SimplifiedChinese] {
        let config = Config {
            language,
            quick_pick: true,
            ..Config::default()
        };
        store.save(&config).unwrap();
        let restored = store.load();
        assert!(restored.warning.is_none() && restored.save_allowed);
        assert_eq!(restored.config, config);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(json["language"], language.code());
    }
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

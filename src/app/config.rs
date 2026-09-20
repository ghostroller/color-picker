//! Validated user settings and atomic, explicitly requested persistence.

use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::core::format::ColorFormat;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub hotkey: HotkeyConfig,
    pub default_format: ColorFormat,
    pub auto_copy_on_pick: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            hotkey: HotkeyConfig::default(),
            default_format: ColorFormat::Hex,
            auto_copy_on_pick: false,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchema(u64::from(
                self.schema_version,
            )));
        }
        self.hotkey.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeyConfig {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            ctrl: true,
            alt: true,
            shift: false,
            key: "C".into(),
        }
    }
}

impl HotkeyConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.ctrl && !self.alt {
            return Err(ConfigError::InvalidHotkey("快捷键至少需要包含 Ctrl 或 Alt"));
        }
        self.key_code().map(|_| ())
    }

    pub fn virtual_key(&self) -> Result<u32, ConfigError> {
        self.validate()?;
        self.key_code()
    }

    fn key_code(&self) -> Result<u32, ConfigError> {
        let key = self.key.as_bytes();
        if key.len() == 1 && (key[0].is_ascii_uppercase() || key[0].is_ascii_digit()) {
            return Ok(u32::from(key[0]));
        }
        let function = match self.key.as_str() {
            "F1" => 1,
            "F2" => 2,
            "F3" => 3,
            "F4" => 4,
            "F5" => 5,
            "F6" => 6,
            "F7" => 7,
            "F8" => 8,
            "F9" => 9,
            "F10" => 10,
            "F11" => 11,
            _ => {
                return Err(ConfigError::InvalidHotkey(
                    "主键仅支持 A–Z、0–9、F1–F11；F12 为系统保留键",
                ));
            }
        };
        // Win32 virtual-key codes: VK_F1 = 0x70, subsequent F-keys are contiguous.
        Ok(0x70 + function - 1)
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::with_capacity(4);
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(&self.key);
        parts.join(" + ")
    }
}

#[derive(Debug)]
pub enum ConfigError {
    UnsupportedSchema(u64),
    InvalidHotkey(&'static str),
    InvalidJson(serde_json::Error),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    ExistingFileProtected {
        path: PathBuf,
        reason: String,
    },
    ChangedDuringSave(PathBuf),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema(version) => write!(
                formatter,
                "不支持配置版本 {version}，当前仅支持版本 {SCHEMA_VERSION}"
            ),
            Self::InvalidHotkey(reason) => formatter.write_str(reason),
            Self::InvalidJson(error) => write!(formatter, "配置 JSON 无效：{error}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation}（{}）：{source}", path.display()),
            Self::ExistingFileProtected { path, reason } => write!(
                formatter,
                "不会覆盖无法识别的原配置 {}：{reason}；请先修复或移走该文件并重新启动",
                path.display()
            ),
            Self::ChangedDuringSave(path) => write!(
                formatter,
                "保存期间配置文件 {} 已被修改，本次未覆盖；请重新加载后再试",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub warning: Option<String>,
    pub save_allowed: bool,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> LoadedConfig {
        match self.read_checked() {
            Ok(Some((config, _))) => LoadedConfig {
                config,
                warning: None,
                save_allowed: true,
            },
            Ok(None) => LoadedConfig {
                config: Config::default(),
                warning: None,
                save_allowed: true,
            },
            Err(error) => LoadedConfig {
                config: Config::default(),
                save_allowed: false,
                warning: Some(format!(
                    "配置无法使用：{error}。已使用默认设置；请修复或移走原文件并重新启动，当前不会覆盖该文件。"
                )),
            },
        }
    }

    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        config.validate()?;
        // The UI's save_allowed flag is not sufficient: the on-disk file may
        // have changed since startup, so protect invalid/future schemas again.
        let previous = self
            .read_checked()
            .map_err(|error| ConfigError::ExistingFileProtected {
                path: self.path.clone(),
                reason: error.to_string(),
            })?
            .map(|(_, bytes)| bytes);
        let mut bytes = serde_json::to_vec_pretty(config).map_err(ConfigError::InvalidJson)?;
        bytes.push(b'\n');
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|source| io_error("无法创建配置目录", parent, source))?;
        let name = self.path.file_name().ok_or_else(|| {
            io_error(
                "配置路径不是文件",
                &self.path,
                io::Error::new(io::ErrorKind::InvalidInput, "missing filename"),
            )
        })?;
        let mut temporary = TemporaryConfig::create(parent, name)?;
        temporary.write(&bytes)?;

        // A second check catches an external edit during our temporary write,
        // while keeping the old file intact on every reported failure.
        let current = self
            .read_checked()
            .map_err(|error| ConfigError::ExistingFileProtected {
                path: self.path.clone(),
                reason: error.to_string(),
            })?
            .map(|(_, bytes)| bytes);
        if current != previous {
            return Err(ConfigError::ChangedDuringSave(self.path.clone()));
        }
        std::fs::rename(&temporary.path, &self.path)
            .map_err(|source| io_error("无法原子替换配置文件，原配置已保留", &self.path, source))?;
        temporary.committed = true;
        Ok(())
    }

    fn read_checked(&self) -> Result<Option<(Config, Vec<u8>)>, ConfigError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error("无法读取配置文件", &self.path, source)),
        };
        // Inspect the version first so a newer schema reports a clear version
        // error even when it introduces fields this version cannot deserialize.
        #[derive(Deserialize)]
        struct Version {
            schema_version: u64,
        }
        let version: Version = serde_json::from_slice(&bytes).map_err(ConfigError::InvalidJson)?;
        if version.schema_version != u64::from(SCHEMA_VERSION) {
            return Err(ConfigError::UnsupportedSchema(version.schema_version));
        }
        let config: Config = serde_json::from_slice(&bytes).map_err(ConfigError::InvalidJson)?;
        config.validate()?;
        Ok(Some((config, bytes)))
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

struct TemporaryConfig {
    path: PathBuf,
    file: Option<File>,
    committed: bool,
}

impl TemporaryConfig {
    fn create(parent: &Path, target_name: &std::ffi::OsStr) -> Result<Self, ConfigError> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        for _ in 0..32 {
            let tick = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let mut name = OsString::from(".");
            name.push(target_name);
            name.push(format!(".{}.{tick}.{serial}.tmp", std::process::id()));
            let path = parent.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        committed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(io_error("无法创建配置临时文件", &path, source)),
            }
        }
        Err(io_error(
            "无法分配唯一配置临时文件",
            parent,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "temporary filename collisions",
            ),
        ))
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), ConfigError> {
        if let Some(file) = self.file.as_mut() {
            file.write_all(bytes)
                .map_err(|source| io_error("无法写入配置临时文件", &self.path, source))?;
            file.sync_all()
                .map_err(|source| io_error("无法同步配置临时文件", &self.path, source))?;
        }
        // Close before rename, including on Windows where open handles can
        // prevent a replacement. Drop also closes before cleaning a failure.
        self.file.take();
        Ok(())
    }
}

impl Drop for TemporaryConfig {
    fn drop(&mut self) {
        self.file.take();
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

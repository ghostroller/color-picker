//! Resolve the configuration directory through the current user's known folder.

use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use windows::{
    Win32::{
        Foundation::E_FAIL,
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath},
    },
    core::{Error, PWSTR, Result},
};

pub fn default_config_path() -> Result<PathBuf> {
    // KF_FLAG_DEFAULT only locates the known folder; it does not request its
    // creation or alter the folder itself. Saving creates our child directory.
    let allocation = KnownFolderPath(unsafe {
        SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)?
    });
    if allocation.0.is_null() {
        return Err(Error::new(
            E_FAIL,
            "LocalAppData known folder returned a null path",
        ));
    }
    let path = OsString::from_wide(unsafe { allocation.0.as_wide() });
    if path.is_empty() {
        return Err(Error::new(
            E_FAIL,
            "LocalAppData known folder returned an empty path",
        ));
    }
    Ok(PathBuf::from(path).join("color-picker").join("config.json"))
}

struct KnownFolderPath(PWSTR);
impl Drop for KnownFolderPath {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.0.cast())) };
    }
}

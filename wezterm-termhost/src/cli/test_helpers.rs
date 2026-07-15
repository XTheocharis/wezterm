//! Shared test helpers for `enable.rs` and `disable.rs`.

#![cfg(all(windows, test))]

use std::time::{SystemTime, UNIX_EPOCH};
use winreg::enums::*;
use winreg::RegKey;

pub(crate) const STARTUP_KEY: &str = "Console\\%%Startup";
pub(crate) const DELEGATION_CONSOLE: &str = "DelegationConsole";
pub(crate) const DELEGATION_TERMINAL: &str = "DelegationTerminal";
pub(crate) const LAST_CONSOLE: &str = "WezTerm_Last_Console";
pub(crate) const LAST_TERMINAL: &str = "WezTerm_Last_Terminal";

/// Snapshot of the four Delegation/backup values under `HKCU\Console\%%Startup`.
#[derive(Default)]
pub(crate) struct StartupValues {
    pub delegation_console: Option<String>,
    pub delegation_terminal: Option<String>,
    pub last_console: Option<String>,
    pub last_terminal: Option<String>,
}

impl StartupValues {
    fn capture(startup: &RegKey) -> Self {
        Self {
            delegation_console: startup.get_value(DELEGATION_CONSOLE).ok(),
            delegation_terminal: startup.get_value(DELEGATION_TERMINAL).ok(),
            last_console: startup.get_value(LAST_CONSOLE).ok(),
            last_terminal: startup.get_value(LAST_TERMINAL).ok(),
        }
    }

    fn restore_value(startup: &RegKey, name: &str, value: &Option<String>) {
        if let Some(value) = value {
            startup.set_value(name, value).unwrap();
        } else {
            let _ = startup.delete_value(name);
        }
    }

    fn restore(&self, startup: &RegKey) {
        Self::restore_value(startup, DELEGATION_CONSOLE, &self.delegation_console);
        Self::restore_value(startup, DELEGATION_TERMINAL, &self.delegation_terminal);
        Self::restore_value(startup, LAST_CONSOLE, &self.last_console);
        Self::restore_value(startup, LAST_TERMINAL, &self.last_terminal);
    }
}

/// RAII guard that backs up `HKCU\Console\%%Startup` on construction and
/// restores it on drop.
pub(crate) struct StartupKeyGuard {
    existed: bool,
    values: StartupValues,
}

impl StartupKeyGuard {
    pub(crate) fn capture() -> Self {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        match hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_READ) {
            Ok(startup) => Self {
                existed: true,
                values: StartupValues::capture(&startup),
            },
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Self {
                existed: false,
                values: StartupValues::default(),
            },
            Err(e) => panic!("opening HKCU\\{} for test backup: {}", STARTUP_KEY, e),
        }
    }
}

impl Drop for StartupKeyGuard {
    fn drop(&mut self) {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();
        self.values.restore(&startup);

        if !self.existed
            && startup.enum_keys().next().is_none()
            && startup.enum_values().next().is_none()
        {
            drop(startup);
            let _ = hkcu.delete_subkey(STARTUP_KEY);
        }
    }
}

/// RAII guard for a test CLSID under `HKCU\Software\Classes\CLSID\{...}`.
/// Cleans up `LocalServer32` subkey and the CLSID key itself on drop.
pub(crate) struct TestClsid {
    pub clsid: String,
}

impl TestClsid {
    /// Generate a unique GUID-format CLSID.
    pub(crate) fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id() as u128;
        let clsid = format!(
            "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
            nanos & 0xffff_ffff,
            (nanos >> 32) & 0xffff,
            (nanos >> 48) & 0xffff,
            pid & 0xffff,
            (nanos >> 64) & 0xffff_ffff_ffff
        );
        cleanup_clsid(&clsid);
        Self { clsid }
    }

    /// Generate a CLSID with a human-readable name embedded (for debugging).
    pub(crate) fn new_named(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let clsid = format!("{{WEZTERM-TEST-{}-{}-{}}}", std::process::id(), name, nanos);
        cleanup_clsid(&clsid);
        Self { clsid }
    }

    pub(crate) fn key_path(&self) -> String {
        crate::registration::clsid_registry_path(&self.clsid)
    }
}

impl Drop for TestClsid {
    fn drop(&mut self) {
        cleanup_clsid(&self.clsid);
    }
}

pub(crate) fn cleanup_clsid(clsid: &str) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key_path = crate::registration::clsid_registry_path(clsid);
    if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&key_path, KEY_WRITE) {
        let _ = clsid_key.delete_subkey("LocalServer32");
    }
    let _ = hkcu.delete_subkey(&key_path);
}

pub(crate) fn cleanup_backup_values() {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(startup) = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_WRITE) {
        let _ = startup.delete_value(LAST_CONSOLE);
        let _ = startup.delete_value(LAST_TERMINAL);
    }
}

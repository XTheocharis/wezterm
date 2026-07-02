//! Bundled `OpenConsole.exe` fallback registration.

use std::path::{Path, PathBuf};
use winreg::enums::*;
use winreg::RegKey;

use super::{
    clsid_registry_path, find_clsid_server_path, WEZTERM_OWNED_VALUE,
    WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID,
};

/// Resolve the path to the bundled OpenConsole.exe.
pub fn resolve_bundled_openconsole_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    let candidate = exe_dir.join("OpenConsole.exe");
    if candidate.exists() {
        return Some(candidate);
    }

    let mut dir: &Path = exe_dir.as_path();
    while let Some(parent) = dir.parent() {
        let target_dir = parent.join("target");
        if target_dir.is_dir() {
            for profile in ["debug", "release"] {
                let candidate = target_dir.join(profile).join("OpenConsole.exe");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        dir = parent;
    }

    None
}

/// Register the bundled `OpenConsole.exe` in HKCU if no COM server
/// is already registered for the fallback CLSID.
pub fn register_openconsole_fallback() -> anyhow::Result<()> {
    let bundled = match resolve_bundled_openconsole_path() {
        Some(p) => p,
        None => {
            log::warn!(
                "Bundled OpenConsole.exe not found; \
                 skipping fallback registration"
            );
            return Ok(());
        }
    };

    let clsid = WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID;

    // If an on-disk server is registered, decide between skip and refresh
    // based on ownership. We clobber only entries we previously wrote (marked
    // via WezTermOwned) when the recorded path no longer matches the
    // bundled OpenConsole.exe (e.g. portable install moved, side-by-side
    // upgrade); foreign registrations (HKLM, WT MSIX, or any unmarked entry)
    // are left alone.
    if let Some(existing) = find_clsid_server_path(clsid) {
        match read_wezterm_owned_path(clsid) {
            None => {
                log::info!(
                    "CLSID {} owned by another host at {}; \
                     skipping fallback",
                    clsid,
                    existing.display()
                );
                return Ok(());
            }
            Some(ours) if ours == bundled => {
                log::info!(
                    "Bundled OpenConsole.exe already registered at {}; \
                     skipping fallback",
                    existing.display()
                );
                return Ok(());
            }
            Some(stale) => {
                log::info!(
                    "Refreshing WezTerm-owned OpenConsole fallback\n  \
                     was: {}\n  now: {}",
                    stale.display(),
                    bundled.display(),
                );
            }
        }
    }

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key_path = clsid_registry_path(clsid);
    let (key, _) = hkcu
        .create_subkey(&key_path)
        .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", key_path, e))?;
    key.set_value("", &"WezTerm-bundled OpenConsole (Microsoft MIT-licensed)")
        .map_err(|e| anyhow::anyhow!("writing CLSID default value: {}", e))?;
    key.set_value(WEZTERM_OWNED_VALUE, &1u32)
        .map_err(|e| anyhow::anyhow!("writing WezTermOwned marker: {}", e))?;
    let (local_server, _) = key
        .create_subkey("LocalServer32")
        .map_err(|e| anyhow::anyhow!("creating LocalServer32: {}", e))?;
    let value = format!("\"{}\"", bundled.display());
    local_server
        .set_value("", &value)
        .map_err(|e| anyhow::anyhow!("writing LocalServer32 value: {}", e))?;
    log::info!(
        "Registered bundled OpenConsole.exe at {} \
         (HKCU\\Software\\Classes\\CLSID\\{}\\LocalServer32)",
        bundled.display(),
        clsid
    );
    Ok(())
}

/// Read the LocalServer32 path previously written by WezTerm under this
/// CLSID in HKCU. Returns `None` if the CLSID isn't registered in HKCU,
/// lacks the `WezTermOwned` marker (i.e. owned by another host), or has
/// no readable LocalServer32 value.
fn read_wezterm_owned_path(clsid: &str) -> Option<PathBuf> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(clsid_registry_path(clsid), KEY_READ)
        .ok()?;
    let owned: u32 = key.get_value(WEZTERM_OWNED_VALUE).ok()?;
    if owned == 0 {
        return None;
    }
    let local_server = key.open_subkey("LocalServer32").ok()?;
    let value: String = local_server.get_value("").ok()?;
    Some(PathBuf::from(super::extract_exe_token(&value)))
}

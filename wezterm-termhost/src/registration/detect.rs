//! Detect other Windows Terminal installations.
//! Checks whether WT-branded OpenConsole CLSIDs are registered, and
//! whether a COM server exe exists on disk for a given CLSID.

use std::path::PathBuf;
use winreg::enums::*;
use winreg::RegKey;

use super::read_local_server_exe;

/// OpenConsole CLSIDs for the four WT channels (Release, Preview, Canary,
/// Dev). Each channel ships OpenConsole.exe with a different CLSID
/// (`microsoft/terminal/src/cascadia/CascadiaPackage/Package-*.appxmanifest`).
fn wt_brand_openconsole_clsids() -> Vec<&'static str> {
    crate::cli::KNOWN_HOSTS
        .iter()
        .filter(|h| h.id.starts_with("wt-"))
        .map(|h| h.console_clsid)
        .collect()
}

/// Find a registered COM server for `clsid` by checking HKCU then HKLM.
/// Returns the exe path if the file exists on disk.
pub(crate) fn find_clsid_server_path(clsid: &str) -> Option<PathBuf> {
    for root in [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ] {
        if let Some(path) = read_local_server_exe(&root, clsid) {
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

pub fn clsid_server_exists(clsid: &str) -> bool {
    find_clsid_server_path(clsid).is_some()
}

/// Returns `true` iff any Windows Terminal brand
/// (Release / Preview / Canary / Dev) is installed.
pub fn is_wt_installed() -> bool {
    wt_brand_openconsole_clsids()
        .iter()
        .any(|c| clsid_server_exists(c))
}

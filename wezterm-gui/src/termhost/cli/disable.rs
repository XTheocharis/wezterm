use crate::termhost::{key_is_wezterm_owned, TERMHOST_HANDOFF_IIDS, WEZTERM_PROXY_STUB_CLSID};

pub struct DisableCommand {}

impl DisableCommand {
    pub fn run() -> anyhow::Result<()> {
        // Restore first but don't abort on error. The unregistrations
        // below check ownership and are safe in any Delegation state.
        // Leaving stale COM entries in the registry is worse than a
        // failed restore, so we propagate the restore error only after
        // cleanup is done.
        let restore_result = crate::termhost::registration::restore_delegation_backup();
        match &restore_result {
            Ok(true) => println!("Restored previous default terminal selection."),
            Ok(false) => println!(
                "Current default is not WezTerm; leaving DelegationConsole/DelegationTerminal unchanged."
            ),
            // Use eprintln (not log::error!) so the user always sees this.
            // CLI subcommands don't set up env_logger before dispatching,
            // so log::error! would be invisible.
            Err(e) => eprintln!("warning: delegation restore failed: {e:#}; continuing with cleanup"),
        }

        let mut was_registered = false;
        match unregister_local_server_for_unpackaged() {
            Ok(true) => {
                was_registered = true;
                println!(
                    "Removed WezTerm-owned HKCU\\Software\\Classes\\CLSID\\{} entry.",
                    crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID
                );
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!("warning: local server unregister failed: {e:#}; continuing with cleanup")
            }
        }

        unregister_proxy_stub_per_user();
        unregister_openconsole_fallback();

        if was_registered {
            println!("WezTerm is no longer registered as the Windows default terminal.");
        } else {
            println!("WezTerm was not registered as the default terminal; nothing to do.");
        }
        restore_result?;
        Ok(())
    }
}

/// Open a registry key under `parent` and check if WezTerm owns it.
/// Returns `false` on `NotFound` (key absent — not ours). Logs and
/// returns `false` on other errors: we skip deletion when uncertain,
/// since removing a foreign entry would break the other application's
/// defterm registration.
fn subkey_is_wezterm_owned(parent: &winreg::RegKey, path: &str) -> bool {
    use winreg::enums::KEY_READ;
    match parent.open_subkey_with_flags(path, KEY_READ) {
        Ok(k) => key_is_wezterm_owned(&k),
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            log::warn!(
                "Ownership check for {} failed: {}; skipping deletion",
                path,
                e
            );
            false
        }
    }
}

fn unregister_local_server_for_unpackaged() -> anyhow::Result<bool> {
    use winreg::enums::*;
    use winreg::RegKey;

    let clsid = crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    unregister_local_server_for_clsid(&hkcu, clsid)
}

fn unregister_local_server_for_clsid(hkcu: &winreg::RegKey, clsid: &str) -> anyhow::Result<bool> {
    use winreg::enums::*;

    let key_path = crate::termhost::registration::clsid_registry_path(clsid);
    let owned_by_wezterm = subkey_is_wezterm_owned(hkcu, &key_path);

    if owned_by_wezterm {
        if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&key_path, KEY_WRITE) {
            let _ = clsid_key.delete_subkey("LocalServer32");
        }
        let _ = hkcu.delete_subkey(&key_path);
    }
    Ok(owned_by_wezterm)
}

fn unregister_proxy_stub_per_user() {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    for iid in TERMHOST_HANDOFF_IIDS {
        let iid_path = format!("Software\\Classes\\Interface\\{}", iid);
        let ps_path = format!("{}\\ProxyStubClsid32", iid_path);

        let should_remove_proxy_stub = subkey_is_wezterm_owned(&hkcu, &ps_path);

        if should_remove_proxy_stub {
            if let Ok(iid_key) = hkcu.open_subkey_with_flags(&iid_path, KEY_WRITE) {
                let _ = iid_key.delete_subkey("ProxyStubClsid32");
            }
        }

        let should_remove_interface = subkey_is_wezterm_owned(&hkcu, &iid_path);

        if should_remove_interface {
            let _ = hkcu.delete_subkey(&iid_path);
        }
    }

    let clsid_path = format!("Software\\Classes\\CLSID\\{}", WEZTERM_PROXY_STUB_CLSID);
    let inproc_path = format!("{}\\InProcServer32", clsid_path);
    let should_remove_clsid = subkey_is_wezterm_owned(&hkcu, &clsid_path);
    let should_remove_inproc = should_remove_clsid || subkey_is_wezterm_owned(&hkcu, &inproc_path);

    if should_remove_inproc {
        if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&clsid_path, KEY_WRITE) {
            let _ = clsid_key.delete_subkey("InProcServer32");
        }
    }
    if should_remove_clsid {
        let _ = hkcu.delete_subkey(&clsid_path);
    }
}

fn unregister_openconsole_fallback() {
    use winreg::enums::*;
    use winreg::RegKey;

    let clsid = crate::termhost::WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID;
    let key_path = crate::termhost::registration::clsid_registry_path(clsid);
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let owned_by_wezterm = subkey_is_wezterm_owned(&hkcu, &key_path);

    if owned_by_wezterm {
        if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&key_path, KEY_WRITE) {
            let _ = clsid_key.delete_subkey("LocalServer32");
        }
        let _ = hkcu.delete_subkey(&key_path);
        println!(
            "Removed WezTerm-owned OpenConsole fallback (HKCU\\Software\\Classes\\CLSID\\{}).",
            clsid
        );
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use crate::termhost::cli::test_helpers::{
        cleanup_backup_values, StartupKeyGuard, TestClsid, DELEGATION_CONSOLE, DELEGATION_TERMINAL,
        LAST_CONSOLE, LAST_TERMINAL, STARTUP_KEY,
    };
    use winreg::enums::*;
    use winreg::RegKey;

    // === Ownership-check tests ===

    /// Ported: unowned CLSID entries must NOT be removed.
    #[test]
    fn unregister_local_server_preserves_unowned_clsid() {
        let test_key = TestClsid::new_named("unowned");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key_path = test_key.key_path();

        {
            let (key, _) = hkcu.create_subkey(&key_path).unwrap();
            key.set_value("", &"External Terminal Handoff").unwrap();
            let (local_server, _) = key.create_subkey("LocalServer32").unwrap();
            local_server
                .set_value("", &"\"C:\\external-terminal.exe\"")
                .unwrap();
        }

        let removed = unregister_local_server_for_clsid(&hkcu, &test_key.clsid).unwrap();
        assert!(!removed);

        let key = hkcu.open_subkey_with_flags(&key_path, KEY_READ).unwrap();
        let local_server = key.open_subkey("LocalServer32").unwrap();
        let value: String = local_server.get_value("").unwrap();
        assert_eq!(value, "\"C:\\external-terminal.exe\"");
    }

    /// Ported: WezTerm-owned CLSID entries ARE removed.
    #[test]
    fn unregister_local_server_removes_wezterm_owned_clsid() {
        let test_key = TestClsid::new_named("owned");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key_path = test_key.key_path();

        {
            let (key, _) = hkcu.create_subkey(&key_path).unwrap();
            key.set_value(crate::termhost::WEZTERM_OWNED_VALUE, &1u32)
                .unwrap();
            let (local_server, _) = key.create_subkey("LocalServer32").unwrap();
            local_server
                .set_value("", &"\"C:\\wezterm-gui.exe\"")
                .unwrap();
        }

        let removed = unregister_local_server_for_clsid(&hkcu, &test_key.clsid).unwrap();
        assert!(removed);
        assert!(hkcu.open_subkey_with_flags(&key_path, KEY_READ).is_err());
    }

    /// Verify restore_delegation_backup restores when WezTerm is current default.
    #[test]
    fn restore_happy_path() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();

        // Both Delegation* values must be set so current_registration()
        // returns Some (partial state would return None and skip restore).
        startup
            .set_value(
                DELEGATION_CONSOLE,
                &crate::termhost::WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID.to_string(),
            )
            .unwrap();
        startup
            .set_value(
                DELEGATION_TERMINAL,
                &crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID.to_string(),
            )
            .unwrap();
        startup
            .set_value(LAST_CONSOLE, &"{PREV-CONSOLE}".to_string())
            .unwrap();
        startup
            .set_value(LAST_TERMINAL, &"{PREV-TERMINAL}".to_string())
            .unwrap();

        let restored = crate::termhost::registration::restore_delegation_backup().unwrap();
        assert!(restored);

        // Re-open key — restore writes through register_termhost_with
        let startup = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_READ).unwrap();
        let console: String = startup.get_value(DELEGATION_CONSOLE).unwrap();
        assert_eq!(console, "{PREV-CONSOLE}");
        let terminal: String = startup.get_value(DELEGATION_TERMINAL).unwrap();
        assert_eq!(terminal, "{PREV-TERMINAL}");

        let console_result: Result<String, _> = startup.get_value(LAST_CONSOLE);
        assert!(console_result.is_err());

        cleanup_backup_values();
    }

    /// Verify restore_delegation_backup skips restore when current ≠ WezTerm.
    #[test]
    fn interloper_protection() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();

        // Set up: another host is default
        startup
            .set_value(DELEGATION_CONSOLE, &"{OTHER-HOST}".to_string())
            .unwrap();
        startup
            .set_value(DELEGATION_TERMINAL, &"{OTHER-TERMINAL}".to_string())
            .unwrap();
        startup
            .set_value(LAST_CONSOLE, &"{SAVED-CONSOLE}".to_string())
            .unwrap();
        startup
            .set_value(LAST_TERMINAL, &"{SAVED-TERMINAL}".to_string())
            .unwrap();

        let restored = crate::termhost::registration::restore_delegation_backup().unwrap();
        assert!(!restored);

        // Verify DelegationConsole/Terminal were NOT changed
        let console: String = startup.get_value(DELEGATION_CONSOLE).unwrap();
        assert_eq!(console, "{OTHER-HOST}");

        // Verify backup values WERE cleared (always cleaned up)
        let result: Result<String, _> = startup.get_value(LAST_CONSOLE);
        assert!(result.is_err());

        cleanup_backup_values();
    }

    /// Verify restore skips when only DelegationTerminal is WezTerm's (partial state).
    #[test]
    fn interloper_protection_console_only_changed() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();

        startup
            .set_value(DELEGATION_CONSOLE, &"{OTHER-CONSOLE}".to_string())
            .unwrap();
        startup
            .set_value(
                DELEGATION_TERMINAL,
                &crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID.to_string(),
            )
            .unwrap();
        startup
            .set_value(LAST_CONSOLE, &"{SAVED-CONSOLE}".to_string())
            .unwrap();
        startup
            .set_value(LAST_TERMINAL, &"{SAVED-TERMINAL}".to_string())
            .unwrap();

        let restored = crate::termhost::registration::restore_delegation_backup().unwrap();
        assert!(!restored);

        let startup = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_READ).unwrap();
        let console: String = startup.get_value(DELEGATION_CONSOLE).unwrap();
        assert_eq!(console, "{OTHER-CONSOLE}");
        let terminal: String = startup.get_value(DELEGATION_TERMINAL).unwrap();
        assert_eq!(terminal, crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID);

        cleanup_backup_values();
    }

    /// Verify restore_delegation_backup uses null GUID when backup is missing.
    #[test]
    fn missing_backup_restores_null_guid() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();

        // Both Delegation* values must be set so current_registration()
        // returns Some (partial state would return None and skip restore).
        startup
            .set_value(
                DELEGATION_CONSOLE,
                &crate::termhost::WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID.to_string(),
            )
            .unwrap();
        startup
            .set_value(
                DELEGATION_TERMINAL,
                &crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID.to_string(),
            )
            .unwrap();

        let restored = crate::termhost::registration::restore_delegation_backup().unwrap();
        assert!(restored);

        // Re-open key — restore writes through register_termhost_with
        let startup = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_READ).unwrap();
        let console: String = startup.get_value(DELEGATION_CONSOLE).unwrap();
        assert_eq!(console, "{00000000-0000-0000-0000-000000000000}");

        cleanup_backup_values();
    }

    /// Verify clear_delegation_backup removes both values.
    #[test]
    fn clear_removes_both_values() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();

        startup
            .set_value(LAST_CONSOLE, &"{TEST}".to_string())
            .unwrap();
        startup
            .set_value(LAST_TERMINAL, &"{TEST}".to_string())
            .unwrap();

        crate::termhost::registration::clear_delegation_backup().unwrap();

        assert!(startup.get_value::<String, _>(LAST_CONSOLE).is_err());
        assert!(startup.get_value::<String, _>(LAST_TERMINAL).is_err());

        cleanup_backup_values();
    }
}

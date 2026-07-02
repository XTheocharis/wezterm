use crate::termhost::{key_is_wezterm_owned, TERMHOST_HANDOFF_IIDS, WEZTERM_PROXY_STUB_CLSID};

pub struct DisableCommand {}

impl DisableCommand {
    pub fn run() -> anyhow::Result<()> {
        // Restore first but don't abort on error: the unregistrations
        // below are ownership-checked and safe regardless of Delegation
        // state, and leaving COM entries in the registry is the worse
        // failure mode for a `disable` command. Propagate the restore
        // error after cleanup so the user still sees what went wrong.
        let restore_result = crate::termhost::registration::restore_delegation_backup();
        match &restore_result {
            Ok(true) => println!("Restored previous default terminal selection."),
            Ok(false) => println!(
                "Current default is not WezTerm; leaving DelegationConsole/DelegationTerminal unchanged."
            ),
            // eprintln (not log::error!) so the user sees this even when
            // the logger isn't initialized — CLI subcommands don't set up
            // env_logger before dispatching.
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

/// Open `path` under `parent` and check WezTerm ownership. Returns false
/// on `NotFound` (key absent — legitimately not ours). Logs and returns
/// false on other errors (permission denied, corrupted hive): we skip
/// deletion rather than guessing, since deleting a foreign entry would
/// break the other application's defterm registration.
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
    use std::time::{SystemTime, UNIX_EPOCH};
    use winreg::enums::*;
    use winreg::RegKey;

    const STARTUP_KEY: &str = "Console\\%%Startup";
    const DELEGATION_CONSOLE: &str = "DelegationConsole";
    const DELEGATION_TERMINAL: &str = "DelegationTerminal";
    const LAST_CONSOLE: &str = "WezTerm_Last_Console";
    const LAST_TERMINAL: &str = "WezTerm_Last_Terminal";

    #[derive(Default)]
    struct StartupValues {
        delegation_console: Option<String>,
        delegation_terminal: Option<String>,
        last_console: Option<String>,
        last_terminal: Option<String>,
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

    struct StartupKeyGuard {
        existed: bool,
        values: StartupValues,
    }

    impl StartupKeyGuard {
        fn capture() -> Self {
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

    // === Ported from unregister.rs (ownership-check tests) ===

    struct TestClsid {
        clsid: String,
    }

    impl TestClsid {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let clsid = format!("{{WEZTERM-TEST-{}-{}-{}}}", std::process::id(), name, nanos);
            cleanup_clsid(&clsid);
            Self { clsid }
        }

        fn key_path(&self) -> String {
            crate::termhost::registration::clsid_registry_path(&self.clsid)
        }
    }

    impl Drop for TestClsid {
        fn drop(&mut self) {
            cleanup_clsid(&self.clsid);
        }
    }

    fn cleanup_clsid(clsid: &str) {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key_path = crate::termhost::registration::clsid_registry_path(clsid);
        if let Ok(clsid_key) = hkcu.open_subkey_with_flags(&key_path, KEY_WRITE) {
            let _ = clsid_key.delete_subkey("LocalServer32");
        }
        let _ = hkcu.delete_subkey(&key_path);
    }

    fn cleanup_backup_values() {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(startup) = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_WRITE) {
            let _ = startup.delete_value(LAST_CONSOLE);
            let _ = startup.delete_value(LAST_TERMINAL);
        }
    }

    /// Ported: unowned CLSID entries must NOT be removed.
    #[test]
    fn unregister_local_server_preserves_unowned_clsid() {
        let test_key = TestClsid::new("unowned");
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
        let test_key = TestClsid::new("owned");
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

    // === New tests for backup/restore behavior ===

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

        // Set up: WezTerm is default, backup has previous values.
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

        // WezTerm is default, but no backup exists.
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

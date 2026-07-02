pub struct EnableCommand {}

impl EnableCommand {
    pub fn run() -> anyhow::Result<()> {
        crate::termhost::registration::capture_delegation_backup()?;
        register_local_server_for_unpackaged()?;
        register_openconsole_fallback();
        register_proxy_stub_per_user();
        crate::termhost::register_termhost()?;
        println!(
            "WezTerm is now the Windows default terminal.\n\
             \n\
             Terminal CLSID : {}\n\
             Console CLSID  : {} (Microsoft OpenConsole.exe; bundled copy registered as fallback)\n\
             Registry key   : HKCU\\Console\\%%Startup\\DelegationConsole, DelegationTerminal",
            crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID,
            crate::termhost::WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID
        );
        println!(
            "\nRegistered HKCU\\Software\\Classes\\CLSID\\{}\\LocalServer32 -> wezterm-gui.exe.",
            crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID
        );
        Ok(())
    }
}

fn register_local_server_for_unpackaged() -> anyhow::Result<()> {
    let exe_path = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("determining wezterm-gui.exe path: {}", e))?;

    let clsid = crate::termhost::WEZTERM_TERMHOST_TERMINAL_CLSID;
    register_local_server_for_clsid(clsid, &exe_path)
}

fn register_local_server_for_clsid(clsid: &str, exe_path: &std::path::Path) -> anyhow::Result<()> {
    use winreg::enums::*;
    use winreg::RegKey;

    let exe_str = exe_path.to_string_lossy();
    let key_path = crate::termhost::registration::clsid_registry_path(clsid);

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(&key_path)
        .map_err(|e| anyhow::anyhow!("creating HKCU\\{}: {}", key_path, e))?;
    key.set_value("", &"WezTerm Default Terminal Handoff")
        .map_err(|e| anyhow::anyhow!("writing default value: {}", e))?;

    let (local_server, _) = key
        .create_subkey("LocalServer32")
        .map_err(|e| anyhow::anyhow!("creating LocalServer32: {}", e))?;
    let cmd_line = format!("\"{}\"", exe_str);
    local_server
        .set_value("", &cmd_line)
        .map_err(|e| anyhow::anyhow!("writing LocalServer32 value: {}", e))?;
    key.set_value(crate::termhost::WEZTERM_OWNED_VALUE, &1u32)
        .map_err(|e| anyhow::anyhow!("writing WezTermOwned marker: {}", e))?;

    Ok(())
}

fn register_openconsole_fallback() {
    if let Err(e) = crate::termhost::register_openconsole_fallback() {
        eprintln!("warning: OpenConsole fallback registration skipped: {}", e);
    } else if crate::termhost::resolve_bundled_openconsole_path().is_none() {
        eprintln!(
            "warning: bundled OpenConsole.exe not found. \
                 Console launches will crash with 0xc0000142 until \
                 OpenConsole.exe is available next to wezterm-gui.exe."
        );
    }
}

fn register_proxy_stub_per_user() {
    if let Some(dll) = crate::termhost::resolve_proxy_stub_dll_path() {
        if let Err(e) = crate::termhost::register_proxy_stub_per_user(&dll) {
            eprintln!("warning: proxy/stub registration skipped: {}", e);
        }
    } else if !crate::termhost::is_wt_installed() {
        eprintln!(
            "note: proxy/stub DLL not found next to wezterm-gui.exe; \
                 skipping per-user registration. The DLL is bundled in \
                 assets/windows/conhost/ and copied by the build."
        );
    } else {
        eprintln!(
            "note: proxy/stub DLL not found; relying on Windows Terminal's \
             MSIX-packaged proxy stub. If defterm does not work, install \
             OpenConsoleProxy.dll next to wezterm-gui.exe."
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

    struct TestClsid {
        clsid: String,
    }

    impl TestClsid {
        fn new() -> Self {
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

    /// Verify that capture_delegation_backup skips when WezTerm is already default
    /// (idempotency — doesn't overwrite the original pre-WezTerm capture).
    #[test]
    fn capture_skips_when_wezterm_already_default() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();

        // Set Delegation* to WezTerm's CLSIDs (simulating already-default).
        // Both values must be set so current_registration() returns Some,
        // which is required for the idempotency guard to fire.
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
        // Write a stale backup value that should NOT be overwritten
        startup
            .set_value(LAST_TERMINAL, &"{OLD-VALUE}".to_string())
            .unwrap();

        crate::termhost::registration::capture_delegation_backup().unwrap();

        // Re-open key — the function may have recreated it internally
        let startup = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_READ).unwrap();
        let last: String = startup.get_value(LAST_TERMINAL).unwrap();
        assert_eq!(last, "{OLD-VALUE}");
    }

    /// Verify that capture_delegation_backup writes when another host is default.
    #[test]
    fn capture_writes_when_other_default() {
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
            .set_value(DELEGATION_TERMINAL, &"{OTHER-TERMINAL}".to_string())
            .unwrap();

        crate::termhost::registration::capture_delegation_backup().unwrap();

        let console: String = startup.get_value(LAST_CONSOLE).unwrap();
        let terminal: String = startup.get_value(LAST_TERMINAL).unwrap();
        assert_eq!(console, "{OTHER-CONSOLE}");
        assert_eq!(terminal, "{OTHER-TERMINAL}");
    }

    /// Verify that capture_delegation_backup writes null GUID when no default exists.
    #[test]
    fn capture_writes_null_guid_when_no_default() {
        let _guard = crate::termhost::registration::BACKUP_TEST_GUARD
            .lock()
            .unwrap();
        let _startup_guard = StartupKeyGuard::capture();
        cleanup_backup_values();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (startup, _) = hkcu.create_subkey(STARTUP_KEY).unwrap();
        let _ = startup.delete_value(DELEGATION_CONSOLE);
        let _ = startup.delete_value(DELEGATION_TERMINAL);

        crate::termhost::registration::capture_delegation_backup().unwrap();

        // Re-open key — capture created it fresh after we deleted it
        let startup = hkcu.open_subkey_with_flags(STARTUP_KEY, KEY_READ).unwrap();
        let console: String = startup.get_value(LAST_CONSOLE).unwrap();
        let terminal: String = startup.get_value(LAST_TERMINAL).unwrap();
        assert_eq!(console, "{00000000-0000-0000-0000-000000000000}");
        assert_eq!(terminal, "{00000000-0000-0000-0000-000000000000}");
    }

    /// Verify that local-server registration creates the expected registry
    /// entries with WezTerm ownership marker.
    #[test]
    fn enable_registers_local_server() {
        let test_key = TestClsid::new();
        let exe_path = std::env::current_exe().unwrap();
        register_local_server_for_clsid(&test_key.clsid, &exe_path).unwrap();

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key_path = test_key.key_path();
        let key = hkcu.open_subkey_with_flags(&key_path, KEY_READ).unwrap();

        let name: String = key.get_value("").unwrap();
        assert!(name.contains("WezTerm"));

        let owned: u32 = key.get_value(crate::termhost::WEZTERM_OWNED_VALUE).unwrap();
        assert_eq!(owned, 1u32);

        let local_server = key.open_subkey("LocalServer32").unwrap();
        let exe: String = local_server.get_value("").unwrap();
        assert_eq!(exe, format!("\"{}\"", exe_path.to_string_lossy()));
    }
}

---
tags:
  - windows
---

## Windows Default Terminal

{{since('nightly')}}

Starting with Windows 11 22H2 and Windows 10 (build 19044 or later,
with the defterm update applied), Windows can redirect console sessions
to a third-party terminal application selected by the user. When a
console application is launched — from Explorer, the Start Menu, the
Run dialog, or a script — Windows opens it in the chosen terminal
instead of the bare console window.

Microsoft ships `WindowsTerminal.exe` to fill this role. With this
feature, WezTerm can fill the same role.

## Requirements

This feature is only available on Windows. You need one of:

* Windows 11 22H2 or later
* Windows 10, build 19044 or later (with KB5026435 or equivalent update applied)

No extra build flags or cargo features are required. The bundled
`OpenConsole.exe` and `OpenConsoleProxy.dll` (from Microsoft's Windows
Terminal, MIT-licensed) are copied next to `wezterm-gui.exe` at build
time. See [Building from source](../install/source.md) for Windows
build instructions.

## Enabling WezTerm as the default terminal

```console
> wezterm terminal-host enable
```

This registers WezTerm with Windows, sets it as the default terminal,
and saves the previous selection so it can be restored later. After
enabling, console applications open in WezTerm.

The previous selection is stored as sibling values
(`WezTerm_Last_Console`, `WezTerm_Last_Terminal`) alongside the
`DelegationConsole` / `DelegationTerminal` values that Windows reads.

!!! note
    When no other host is registered for the Microsoft OpenConsole CLSID,
    `enable` also registers the bundled `OpenConsole.exe` as a fallback
    ConPTY host under that CLSID. ConPTY is Windows' pseudo-terminal
    implementation — the component that bridges between the console API
    and the actual terminal emulator. Without this fallback, console
    application launches would fail with `0xc0000142`
    (`STATUS_DLL_INIT_FAILED`).

## Disabling (restoring the previous default)

```console
> wezterm terminal-host disable
```

This restores the previous default terminal selection (or resets to
"Let Windows decide" if no prior default was captured), removes
WezTerm's COM registrations, and cleans up the backup values.

If you switched to a different terminal via Windows Settings between
`enable` and `disable`, `disable` will **not** overwrite your choice —
it leaves the current selection unchanged and only removes WezTerm's
own entries.

## Verifying the registration

You can inspect the Windows registry to confirm the registration:

```console
> reg query "HKCU\Console\%%Startup"

HKEY_CURRENT_USER\Console\%%Startup
    DelegationConsole    REG_SZ    {2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}
    DelegationTerminal    REG_SZ    {8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}

> reg query "HKCU\Software\Classes\CLSID\{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}\LocalServer32"

HKEY_CURRENT_USER\Software\Classes\CLSID\{8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}\LocalServer32
    (Default)    REG_SZ    "C:\Program Files\WezTerm\wezterm-gui.exe"
```

## How it works

When a console application is launched, Windows starts `conhost.exe`
(the built-in console host), which reads two values from
`HKCU\Console\%%Startup`:

| Value                | Purpose                                                |
|----------------------|--------------------------------------------------------|
| `DelegationConsole`  | CLSID of the application that hosts the ConPTY layer  |
| `DelegationTerminal` | CLSID of the application that provides the terminal UI |

WezTerm registers itself under `DelegationTerminal`. The ConPTY side
(`DelegationConsole`) is satisfied either by an installed Windows
Terminal, or by the bundled `OpenConsole.exe` registered as a fallback
when `enable` runs.

If no WezTerm process is running, Windows launches `wezterm-gui.exe`
with an `-Embedding` flag. WezTerm strips the flag, registers itself
as a COM server, and waits for the handoff callback. The callback
delivers the console session's pipe handles, which WezTerm wraps as a
PTY and attaches to a new tab in a new window.

## Honored startup hints

When a console application is launched, the launcher can pass
[`STARTUPINFO`](https://learn.microsoft.com/en-us/windows/win32/api/processthreads/ns-processthreads-startupinfoa)
fields describing how the new console was expected to look. WezTerm
receives these through the handoff and applies a subset to the new
window.

| Field                       | Effect                                                |
|-----------------------------|-------------------------------------------------------|
| `wShowWindow`               | `SW_SHOWMAXIMIZED` opens the window maximized         |
| `dwX`, `dwY`                | Initial window position, in pixels; only applied when `dwFlags` includes `STARTF_USEPOSITION` |

Other fields are ignored. In particular, `dwXSize` and `dwYSize` have
no effect (WezTerm sizes windows by cell count, not pixels),
`dwFillAttribute` is a legacy console attribute that does not map to
anything in WezTerm, and `pszIconPath`/`iconIndex` are dropped (WezTerm
always uses its own application icon).

`SW_HIDE` and `SW_SHOWMINIMIZED` are filtered out by `conhost.exe`
before the handoff is attempted, so WezTerm never sees them.

## Fallback on handoff failure

If the handoff reaches WezTerm but attaching the incoming PTY fails
after the pipe handles have been delivered, WezTerm spawns a new tab
using the default profile instead. This ensures the user still gets a
usable window rather than a stuck process.

## Troubleshooting

To inspect the current default terminal selection and WezTerm's backup
values:

```console
> reg query "HKCU\Console\%%Startup"

HKEY_CURRENT_USER\Console\%%Startup
    DelegationConsole    REG_SZ    {2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}
    DelegationTerminal   REG_SZ    {8B7D4E2A-3F5C-4D1B-9A6E-7C2B5F8D1E4A}
    WezTerm_Last_Console    REG_SZ    (previous value or null GUID)
    WezTerm_Last_Terminal   REG_SZ    (previous value or null GUID)
```

To check whether Windows Terminal is installed (MSIX-packaged builds
are not visible in the classic registry):

```powershell
Get-AppxPackage | Where-Object Name -match 'Terminal'
```

## See also

* [Microsoft Default Terminal spec (#492)](https://github.com/microsoft/terminal/blob/main/doc/specs/%23492%20-%20Default%20Terminal/spec.md)
* [`ITerminalHandoff.idl`](https://github.com/microsoft/terminal/blob/main/src/host/proxy/ITerminalHandoff.idl)
* [Installing on Windows](../install/windows.md)

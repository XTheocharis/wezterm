---
tags:
  - windows
---

## Windows Default Terminal

{{since('nightly')}}

Starting with Windows 11 22H2 and Windows 10 (build 19044 or later,
with the defterm update applied), the inbox `conhost.exe` can delegate
an incoming console session to an out-of-process COM server selected
by the user.
Microsoft ships `WindowsTerminal.exe` to fill that role; wezterm can fill
the same role, so launching a console application from the Start Menu,
Explorer, or the `Run` dialog opens a wezterm window instead of a bare
console window.

## Requirements

This feature is only available on Windows.  You need one of:

* Windows 11 22H2 or later
* Windows 10, build 19044 or later (with KB5026435 or equivalent update applied)

No extra build flags or cargo features are required.  The bundled
`OpenConsole.exe` and `OpenConsoleProxy.dll` (from Microsoft's Windows
Terminal, MIT-licensed) are copied next to `wezterm-gui.exe` at build
time.  See [Building from source](../install/source.md) for Windows
build instructions.

## Enabling WezTerm as the default terminal

```console
> wezterm terminal-host enable
```

This captures the current default terminal selection (so it can be
restored later), registers wezterm-gui.exe as the local COM server
for the handoff, and sets wezterm as the Windows default terminal.
After enabling, console applications open in wezterm.

The previous default terminal selection is stored under
`HKCU\Console\%%Startup` as sibling `REG_SZ` values
(`WezTerm_Last_Console`, `WezTerm_Last_Terminal`) alongside the
canonical `DelegationConsole` / `DelegationTerminal` values that
Windows reads.

!!! note
    On machines that don't have Windows Terminal installed, `enable`
    also registers the bundled `OpenConsole.exe` under the Microsoft
    OpenConsole CLSID as a fallback ConPTY host.  Without that fallback,
    console application launches would fail with `0xc0000142`
    (`STATUS_DLL_INIT_FAILED`).

## Disabling (restoring the previous default)

```console
> wezterm terminal-host disable
```

This restores the default terminal selection captured at `enable`
time (or resets to "Let Windows decide" if no prior default was
captured), removes wezterm's COM class registrations, and cleans up
the backup values.

If you switched to a different terminal via Windows Settings between
`enable` and `disable`, `disable` will **not** overwrite your choice —
it leaves `DelegationConsole` / `DelegationTerminal` unchanged and only
removes wezterm's COM entries. This prevents silently clobbering a
deliberate selection you made outside wezterm.

## Verifying the registration

You can inspect the registry directly to confirm the registration:

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

When a console application is launched, Windows boots `conhost.exe`,
which reads two `REG_SZ` values from `HKCU\Console\%%Startup`:

| Value                | Purpose                                                |
|----------------------|--------------------------------------------------------|
| `DelegationConsole`  | CLSID of the COM server that hosts the ConPTY          |
| `DelegationTerminal` | CLSID of the COM server that provides the terminal UX  |

WezTerm registers itself under `DelegationTerminal`.  The ConPTY side
(`DelegationConsole`) is satisfied either by an installed Windows
Terminal, or by the bundled `OpenConsole.exe` registered as a fallback
when `enable` runs.

When no wezterm is running, the COM Service Control Manager launches
`wezterm-gui.exe` with an `-Embedding` flag.  WezTerm strips the flag,
registers the termhost COM class, and waits for the handoff callback.
The incoming PTY handles are then attached to a new tab in a new window
via the normal mux machinery.

## Honored startup hints

When a console application is launched, the launcher can pass
[`STARTUPINFO`](https://learn.microsoft.com/en-us/windows/win32/api/processthreads/ns-processthreads-startupinfoa)
fields describing how the new console was expected to look.  WezTerm
receives these through the handoff and applies a subset to the new
window.

| Field                       | Effect                                                |
|-----------------------------|-------------------------------------------------------|
| `wShowWindow`               | `SW_SHOWMAXIMIZED` opens the window maximized         |
| `dwX`, `dwY`                | Initial window position, in pixels                    |
| `pszIconPath`, `iconIndex`  | Window icon loaded from the originating executable    |

Other fields are ignored.  In particular, `dwXSize` and `dwYSize` have
no effect (wezterm sizes windows by cell count, not pixels), and
`dwFillAttribute` is a legacy console attribute that does not map to
anything in wezterm.

`SW_HIDE` and `SW_SHOWMINIMIZED` are filtered out by `conhost.exe`
before the handoff is attempted, so wezterm never sees them.

## Fallback on handoff failure

If the handoff reaches wezterm but attaching the incoming PTY fails
after the pipe handles have been delivered, wezterm spawns a new tab
using the default profile instead.  This ensures the user still gets a
usable window rather than a stuck process.

## Troubleshooting

To inspect the current default terminal selection and wezterm's backup
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

# Console Host

This directory contains a copy of built artifacts from the Microsoft
Terminal project which is provided by Microsoft under the terms
of the MIT license.

Why are they here?  At the time of writing, the conpty implementation
that ships with windows is lacking support for mouse reporting but
that support is available in the opensource project so it is desirable
to point to that so that we can enable mouse reporting in wezterm.

It looks like we'll eventually be able to drop this once Windows
and/or the build for the terminal project make some more progress.

https://github.com/wezterm/wezterm/issues/1927

These binaries also serve a second purpose: the Default Terminal feature
(see `docs/config/terminal-host.md`) requires `OpenConsole.exe` as a ConPTY
host and `OpenConsoleProxy.dll` as a COM marshalling stub. Both are
included here.

## What's included

| File | Purpose |
|---|---|
| `OpenConsole.exe` | Updated console host (replaces the inbox `conhost.exe`) |
| `conpty.dll` | ConPTY client library (matches `OpenConsole.exe` version) |
| `OpenConsoleProxy.dll` | COM marshalling stub for the defterm handoff protocol |

These assets were built by cloning the ms-terminal repo and running:

```
.\tools\razzle.cmd
bcz rel
```

then the files can be copied from `bin/x64/Release` to this location.

## Updating the binaries

**From GitHub releases** (recommended): run
`./assets/windows/conhost/update-fetch.sh` to download the latest
signed release binaries from microsoft/terminal.

**From source**: run `./assets/windows/conhost/update-build.ps1` on a
Windows host with Visual Studio 2022 (VCTools workload, VC.Tools.x86.x64,
Windows11SDK.26100, VC.ATL). The script clones microsoft/terminal at the
pinned tag, restores NuGet packages, and builds the 3 target projects
with `WindowsTerminalBranding=Release` so that `OpenConsoleProxy.dll`
embeds the correct CLSID.

It's possible that you'll need to download this runtime support package
from MS in order for this to work:
https://www.microsoft.com/en-us/download/details.aspx?id=53175

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

The bundled binaries in this directory are the official signed Microsoft
Windows Terminal release binaries.  They were downloaded from the release
zip/nupkg by running `./assets/windows/conhost/update-fetch.sh`; they were
not built from source by the WezTerm project.

To build equivalent assets from source instead, run
`./assets/windows/conhost/update-build.ps1` on a Windows host with
Visual Studio 2022 (VCTools workload, VC.Tools.x86.x64, Windows11SDK.26100,
VC.ATL). The script clones microsoft/terminal at the pinned tag, restores
NuGet packages, and builds the 3 target projects with
`WindowsTerminalBranding=Release` so that OpenConsoleProxy.dll embeds the
WezTerm-compatible CLSID. Run `pwsh -File update-build.ps1 -CopyPdb` to
also copy debug symbols.

It's possible that you'll need to download this runtime support package
from MS in order for this to work:
https://www.microsoft.com/en-us/download/details.aspx?id=53175

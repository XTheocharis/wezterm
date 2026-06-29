#Requires -Version 7.0
<#
.SYNOPSIS
    End-to-end build of OpenConsole.exe + conpty.dll + OpenConsoleProxy.dll
    from microsoft/terminal source, producing WezTerm-compatible binaries.

.DESCRIPTION
    Clones microsoft/terminal at the requested tag, restores NuGet packages,
    and builds 3 target vcxproj with WindowsTerminalBranding=Release so the
    OpenConsoleProxy.dll embeds the WezTerm-compatible CLSID
    {3171DE52-6EFA-4AEF-8A9F-D02BD67E7A4F}.

    Requires Visual Studio 2022+ with: VCTools workload, VC.Tools.x86.x64,
    Windows11SDK.26100, VC.ATL. Idempotent: skips clone when the source tree
    is already at the requested tag.

.PARAMETER OutputDir
    Destination directory for the 3 built artifacts. Default: ./output

.PARAMETER Tag
    microsoft/terminal git tag to build. Default: v1.24.11321.0

.PARAMETER Platform
    Build platform. Default: x64. Also accepted: x86, ARM64 (untested).

.PARAMETER Configuration
    Build configuration. Default: Release. Also accepted: Debug.

.PARAMETER VsRange
    vswhere version range for VS detection. Default: '[17.0,19.0)'
    (matches VS 2022 + VS 2026 Insiders).

.PARAMETER CopyPdb
    Also copy *.pdb alongside the binaries if present.

.EXAMPLE
    pwsh -File build-ms-terminal.ps1
    pwsh -File build-ms-terminal.ps1 -OutputDir D:\releases\2026-06-30
    pwsh -File build-ms-terminal.ps1 -Tag v1.25.11111.0 -CopyPdb
#>
[CmdletBinding()]
param(
    [string]$OutputDir,
    [string]$Tag = 'v1.24.11321.0',
    [ValidateSet('x64', 'x86', 'ARM64')]
    [string]$Platform = 'x64',
    [ValidateSet('Release', 'Debug')]
    [string]$Configuration = 'Release',
    [string]$VsRange = '[17.0,19.0)',
    [switch]$CopyPdb
)
$ErrorActionPreference = 'Stop'

# --- Config -----------------------------------------------------------------
$Root       = $PSScriptRoot
$Src        = Join-Path $Root 'terminal'
if (-not $OutputDir) { $OutputDir = Join-Path $Root 'output' }
$Vswhere    = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$Required   = 'Microsoft.VisualStudio.Workload.NativeDesktop',
              'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
              'Microsoft.VisualStudio.Component.Windows11SDK.26100',
              'Microsoft.VisualStudio.Component.VC.ATL'
$TotalSteps = 6

# MS GUID {3171DE52-6EFA-4AEF-8A9F-D02BD67E7A4F} in mixed-endian byte order
$ClsidNeedle = [byte[]](0x52, 0xDE, 0x71, 0x31, 0xFA, 0x6E, 0xEF, 0x4A,
                        0x8A, 0x9F, 0xD0, 0x2B, 0xD6, 0x7E, 0x7A, 0x4F)
$ClsidLabel  = '{3171DE52-6EFA-4AEF-8A9F-D02BD67E7A4F}'

function Step ($n, $msg) { Write-Host "`n=== [$n/$TotalSteps] $msg ===" -ForegroundColor Cyan }

# --- [1] Find Visual Studio -------------------------------------------------
Step 1 'Locating Visual Studio'
if (-not (Test-Path $Vswhere)) { throw "vswhere.exe not found at: $Vswhere" }
# .TrimEnd() guards against the \r leak on Windows PowerShell 5.x (which we no
# longer target, but the call is harmless on PS 7+ and protects against regressions).
$Vs = & $Vswhere -version $VsRange -products * -requires $Required `
       -property installationPath | Select-Object -First 1
if (-not $Vs) { throw "No VS in range $VsRange with required components: $($Required -join ', ')" }
$Vs = $Vs.TrimEnd()
$Msbuild = Join-Path $Vs 'MSBuild\Current\Bin\MSBuild.exe'
if (-not (Test-Path $Msbuild)) { throw "MSBuild not found: $Msbuild" }
Write-Host "  VS:       $Vs`n  MSBuild:  $Msbuild"

# --- [2] Clone + submodules -------------------------------------------------
Step 2 "Cloning microsoft/terminal @ $Tag"
if (Test-Path $Src) {
    if (-not (Test-Path (Join-Path $Src '.git'))) {
        # O1: partial-clone recovery — $Src left behind by a failed clone breaks
        # the next run because git clone refuses non-empty destinations.
        throw "$Src exists without .git (partial clone?). Remove $Src and re-run."
    }
    # O2 + S3: verify HEAD is at the requested tag so we never silently build
    # the wrong version. We do not auto-fetch a new tag into a shallow clone —
    # delete $Src and re-run to switch tags cleanly.
    $headTag = & git -C $Src describe --tags --exact-match HEAD 2>$null
    if ($null -eq $headTag) {
        throw "Source tree HEAD is not at a tag. Delete $Src and re-run."
    }
    if ($headTag -ne $Tag) {
        throw "Source tree at '$headTag', need '$Tag'. Delete $Src and re-run."
    }
    Write-Host "  Source tree at $Tag, skipping clone."
} else {
    git clone --branch $Tag --depth 1 https://github.com/microsoft/terminal.git $Src
    if ($LASTEXITCODE) { throw 'git clone failed' }
    # git -C avoids Push-Location entirely for git-side operations.
    git -C $Src submodule update --init --recursive --depth 1
    if ($LASTEXITCODE) { throw 'git submodule update failed' }
}

# --- [3] NuGet restore ------------------------------------------------------
Step 3 'Restoring NuGet packages'
$NugetDir = Join-Path $Src 'dep\nuget'
$Nuget    = Join-Path $NugetDir 'nuget.exe'
if (-not (Test-Path $Nuget)) { throw "$Nuget missing - submodule init failed" }
& $Nuget restore (Join-Path $NugetDir 'packages.config') `
    -PackagesDirectory (Join-Path $Src 'packages') -Verbosity quiet
if ($LASTEXITCODE) { throw 'nuget restore failed' }

# --- [4] Build 3 target vcxproj ---------------------------------------------
Step 4 "Building 3 target projects ($Configuration $Platform, WindowsTerminalBranding=Release)"
# Forward slash for SolutionDir avoids the MSBuild arg-parsing bug when $Src
# contains spaces: PS auto-quotes args with spaces, and a literal backslash
# before the implicit closing quote escapes the quote (MSBuild #3964). MSBuild
# and Windows both tolerate forward slashes in paths.
$Common = '/t:Rebuild',
          "/p:Configuration=$Configuration",
          "/p:Platform=$Platform",
          "/p:SolutionDir=$Src/",
          '/p:WindowsTerminalBranding=Release',
          '/p:GenerateAppxPackageOnBuild=false',
          '/m', '/nologo', '/verbosity:minimal'

$Targets = @{
    'OpenConsoleProxy.dll' = 'src\host\proxy\Host.Proxy.vcxproj'
    'conpty.dll'           = 'src\winconpty\dll\winconptydll.vcxproj'
    'OpenConsole.exe'      = 'src\host\exe\Host.EXE.vcxproj'
}

$i = 0
foreach ($name in $Targets.Keys) {
    $i++
    Write-Host "`n--- Build $i/$($Targets.Count): $name ---"
    # S1: try/finally preserves the location stack if MSBuild ever fails to start.
    Push-Location $Src
    try {
        & $Msbuild $Targets[$name] @Common
        $rc = $LASTEXITCODE
    } finally { Pop-Location }
    if ($rc) { throw "Build failed: $name (exit $rc)" }
}

# --- [5] Verify + CLSID check -----------------------------------------------
Step 5 'Verifying outputs'
$Bin = Join-Path $Src "bin\$Platform\$Configuration"
foreach ($n in $Targets.Keys) {
    if (-not (Test-Path (Join-Path $Bin $n))) { throw "Missing: $Bin\$n" }
}

# E1: wrong CLSID = broken COM registration. This is a hard error, not a warning.
$proxy = Join-Path $Bin 'OpenConsoleProxy.dll'
$bytes = [IO.File]::ReadAllBytes($proxy)
$found = -1
for ($idx = 0; $idx -le $bytes.Length - $ClsidNeedle.Length; $idx++) {
    for ($j = 0; $j -lt $ClsidNeedle.Length -and $bytes[$idx + $j] -eq $ClsidNeedle[$j]; $j++) { }
    if ($j -eq $ClsidNeedle.Length) { $found = $idx; break }
}
if ($found -lt 0) {
    throw "CLSID $ClsidLabel NOT FOUND in $proxy. WindowsTerminalBranding=Release not honored; DLL is unusable."
}
Write-Host ("  CLSID: FOUND WezTerm {0} at offset 0x{1:X}" -f $ClsidLabel, $found)

# --- [6] Copy ---------------------------------------------------------------
Step 6 "Copying artifacts to $OutputDir"
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
foreach ($n in $Targets.Keys) {
    Copy-Item (Join-Path $Bin $n) $OutputDir -Force
    if ($CopyPdb) {
        $pdb = Join-Path $Bin ([IO.Path]::GetFileNameWithoutExtension($n) + '.pdb')
        if (Test-Path $pdb) { Copy-Item $pdb $OutputDir -Force }
    }
}

Write-Host "`n============================================================================"
Write-Host ' SUCCESS - 3 artifacts built' -ForegroundColor Green
# S2: filter to just our outputs so stale files in $OutputDir don't appear.
$built = @($Targets.Keys)
Get-ChildItem $OutputDir -File | Where-Object { $_.Name -in $built } |
    Sort-Object Name |
    ForEach-Object { Write-Host ("  {0,8} bytes  {1}" -f $_.Length, $_.Name) }
Write-Host "============================================================================"

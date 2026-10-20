[CmdletBinding()]
param(
    # Empty builds for the machine's default Rust target. Both choices are
    # x64, which is also what the resource compiler stamps.
    [ValidateSet('', 'x86_64-pc-windows-msvc', 'x86_64-pc-windows-gnu')]
    [string]$Target = '',
    [string]$GoToolchain = 'go1.27.3'
)

# Package the desktop app for Windows: the Rust core as a DLL
# (crates/desktop) and the MyGo window as a Go executable (desktop/), in one
# folder that runs as it is, and a zip beside it.
$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$cargoBin = Join-Path $env:USERPROFILE '.cargo/bin'
if (Test-Path -LiteralPath $cargoBin) {
    $env:Path = "$cargoBin;$env:Path"
}

$buildRoot = Join-Path $repoRoot 'Build'
$packageName = 'VanGoal-windows-x86_64'
$packageDir = Join-Path $buildRoot $packageName
$icon = Join-Path $repoRoot 'assets/VanGoal.ico'
$desktopDir = Join-Path $repoRoot 'desktop'
$syso = Join-Path $desktopDir 'van_goal_windows_amd64.syso'

# windres stamps the executable's icon, manifest and version, which Explorer
# and the taskbar read. Go includes a .syso that sits in the package folder.
# The app sets its window icon from AppIcon.png at run time too, so a machine
# without windres still builds a usable app.
function Find-Windres {
    foreach ($name in @('x86_64-w64-mingw32-windres', 'windres', 'llvm-windres')) {
        $command = Get-Command $name -ErrorAction SilentlyContinue
        if ($command) { return $command.Source }
    }
    foreach ($path in @(
            (Join-Path $env:ProgramFiles 'mingw64/bin/windres.exe'),
            'C:\msys64\mingw64\bin\windres.exe',
            'C:\mingw64\bin\windres.exe')) {
        if (Test-Path -LiteralPath $path) { return $path }
    }
    return $null
}

Push-Location $repoRoot
$resourceDir = $null
# A previous interrupted run must not leave resources that this one does not
# mean to embed.
if (Test-Path -LiteralPath $syso) {
    Remove-Item -LiteralPath $syso -Force
}
try {
    $metadataJson = & cargo metadata --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed.'
    }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    # This is a Cargo workspace, so ask for the desktop package by name; the
    # order of packages is not guaranteed.
    $version = ($metadata.packages | Where-Object name -eq 'van-goal-desktop' | Select-Object -First 1).version
    if (-not $version) {
        throw 'Could not read the van-goal-desktop version from Cargo metadata.'
    }
    $targetArgs = @()
    if ($Target) {
        $targetArgs = @('--target', $Target)
        $targetDir = Join-Path $repoRoot "target/$Target/release"
    } else {
        $targetDir = Join-Path $repoRoot 'target/release'
    }

    $goarch = (& go env GOARCH).Trim()
    if ($goarch -ne 'amd64') {
        throw "This package is x64-only; this Go builds for $goarch."
    }

    New-Item -ItemType Directory -Path $buildRoot -Force | Out-Null
    if (Test-Path -LiteralPath $packageDir) {
        Remove-Item -LiteralPath $packageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $packageDir -Force | Out-Null

    Write-Host 'Building the core for Windows...'
    & cargo build --release -p van-goal-desktop @targetArgs
    if ($LASTEXITCODE -ne 0) {
        throw 'The Windows release build of the core failed.'
    }
    $library = Join-Path $targetDir 'van_goal_desktop.dll'
    if (-not (Test-Path -LiteralPath $library -PathType Leaf)) {
        throw "The release DLL was not produced: $library"
    }

    $windres = Find-Windres
    if ($windres) {
        $resourceDir = Join-Path ([System.IO.Path]::GetTempPath()) ('van-goal-winres-' + [guid]::NewGuid())
        New-Item -ItemType Directory -Path $resourceDir -Force | Out-Null
        $manifest = Join-Path $resourceDir 'VanGoal.manifest'
        $rc = Join-Path $resourceDir 'VanGoal.rc'

        $parts = $version.Split('.')
        while ($parts.Count -lt 4) { $parts += '0' }
        $quadString = $parts[0..3] -join '.'
        $quad = $parts[0..3] -join ','

        # Per-monitor DPI and the common controls v6 manifest MyGo's own build
        # embeds. The app calls SetProcessDpiAwarenessContext too, but the
        # manifest also gives its dialogs the current control style.
        @"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="com.abelliuxl.VanGoal" version="$quadString" processorArchitecture="*"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </windowsSettings>
  </application>
</assembly>
"@ | Set-Content -LiteralPath $manifest -Encoding utf8

        $iconPath = $icon.Replace('\', '/')
        $manifestPath = $manifest.Replace('\', '/')
        @"
1 ICON "$iconPath"
1 24 "$manifestPath"
1 VERSIONINFO
FILEVERSION $quad
PRODUCTVERSION $quad
FILEOS 0x00040004L
FILETYPE 0x00000001L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Abelliuxl"
      VALUE "FileDescription", "Van-Goal"
      VALUE "FileVersion", "$version"
      VALUE "InternalName", "VanGoal"
      VALUE "OriginalFilename", "VanGoal.exe"
      VALUE "ProductName", "Van-Goal"
      VALUE "ProductVersion", "$version"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 0x04B0
  END
END
"@ | Set-Content -LiteralPath $rc -Encoding utf8

        Write-Host 'Embedding the icon, manifest and version...'
        & $windres -O coff -F pe-x86-64 -i $rc -o $syso
        if ($LASTEXITCODE -ne 0) {
            throw 'windres could not compile the Windows resources.'
        }
    } else {
        Write-Warning 'windres was not found: the executable gets no icon resource. The window icon still comes from AppIcon.png.'
    }

    Write-Host 'Building the window...'
    $env:CGO_ENABLED = '0'
    $env:GOTOOLCHAIN = $GoToolchain
    $ldflags = "-s -w -H=windowsgui -X main.version=$version"
    Push-Location $desktopDir
    try {
        & go build -trimpath -tags mygo_noinspector -ldflags $ldflags -o (Join-Path $packageDir 'VanGoal.exe') .
        if ($LASTEXITCODE -ne 0) {
            throw 'The Go build of the window failed.'
        }
    }
    finally {
        Pop-Location
    }
} finally {
    if (Test-Path -LiteralPath $syso) {
        Remove-Item -LiteralPath $syso -Force
    }
    if ($resourceDir -and (Test-Path -LiteralPath $resourceDir)) {
        Remove-Item -LiteralPath $resourceDir -Recurse -Force
    }
    Pop-Location
}

if (-not (Test-Path -LiteralPath (Join-Path $packageDir 'VanGoal.exe'))) {
    throw "The window build did not produce $(Join-Path $packageDir 'VanGoal.exe')."
}

Copy-Item -LiteralPath $library -Destination (Join-Path $packageDir 'van_goal_desktop.dll')
Copy-Item -LiteralPath (Join-Path $repoRoot 'assets/AppIcon.png') -Destination (Join-Path $packageDir 'AppIcon.png')
Copy-Item -LiteralPath $icon -Destination (Join-Path $packageDir 'VanGoal.ico')

@"
Van-Goal $version for Windows x64

Run VanGoal.exe. Keep van_goal_desktop.dll and AppIcon.png next to it: the
executable loads the first and draws the window icon from the second.

Settings, session history, the local cache and the OpenClaw device identity
are saved under %LOCALAPPDATA%\VanGoal.

Remote backends connect to their configured server. Local backends (Codex
CLI, Claude Code, Pi, a managed hermes serve or mimo serve) need their CLI
installed and on PATH.
"@ | Set-Content -LiteralPath (Join-Path $packageDir 'README.txt') -Encoding utf8

$archivePath = Join-Path $buildRoot "$packageName-$version.zip"
if (Test-Path -LiteralPath $archivePath) {
    Remove-Item -LiteralPath $archivePath -Force
}
Compress-Archive -Path (Join-Path $packageDir '*') -DestinationPath $archivePath -CompressionLevel Optimal
Write-Host "Packaged: $archivePath (version $version)"

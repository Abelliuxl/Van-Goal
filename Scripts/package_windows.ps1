[CmdletBinding()]
param(
    [ValidateSet('x86_64-pc-windows-msvc', 'x86_64-pc-windows-gnu')]
    [string]$Target = 'x86_64-pc-windows-msvc'
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$cargoBin = Join-Path $env:USERPROFILE '.cargo/bin'
if (Test-Path -LiteralPath $cargoBin) {
    $env:Path = "$cargoBin;$env:Path"
}

Push-Location $repoRoot
try {
    $metadataJson = & cargo metadata --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata failed.'
    }
    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
    $version = ($metadata.packages | Where-Object name -eq 'van-goal' | Select-Object -First 1).version
    if (-not $version) {
        throw 'Could not read the van-goal package version from Cargo metadata.'
    }

    Write-Host "Building Van-Goal for $Target..."
    & cargo build --release -p van-goal --target $Target
    if ($LASTEXITCODE -ne 0) {
        throw "The Windows release build failed for $Target."
    }

    $buildRoot = Join-Path $repoRoot 'Build'
    $packageName = 'VanGoal-windows-x86_64'
    $packageDir = Join-Path $buildRoot $packageName
    $archivePath = Join-Path $buildRoot "$packageName-$version.zip"
    $binaryPath = Join-Path $repoRoot "target/$Target/release/VanGoal.exe"
    if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
        throw "The release executable was not produced: $binaryPath"
    }

    New-Item -ItemType Directory -Path $buildRoot -Force | Out-Null
    if (Test-Path -LiteralPath $packageDir) {
        Remove-Item -LiteralPath $packageDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $archivePath) {
        Remove-Item -LiteralPath $archivePath -Force
    }
    New-Item -ItemType Directory -Path $packageDir -Force | Out-Null
    Copy-Item -LiteralPath $binaryPath -Destination (Join-Path $packageDir 'VanGoal.exe')
    Copy-Item -LiteralPath (Join-Path $repoRoot 'assets/AppIcon.png') -Destination (Join-Path $packageDir 'AppIcon.png')
    Copy-Item -LiteralPath (Join-Path $repoRoot 'assets/VanGoal.ico') -Destination (Join-Path $packageDir 'VanGoal.ico')

    @"
Van-Goal $version for Windows x64

Extract this folder and run VanGoal.exe. Settings, session history, and the
OpenClaw device identity are saved under %LOCALAPPDATA%\VanGoal.

Remote backends connect to their configured server. Local backends require
their CLI tools to be installed and available on PATH.
"@ | Set-Content -LiteralPath (Join-Path $packageDir 'README.txt') -Encoding utf8

    Compress-Archive -Path (Join-Path $packageDir '*') -DestinationPath $archivePath -CompressionLevel Optimal
    Write-Host "Packaged: $archivePath (version $version, target $Target)"
}
finally {
    Pop-Location
}

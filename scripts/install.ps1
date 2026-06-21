# Install raven-tui from the latest GitHub release into %LOCALAPPDATA%\raven-hotel\bin\
#
# One-liner:
#   irm https://raw.githubusercontent.com/ixmon/raven-harness/main/scripts/install.ps1 | iex
#
# Opt-out of PATH setup:
#   $env:RAVEN_INSTALL_NO_PATH = "1"; irm ... | iex

$ErrorActionPreference = "Stop"

$Repo = "ixmon/raven-harness"
$BinaryName = "raven-tui"
$InstallDir = if ($env:RAVEN_INSTALL_DIR) { $env:RAVEN_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "raven-hotel\bin" }
$DataDir = if ($env:RAVEN_INSTALL_DATA_DIR) { $env:RAVEN_INSTALL_DATA_DIR } else { Join-Path $env:LOCALAPPDATA "raven-hotel" }
$ReleaseApi = "https://api.github.com/repos/$Repo/releases/latest"
$DownloadBase = "https://github.com/$Repo/releases/latest/download"
$Target = "x86_64-pc-windows-msvc"

function Write-Info([string]$Message) {
    Write-Host "==> $Message"
}

function Write-Warn([string]$Message) {
    Write-Warning $Message
}

function Test-InteractiveInstall {
    -not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected
}

function Get-LatestVersion {
    $release = Invoke-RestMethod -Uri $ReleaseApi -Headers @{ "User-Agent" = "raven-hotel-install" }
    return $release.tag_name
}

function Test-PathAlreadyConfigured {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath -split ';' | Where-Object { $_ -eq $InstallDir }) {
        return $true
    }
    return $false
}

function Show-PathInstructions {
    Write-Host ""
    Write-Host "Add Raven to your PATH:"
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `"$InstallDir;`" + [Environment]::GetEnvironmentVariable('Path','User'), 'User')"
    Write-Host ""
    Write-Host "Then open a new terminal and run: $BinaryName"
    Write-Host "Or run now: $(Join-Path $InstallDir "$BinaryName.exe")"
}

function Maybe-ConfigurePath {
    if ($env:RAVEN_INSTALL_NO_PATH -eq "1") {
        Write-Info "Skipping PATH setup (RAVEN_INSTALL_NO_PATH=1)"
        Show-PathInstructions
        return
    }

    if (Test-PathAlreadyConfigured) {
        Write-Info "PATH already includes $InstallDir"
        return
    }

    if (-not (Test-InteractiveInstall)) {
        Write-Info "Non-interactive install — not modifying user PATH"
        Show-PathInstructions
        return
    }

    $answer = Read-Host "Add $InstallDir to your user PATH? [y/N]"
    if ($answer -match '^(y|yes)$') {
        $current = [Environment]::GetEnvironmentVariable("Path", "User")
        if ([string]::IsNullOrEmpty($current)) {
            $newPath = $InstallDir
        } else {
            $newPath = "$InstallDir;$current"
        }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $env:Path = "$InstallDir;$env:Path"
        Write-Info "Updated user PATH (open a new terminal if this one does not pick it up)"
    } else {
        Write-Info "Skipped PATH modification"
        Show-PathInstructions
    }
}

function Write-InstallMetadata([string]$Version) {
    New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
    $metadata = [ordered]@{
        version      = $Version
        target       = $Target
        binary       = (Join-Path $InstallDir "$BinaryName.exe")
        installed_at = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    }
    $metadata | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $DataDir "install.json")
}

function Main {
    if ($PSVersionTable.PSEdition -eq "Desktop" -and $PSVersionTable.PSVersion.Major -lt 5) {
        throw "PowerShell 5+ is required"
    }

    $version = Get-LatestVersion
    $asset = "$BinaryName-$Target.zip"
    $url = "$DownloadBase/$asset"
    $dest = Join-Path $InstallDir "$BinaryName.exe"

    Write-Info "Installing $BinaryName $version for $Target"
    Write-Info "Destination: $dest"

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

    $tmpdir = Join-Path ([System.IO.Path]::GetTempPath()) ("raven-install-" + [guid]::NewGuid().ToString())
    New-Item -ItemType Directory -Force -Path $tmpdir | Out-Null

    try {
        Write-Info "Downloading $url"
        $archive = Join-Path $tmpdir $asset
        Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing

        Expand-Archive -Path $archive -DestinationPath $tmpdir -Force

        $extracted = Get-ChildItem -Path $tmpdir -Recurse -Filter "$BinaryName.exe" -File |
            Select-Object -First 1

        if (-not $extracted) {
            throw "could not find $BinaryName.exe in archive"
        }

        Copy-Item -Path $extracted.FullName -Destination $dest -Force
        Write-InstallMetadata $version

        Write-Info "Installed $dest"
        Maybe-ConfigurePath

        if ($env:Path -split ';' | Where-Object { $_ -eq $InstallDir }) {
            Write-Info "Ready to run: $BinaryName"
        } else {
            Write-Info "Run once PATH is set: $dest"
        }
    } finally {
        Remove-Item -Path $tmpdir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Main
param(
    [string]$CargoBin = "cargo",
    [string]$PythonBin = "python"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ScriptDir = $PSScriptRoot
$RustDir = Split-Path -Parent $ScriptDir
$ProjectDir = Split-Path -Parent $RustDir
$OutputDir = Join-Path $ProjectDir "outputs/windows-amd64"
$Target = "x86_64-pc-windows-msvc"
$ConfiguredTargetDir = [Environment]::GetEnvironmentVariable("CARGO_TARGET_DIR")
if ([string]::IsNullOrEmpty($ConfiguredTargetDir)) {
    $TargetDir = Join-Path $RustDir "target"
} elseif ([System.IO.Path]::IsPathRooted($ConfiguredTargetDir)) {
    $TargetDir = $ConfiguredTargetDir
} else {
    $TargetDir = Join-Path $RustDir $ConfiguredTargetDir
}
$ReleaseDir = Join-Path (Join-Path $TargetDir $Target) "release"

$CargoCommand = Get-Command -Name $CargoBin -CommandType Application -ErrorAction SilentlyContinue
if ($null -eq $CargoCommand) {
    throw "cargo executable not found: $CargoBin. Install the Rust toolchain first."
}
$script:CargoExe = $CargoCommand.Source

foreach ($Source in @(
    (Join-Path $ProjectDir "LICENSE"),
    (Join-Path $RustDir "THIRD_PARTY_NOTICES.txt"),
    (Join-Path $RustDir "USAGE.txt")
)) {
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Required distribution file is missing: $Source"
    }
}

# Get-Command 可能同时命中 WindowsApps 的占位程序，挑真正的 python.exe。
$PythonCommand = Get-Command -Name $PythonBin -CommandType Application -ErrorAction SilentlyContinue |
    Where-Object { $_.Source -like "*.exe" -and $_.Source -notlike "*WindowsApps*" } |
    Select-Object -First 1
if ($null -eq $PythonCommand) {
    throw "Python 3 is required to verify the embedded manifest."
}
$PythonExe = $PythonCommand.Source
if (-not (Test-Path -LiteralPath $PythonExe -PathType Leaf)) {
    throw "Python executable is not usable: $PythonExe"
}

function Invoke-Cargo {
    param([string[]]$CargoArguments)
    & $script:CargoExe @CargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo failed with exit code ${LASTEXITCODE}: $($CargoArguments -join ' ')"
    }
}

# 还原进程环境变量：原值为空说明变量本来不存在，要删除而不是写入空字符串
# （PowerShell 把 $null 绑到 [string] 参数会变成 ""）。
function Restore-ProcessVariable {
    param([string]$Name, [string]$Value)
    if ([string]::IsNullOrEmpty($Value)) {
        Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
    } else {
        [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
    }
}

$PreviousTemp = [Environment]::GetEnvironmentVariable("TEMP", "Process")
$PreviousTmp = [Environment]::GetEnvironmentVariable("TMP", "Process")
$BuildTempDir = Join-Path $ProjectDir (".oilfind-build-temp-" + [Guid]::NewGuid().ToString("N"))
$TempCreated = $false
$LocationPushed = $false
try {
    # Do not reuse an existing path, even if a name collision occurs.
    New-Item -ItemType Directory -Path $BuildTempDir -ErrorAction Stop | Out-Null
    $TempCreated = $true
    [Environment]::SetEnvironmentVariable("TEMP", $BuildTempDir, "Process")
    [Environment]::SetEnvironmentVariable("TMP", $BuildTempDir, "Process")
    Push-Location -LiteralPath $RustDir
    $LocationPushed = $true

    Invoke-Cargo @("test", "--workspace", "--target-dir", $TargetDir)
    Invoke-Cargo @("build", "--release", "--target", $Target, "--target-dir", $TargetDir, "-p", "oilfind-win")

    $GuiBinary = Join-Path $ReleaseDir "oilfind-win.exe"
    if (-not (Test-Path -LiteralPath $GuiBinary -PathType Leaf)) {
        throw "Build output is missing: $GuiBinary"
    }

    & $PythonExe (Join-Path $ScriptDir "package-windows.py") $GuiBinary $OutputDir
    if ($LASTEXITCODE -ne 0) {
        throw "Windows packaging failed with exit code $LASTEXITCODE."
    }

    Write-Host "Local Windows build completed: $OutputDir\OilFind.exe"
    Write-Host "Single file: static C runtime; no installer, sidecar DLL, or data file."
    Write-Host "No installation, publication, or runtime dependency download was performed."
}
finally {
    Restore-ProcessVariable "TEMP" $PreviousTemp
    Restore-ProcessVariable "TMP" $PreviousTmp
    try {
        if ($LocationPushed) {
            Pop-Location
        }
    }
    finally {
        if ($TempCreated) {
            Remove-Item -LiteralPath $BuildTempDir -Recurse -Force
        }
    }
}

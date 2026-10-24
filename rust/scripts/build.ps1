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
$ReleaseDir = Join-Path $RustDir "target/release"

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

New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null

Push-Location -LiteralPath $RustDir
try {
    Invoke-Cargo @("test", "--workspace")
    Invoke-Cargo @("build", "--release", "-p", "oilfind-win")
    Invoke-Cargo @("build", "--release", "-p", "oilfind-cli")

    $GuiBinary = Join-Path $ReleaseDir "oilfind-win.exe"
    $CliBinary = Join-Path $ReleaseDir "oilfind-cli.exe"
    foreach ($Binary in @($GuiBinary, $CliBinary)) {
        if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
            throw "Build output is missing: $Binary"
        }
    }

    Copy-Item -LiteralPath $GuiBinary -Destination (Join-Path $OutputDir "OilFind.exe") -Force
    Copy-Item -LiteralPath $CliBinary -Destination (Join-Path $OutputDir "oilfind-cli.exe") -Force

    & $PythonExe (Join-Path $ScriptDir "verify-pe.py") --arch amd64 (Join-Path $OutputDir "OilFind.exe")
    if ($LASTEXITCODE -ne 0) {
        throw "PE verification failed: the GUI binary is not an amd64 GUI executable with an embedded asInvoker/PerMonitorV2 manifest."
    }

    Copy-Item -LiteralPath (Join-Path $ProjectDir "LICENSE") -Destination (Join-Path $OutputDir "LICENSE") -Force
    Copy-Item -LiteralPath (Join-Path $RustDir "THIRD_PARTY_NOTICES.txt") -Destination (Join-Path $OutputDir "THIRD_PARTY_NOTICES.txt") -Force
    Copy-Item -LiteralPath (Join-Path $RustDir "USAGE.txt") -Destination (Join-Path $OutputDir "USAGE.txt") -Force

    Write-Host "Local Windows build completed: $OutputDir"
    Write-Host "No installation, publication, or runtime dependency download was performed."
}
finally {
    Pop-Location
}

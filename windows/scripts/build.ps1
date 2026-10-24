param(
    [string]$GoBin,
    [string]$PythonBin = "python"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$ModuleDir = Split-Path -Parent $PSScriptRoot
$ProjectDir = Split-Path -Parent $ModuleDir
$OutputDir = Join-Path $ProjectDir "outputs/windows-amd64"

if ([string]::IsNullOrWhiteSpace($GoBin)) {
    $GoBin = $env:GO_BIN
}
if ([string]::IsNullOrWhiteSpace($GoBin)) {
    $GoBin = "go"
}
$GoCommand = Get-Command -Name $GoBin -CommandType Application -ErrorAction SilentlyContinue
if ($null -eq $GoCommand) {
    throw "Go executable not found: $GoBin. Set GO_BIN or pass -GoBin with the Go executable path."
}
$script:GoExe = $GoCommand.Source

foreach ($Source in @((Join-Path $ProjectDir "LICENSE"), (Join-Path $ModuleDir "THIRD_PARTY_NOTICES.txt"), (Join-Path $ModuleDir "USAGE.txt"))) {
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "Required distribution notice is missing: $Source"
    }
}
$PythonCommand = Get-Command -Name $PythonBin -CommandType Application -ErrorAction SilentlyContinue
if ($null -eq $PythonCommand) { throw "Python is required to embed the application manifest." }
$PythonExe = $PythonCommand.Source

$CacheDir = Join-Path $ProjectDir "outputs/go-cache"
$ModCacheDir = Join-Path $ProjectDir "outputs/go-mod-cache"
$TestTempDir = Join-Path $ProjectDir "outputs/test-tmp"
foreach ($Directory in @($OutputDir, $CacheDir, $ModCacheDir, $TestTempDir)) {
    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
}
$env:GOCACHE = $CacheDir
$env:GOMODCACHE = $ModCacheDir
$env:GOPATH = Join-Path $ProjectDir "outputs/go-path"
$env:CGO_ENABLED = "0"
$env:GOTOOLCHAIN = "local"

function Invoke-Go {
    param([string[]]$GoArguments)
    & $script:GoExe @GoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Go command failed with exit code ${LASTEXITCODE}: $($GoArguments -join ' ')"
    }
}

Push-Location -LiteralPath $ModuleDir
try {
    Remove-Item Env:GOOS -ErrorAction SilentlyContinue
    Remove-Item Env:GOARCH -ErrorAction SilentlyContinue
    $OldTemp = $env:TEMP
    $OldTmp = $env:TMP
    # TestLoadRejectsStreamOverEntryLimit (MaxEntries+1 streamed entries, about
    # 900MB RSS) is skipped by default so packaging stays light. Run it on
    # purpose after touching the cache format:
    #   $env:OILFIND_HEAVY_TESTS = "1"; go test ./internal/index -run TestLoadRejectsStreamOverEntryLimit
    try {
        $env:TEMP = $TestTempDir
        $env:TMP = $TestTempDir
        Invoke-Go -GoArguments @("test", "./...")
    } finally {
        $env:TEMP = $OldTemp
        $env:TMP = $OldTmp
    }
    # vet is split by package on purpose. The Win32 UI receives system pointers
    # (WM_NOTIFY / WM_DPICHANGED / WM_GETMINMAXINFO lParam, GlobalLock results)
    # that unsafeptr cannot model statically; those five conversions were
    # reviewed by hand and stay valid only for the message / GlobalLock scope.
    # Only that one analyzer is disabled for internal/ui; everything else still
    # runs, and no nolint/nocheckptr directive is added to the sources. Do not
    # widen this back to `vet ./...`.
    Invoke-Go -GoArguments @("vet", "./internal/index", "./cmd/...")
    Invoke-Go -GoArguments @("vet", "-unsafeptr=false", "./internal/ui")
    & $PythonExe (Join-Path $PSScriptRoot "make-resource.py") --arch amd64 --manifest (Join-Path $ModuleDir "assets/app.manifest") --output (Join-Path $ModuleDir "cmd/oilfind/resource_windows_amd64.syso")
    if ($LASTEXITCODE -ne 0) { throw "Manifest resource generation failed." }

    $env:GOOS = "windows"
    $env:GOARCH = "amd64"
    # GUI binary: Windows GUI subsystem. CLI binary: console subsystem, no -H.
    Invoke-Go -GoArguments @("build", "-trimpath", "-ldflags", "-H windowsgui -s -w", "-o", (Join-Path $OutputDir "OilFindIntranet.exe"), "./cmd/oilfind")
    if (Test-Path -LiteralPath (Join-Path $ModuleDir "cmd/oilfind-cli") -PathType Container) {
        Invoke-Go -GoArguments @("build", "-trimpath", "-ldflags", "-s -w", "-o", (Join-Path $OutputDir "oilfind-cli.exe"), "./cmd/oilfind-cli")
    }

    & $PythonExe (Join-Path $PSScriptRoot "verify-pe.py") --arch amd64 (Join-Path $OutputDir "OilFindIntranet.exe")
    if ($LASTEXITCODE -ne 0) {
        throw "PE verification failed: the GUI binary is not an amd64 GUI executable with an embedded asInvoker manifest."
    }

    Copy-Item -LiteralPath (Join-Path $ProjectDir "LICENSE") -Destination (Join-Path $OutputDir "LICENSE") -Force
    Copy-Item -LiteralPath (Join-Path $ModuleDir "THIRD_PARTY_NOTICES.txt") -Destination (Join-Path $OutputDir "THIRD_PARTY_NOTICES.txt") -Force
    $Usage = Join-Path $ModuleDir "USAGE.txt"
    Copy-Item -LiteralPath $Usage -Destination (Join-Path $OutputDir "USAGE.txt") -Force
    Write-Host "Local Windows build completed: $OutputDir"
    Write-Host "No installation, publication, or runtime dependency download was performed."
} finally {
    Pop-Location
}

# TEMPORARY diagnostic: identify why two test binaries fail to load on Windows
# with 0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND. Delete once the cause is fixed.
$ErrorActionPreference = 'Continue'
$deps = 'src-tauri/target/debug/deps'

function Pick($pattern) {
  Get-ChildItem $deps -Filter $pattern -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

$failing = Pick 'ipc_bridge_integration-*.exe'
$failing2 = Pick 'desktop_menu_integration-*.exe'
$passing = Pick 'git_ops-*.exe'
Write-Host "failing : $($failing.FullName)"
Write-Host "failing2: $($failing2.FullName)"
Write-Host "passing : $($passing.FullName)"

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) { Write-Host "NO vswhere at $vswhere"; exit 0 }
$vs = & $vswhere -latest -products * -property installationPath
Write-Host "vs: $vs"
$dumpbin = Get-ChildItem "$vs\VC\Tools\MSVC" -Recurse -Filter dumpbin.exe -ErrorAction SilentlyContinue |
  Where-Object { $_.FullName -match 'Hostx64\\x64' } | Select-Object -First 1
if (-not $dumpbin) { Write-Host "NO dumpbin found"; exit 0 }
Write-Host "dumpbin: $($dumpbin.FullName)"

function DllsOf($exe) {
  if (-not $exe) { return @() }
  (& $dumpbin.FullName /nologo /dependents $exe.FullName) |
    Select-String -Pattern '^\s{4}\S+\.dll$' |
    ForEach-Object { $_.ToString().Trim().ToLower() }
}

$fd = DllsOf $failing
$fd2 = DllsOf $failing2
$pd = DllsOf $passing
Write-Host "`n=== DLLs imported by the FAILING ipc_bridge binary ==="; $fd | ForEach-Object { Write-Host "  $_" }
Write-Host "`n=== DLLs ONLY the failing binaries import (not in passing) ==="
$extra = ($fd + $fd2 | Sort-Object -Unique) | Where-Object { $pd -notcontains $_ }
$extra | ForEach-Object {
  $found = (Get-Command $_ -ErrorAction SilentlyContinue).Source
  if (-not $found) {
    $sys = Join-Path $env:WINDIR "System32\$_"
    if (Test-Path $sys) { $found = $sys }
  }
  Write-Host ("  {0,-32} -> {1}" -f $_, ($found ?? 'NOT FOUND ON PATH'))
}

Write-Host "`n=== running the failing binary to capture the loader error ==="
& $failing.FullName --list 2>&1 | Select-Object -First 8
Write-Host "exit code: $LASTEXITCODE"

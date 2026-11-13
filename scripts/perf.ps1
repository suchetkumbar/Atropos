#Requires -Version 7
<#
.SYNOPSIS
  Measures Atropos against the spec §3.2 budgets and compares with a baseline (fails on > 20% regression).
.DESCRIPTION
  Modes (comma-separated; 'all' = startup,idle,size,deps):
    startup  Cold start to first frame: median of -Runs launches (one discarded warm-up), via --startup-log.
    idle     Private working set after -Minutes, and CPU % of one core over that window (window visible, idle).
             CPU comes from Get-Process TotalProcessorTime deltas, not typeperf, so it works on any locale.
    size     Release binary sizes (budget 15 MB each).
    deps     No CRT DLL imports (static CRT; Z1). Uses dumpbin located through vswhere.
  Only ever closes the Atropos.exe instances this script started itself (through its own Process objects).
.EXAMPLE
  pwsh scripts/perf.ps1 -Mode all -Minutes 10
  pwsh scripts/perf.ps1 -Mode size,deps
#>
param(
    [string[]]$Mode = @('all'),
    [double]$Minutes = 10,
    [int]$WarmupSeconds = 30,
    [int]$Runs = 10,
    [string]$Exe,
    [string]$CliExe,
    [string]$Renderer = 'default',
    [string]$Baseline,
    [switch]$UpdateBaseline,
    [switch]$RecordWsOnly   # Phase 0: the WS budget is recorded, not enforced.
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
if (-not $Exe) { $Exe = Join-Path $root 'target\release\Atropos.exe' }
if (-not $CliExe) { $CliExe = Join-Path $root 'target\release\atr.exe' }
if (-not $Baseline) { $Baseline = Join-Path $root 'perf\baseline.json' }
$modes = ($Mode -split ',') | ForEach-Object { $_.Trim().ToLowerInvariant() } | Where-Object { $_ }
if ($modes -contains 'all') { $modes = 'startup', 'idle', 'size', 'deps' }

# Budgets (spec §3.2). Lower is better for every metric.
$budget = [ordered]@{
    startup_ms_median    = 300
    gui_private_ws_mb    = 40
    gui_cpu_pct_one_core = 1
    atropos_exe_mb       = 15
    atr_exe_mb           = 15
}
$result = [ordered]@{
    renderer = $Renderer
    os_build = [Environment]::OSVersion.Version.ToString()
    taken_at = (Get-Date).ToString('o')
}
$failures = [System.Collections.Generic.List[string]]::new()

function Stop-Owned([System.Diagnostics.Process]$p) {
    if ($p.HasExited) { return }
    [void]$p.CloseMainWindow()
    if (-not $p.WaitForExit(5000)) { $p.Kill(); $p.WaitForExit() }
}

if ($modes -contains 'startup') {
    $log = Join-Path ([IO.Path]::GetTempPath()) "atropos-startup-$PID.log"
    Remove-Item $log -ErrorAction SilentlyContinue
    for ($i = 0; $i -le $Runs; $i++) {
        $p = Start-Process -FilePath $Exe -ArgumentList '--startup-log', "`"$log`"", '--exit-after-first-frame' -PassThru
        if (-not $p.WaitForExit(15000)) { Stop-Owned $p; $failures.Add("startup: run $i did not exit within 15 s") }
    }
    $vals = @(Get-Content $log | ForEach-Object { [double]($_ -replace '^first_frame_ms=', '') }) | Select-Object -Skip 1 | Sort-Object
    Remove-Item $log -ErrorAction SilentlyContinue
    if ($vals.Count -eq 0) { $failures.Add('startup: no first_frame_ms lines were written') }
    else {
        $result.startup_ms_median = [math]::Round($vals[[int][math]::Floor(($vals.Count - 1) / 2)], 1)
        $result.startup_ms_max = [math]::Round(($vals | Measure-Object -Maximum).Maximum, 1)
    }
}

if ($modes -contains 'idle') {
    $p = Start-Process -FilePath $Exe -PassThru
    try {
        Start-Sleep -Seconds $WarmupSeconds
        $p.Refresh(); $cpu0 = $p.TotalProcessorTime; $t0 = [Diagnostics.Stopwatch]::StartNew()
        $deadline = [TimeSpan]::FromMinutes($Minutes); $peak = 0
        while ($t0.Elapsed -lt $deadline) {
            Start-Sleep -Seconds ([math]::Min(10, [math]::Max(1, ($deadline - $t0.Elapsed).TotalSeconds)))
            $p.Refresh(); if ($p.HasExited) { throw 'Atropos.exe exited during the idle run' }
            $peak = [math]::Max($peak, $p.PrivateMemorySize64)
        }
        $p.Refresh()
        $result.gui_private_ws_mb = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        $result.gui_private_ws_peak_mb = [math]::Round($peak / 1MB, 1)
        $result.gui_cpu_pct_one_core = [math]::Round(($p.TotalProcessorTime - $cpu0).TotalMilliseconds / $t0.Elapsed.TotalMilliseconds * 100, 3)
        $result.idle_minutes = $Minutes
    } finally { Stop-Owned $p }
}

if ($modes -contains 'size') {
    $result.atropos_exe_mb = [math]::Round((Get-Item $Exe).Length / 1MB, 2)
    $result.atr_exe_mb = [math]::Round((Get-Item $CliExe).Length / 1MB, 2)
}

if ($modes -contains 'deps') {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $dumpbin = & $vswhere -latest -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
    if (-not $dumpbin) { $failures.Add('deps: dumpbin.exe not found (install the VS C++ build tools)') }
    else {
        foreach ($bin in $Exe, $CliExe) {
            $imports = & $dumpbin /nologo /dependents $bin | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '\.dll$' }
            $crt = @($imports | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)' })
            $result["deps_$([IO.Path]::GetFileNameWithoutExtension($bin))"] = ($imports -join ' ')
            if ($crt.Count) { $failures.Add("deps: $bin imports CRT DLLs: $($crt -join ', ')") }
        }
    }
}

# Compare with budgets and baseline.
$base = if (Test-Path $Baseline) { Get-Content $Baseline -Raw | ConvertFrom-Json } else { $null }
$rows = foreach ($k in $budget.Keys) {
    if (-not $result.Contains($k)) { continue }
    $m = [double]$result[$k]; $b = $budget[$k]
    $bl = if ($base -and $null -ne $base.$k) { [double]$base.$k } else { $null }
    $status = 'ok'
    if ($m -gt $b) {
        if ($k -eq 'gui_private_ws_mb' -and $RecordWsOnly) { $status = 'OVER (recorded only)' }
        else { $status = 'OVER BUDGET'; $failures.Add("$k = $m exceeds budget $b") }
    }
    if ($null -ne $bl -and $bl -gt 0 -and $m -gt $bl * 1.2) {
        $status = "$status; REGRESSION"; $failures.Add("$k = $m regressed > 20% vs baseline $bl")
    }
    [pscustomobject]@{ metric = $k; budget = $b; baseline = $bl; measured = $m; status = $status }
}
$rows | Format-Table -AutoSize | Out-String | Write-Host

$resDir = Join-Path $root 'perf\results'
New-Item -ItemType Directory -Force $resDir | Out-Null
$resFile = Join-Path $resDir ("{0}-{1}.json" -f (Get-Date -Format 'yyyyMMdd-HHmmss'), $Renderer)
$result | ConvertTo-Json | Set-Content -Encoding utf8 $resFile
Write-Host "Result: $resFile"

if ($UpdateBaseline) {
    $merged = [ordered]@{}
    if ($base) { $base.PSObject.Properties | ForEach-Object { $merged[$_.Name] = $_.Value } }
    foreach ($k in $result.Keys) { $merged[$k] = $result[$k] }
    $merged | ConvertTo-Json | Set-Content -Encoding utf8 $Baseline
    Write-Host "Baseline updated: $Baseline"
}

if ($failures.Count) { $failures | ForEach-Object { Write-Host "FAIL: $_" -ForegroundColor Red }; exit 1 }
Write-Host 'PASS' -ForegroundColor Green

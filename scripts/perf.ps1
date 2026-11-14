#Requires -Version 7
<#
.SYNOPSIS
  Measures Atropos against the spec §3.2 budgets and compares with a baseline (fails on > 20% regression).
.DESCRIPTION
  Modes (comma-separated; 'all' = startup,idle,size,deps,cli):
    startup  Cold start to first frame: median of -Runs launches (one discarded warm-up), via --startup-log.
    idle     After -Minutes with the window visible but not focused (ADR 0007: the "visible idle" state,
             2 s tick; the script focuses a small window of its own): Atropos's own heap (budget 40 MB; ADR 0005),
             the process private working set (tracked for regressions only: it includes the GPU driver),
             and CPU % of one core. CPU comes from Get-Process TotalProcessorTime deltas, so any locale works.
             With -Sleepers <n>, n fixture processes run during the idle window. From Atropos --perf-log:
             collector own-work p95 (tick minus the OS scan call; budget 5 ms, ADR 0007) and the scan
             call p95 (tracked).
    size     Release binary sizes (budget 15 MB each).
    deps     No CRT DLL imports (static CRT; Z1). Uses dumpbin located through vswhere.
    cli      `atr port <n>` median time (budget 100 ms; hyperfine if installed) against a port this
             script holds, and `atr`'s private working set while it runs (budget 10 MB).
  Only ever closes the Atropos.exe instances this script started itself (through its own Process objects).
.EXAMPLE
  pwsh scripts/perf.ps1 -Mode all -Minutes 10
  pwsh scripts/perf.ps1 -Mode size,deps
  pwsh scripts/perf.ps1 -Mode all -Minutes 10 -Sleepers 300
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
    [int]$Sleepers = 0
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
if (-not $Exe) { $Exe = Join-Path $root 'target\release\Atropos.exe' }
if (-not $CliExe) { $CliExe = Join-Path $root 'target\release\atr.exe' }
if (-not $Baseline) { $Baseline = Join-Path $root 'perf\baseline.json' }
$modes = ($Mode -split ',') | ForEach-Object { $_.Trim().ToLowerInvariant() } | Where-Object { $_ }
if ($modes -contains 'all') { $modes = 'startup', 'idle', 'size', 'deps', 'cli' }

# Budgets (spec §3.2; idle memory per ADR 0005). Lower is better for every metric.
# $null = no budget, but still compared with the baseline for > 20% regressions.
$budget = [ordered]@{
    startup_ms_median    = 300
    gui_app_heap_mb      = 40
    gui_private_ws_mb    = $null
    gui_cpu_pct_one_core = 1
    collector_own_p95_ms = 5
    collector_scan_p95_ms = $null
    atropos_exe_mb       = 15
    atr_exe_mb           = 15
    atr_port_ms_median   = 100
    cli_private_ws_mb    = 10
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
    $vals = @(Get-Content $log | Where-Object { $_ -match '^first_frame_ms=' } | ForEach-Object { [double]($_ -replace '^first_frame_ms=', '') }) | Select-Object -Skip 1 | Sort-Object
    Remove-Item $log -ErrorAction SilentlyContinue
    if ($vals.Count -eq 0) { $failures.Add('startup: no first_frame_ms lines were written') }
    else {
        $result.startup_ms_median = [math]::Round($vals[[int][math]::Floor(($vals.Count - 1) / 2)], 1)
        $result.startup_ms_max = [math]::Round(($vals | Measure-Object -Maximum).Maximum, 1)
    }
}

if ($modes -contains 'idle') {
    $idleLog = Join-Path ([IO.Path]::GetTempPath()) "atropos-idle-$PID.log"
    $tickLog = Join-Path ([IO.Path]::GetTempPath()) "atropos-ticks-$PID.log"
    Remove-Item $idleLog, $tickLog -ErrorAction SilentlyContinue
    # Owned fixture processes: started here, stopped here through their own Process objects only.
    $owned = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
    if ($Sleepers -gt 0) {
        $sleeper = Join-Path (Split-Path $Exe) 'sleeper.exe'
        if (-not (Test-Path $sleeper)) { throw "sleeper.exe not found next to $Exe (cargo build --release --workspace)" }
        $secs = [int]($Minutes * 60 + $WarmupSeconds + 120)
        # Share this console and pipe stdout (one key line, never read): no console host per
        # sleeper, so 300 of them cost ~0.9 GB instead of ~3.7 GB.
        for ($i = 0; $i -lt $Sleepers; $i++) {
            $psi = [System.Diagnostics.ProcessStartInfo]::new($sleeper, "--secs $secs")
            $psi.UseShellExecute = $false; $psi.RedirectStandardOutput = $true
            $owned.Add([System.Diagnostics.Process]::Start($psi))
        }
        $result.sleepers = $Sleepers
    }
    $p = Start-Process -FilePath $Exe -ArgumentList '--startup-log', "`"$idleLog`"", '--perf-log', "`"$tickLog`"" -PassThru
    # Visible but not focused: show a small window of our own and give it the focus.
    Add-Type -AssemblyName System.Windows.Forms
    $focusThief = [System.Windows.Forms.Form]@{ Text = 'perf.ps1 focus'; Width = 240; Height = 80; StartPosition = 'Manual'; Left = 0; Top = 0 }
    try {
        Start-Sleep -Seconds 3
        $focusThief.Show(); $focusThief.Activate(); [System.Windows.Forms.Application]::DoEvents()
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
    } finally {
        $focusThief.Dispose()
        Stop-Owned $p
        foreach ($s in $owned) { if (-not $s.HasExited) { $s.Kill(); [void]$s.WaitForExit(5000) } }
    }
    # Lines: tick_ms=<total> scan_ms=<OS call>.
    $own = [System.Collections.Generic.List[double]]::new(); $scan = [System.Collections.Generic.List[double]]::new()
    foreach ($line in @(Get-Content $tickLog -ErrorAction SilentlyContinue)) {
        if ($line -match '^tick_ms=([\d.]+) scan_ms=([\d.]+)') {
            $own.Add([double]$Matches[1] - [double]$Matches[2]); $scan.Add([double]$Matches[2])
        }
    }
    Remove-Item $tickLog -ErrorAction SilentlyContinue
    function P95($v) { $s = @($v | Sort-Object); [math]::Round($s[[int][math]::Floor(($s.Count - 1) * 0.95)], 2) }
    if ($own.Count -gt 0) {
        $result.collector_own_p95_ms = P95 $own
        $result.collector_scan_p95_ms = P95 $scan
        $result.collector_ticks = $own.Count
        # ~30/min proves the visible-unfocused 2 s tick; ~60/min would mean the window had focus.
        $result.idle_ticks_per_min = [math]::Round($own.Count / ($Minutes + $WarmupSeconds / 60), 1)
    }
    # The app writes app_heap_mb= on a clean exit (CloseMainWindow), i.e. at the end of the window.
    $heap = @(Get-Content $idleLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '^app_heap_mb=' })
    Remove-Item $idleLog -ErrorAction SilentlyContinue
    if ($heap.Count -eq 0) { $failures.Add('idle: no app_heap_mb line (Atropos did not exit cleanly)') }
    else { $result.gui_app_heap_mb = [double]($heap[-1] -replace '^app_heap_mb=', '') }
}

if ($modes -contains 'cli') {
    # A port owned by this script, so `atr port` has something real to find.
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    try {
        $hf = Get-Command hyperfine -ErrorAction SilentlyContinue
        if ($hf) {
            $json = Join-Path ([IO.Path]::GetTempPath()) "atropos-hf-$PID.json"
            & $hf.Source --warmup 3 --runs 30 -N --export-json $json "`"$CliExe`" port $port" | Out-Null
            $r0 = (Get-Content $json -Raw | ConvertFrom-Json).results[0]
            # hyperfine 1.x: results[0].median; 2.x: results[0].summary.time_wall_clock.median.
            $median = if ($null -ne $r0.median) { $r0.median } else { $r0.summary.time_wall_clock.median }
            Remove-Item $json -ErrorAction SilentlyContinue
            $result.atr_port_ms_median = [math]::Round($median * 1000, 1)
            $result.atr_port_timer = 'hyperfine'
        } else {
            $times = foreach ($i in 1..20) {
                $sw = [Diagnostics.Stopwatch]::StartNew(); & $CliExe port $port | Out-Null; $sw.Elapsed.TotalMilliseconds
            }
            $sorted = @($times | Sort-Object)
            $result.atr_port_ms_median = [math]::Round($sorted[[int][math]::Floor(($sorted.Count - 1) / 2)], 1)
            $result.atr_port_timer = 'stopwatch'
        }
        # Memory while `atr` runs: it waits on our busy port; we stop it ourselves.
        $w = Start-Process -FilePath $CliExe -ArgumentList 'wait-port', $port, '--timeout', '20s' -WindowStyle Hidden -PassThru
        Start-Sleep -Milliseconds 1500
        $w.Refresh()
        $result.cli_private_ws_mb = [math]::Round($w.PrivateMemorySize64 / 1MB, 2)
        if (-not $w.HasExited) { $w.Kill(); [void]$w.WaitForExit(5000) }
    } finally { $listener.Stop() }
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
    $status = if ($null -eq $b) { 'tracked' } else { 'ok' }
    if ($null -ne $b -and $m -gt $b) { $status = 'OVER BUDGET'; $failures.Add("$k = $m exceeds budget $b") }
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

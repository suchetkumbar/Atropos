//! Memory parity with Windows' own counter (spec Phase 1 gate: within 5 % of Task Manager).
//! Task Manager's "Memory" column is "Working Set - Private"; we read the same counter through
//! the locale-independent CIM class `Win32_PerfFormattedData_PerfProc_Process`.
//!
//! Timing-sensitive (two samples of a live system), so on demand only:
//! `cargo test -p atropos-fixtures --test parity -- --ignored --nocapture`
#![cfg(windows)]

use std::collections::HashMap;
use std::process::Command;

use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;

fn counter_private_ws() -> std::io::Result<HashMap<u32, u64>> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | \
             ForEach-Object { \"$($_.IDProcess) $($_.WorkingSetPrivate)\" }",
        ])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, ws) = l.trim().split_once(' ')?;
            Some((pid.parse().ok()?, ws.parse().ok()?))
        })
        .collect())
}

#[test]
#[ignore = "compares two live samples; run on demand"]
fn private_working_set_matches_the_windows_counter_within_5_percent() {
    let counters = counter_private_ws().unwrap();
    let mut p = WindowsPlatform::new();
    let mut out = Vec::new();
    p.scan(&mut out).unwrap();
    let (mut compared, mut agree) = (0, 0);
    let mut worst = Vec::new();
    for r in out
        .iter()
        .filter(|r| r.key.pid > 4 && r.private_ws > 20 << 20)
    {
        let Some(&c) = counters.get(&r.key.pid) else {
            continue;
        };
        compared += 1;
        #[allow(clippy::cast_precision_loss)]
        let diff = (r.private_ws as f64 - c as f64).abs() / c.max(1) as f64;
        if diff <= 0.05 {
            agree += 1;
        } else {
            worst.push(format!(
                "{} {}: ours {} vs {}",
                r.image, r.key.pid, r.private_ws, c
            ));
        }
    }
    println!("compared {compared}, within 5%: {agree}");
    for w in &worst {
        println!("  {w}");
    }
    // Both sides sample a moving system a moment apart; demand 90 % agreement.
    assert!(compared >= 5, "too few processes to compare");
    assert!(agree * 10 >= compared * 9, "{agree} of {compared}");
}

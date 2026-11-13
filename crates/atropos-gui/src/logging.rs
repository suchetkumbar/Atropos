//! Rolling file log under `%LOCALAPPDATA%\Atropos\logs`. Default level `warn`; override with `ATROPOS_LOG`.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;

/// Installs the global subscriber. Keep the returned guard alive for the life of the process.
///
/// # Errors
/// If the log directory cannot be created or a subscriber is already installed.
pub fn init() -> anyhow::Result<WorkerGuard> {
    let dir = atropos_core::paths::log_dir();
    std::fs::create_dir_all(&dir)?;
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, "atropos.log"));
    let level = std::env::var("ATROPOS_LOG")
        .ok()
        .and_then(|v| v.parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::WARN);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(level)
        .try_init()
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(guard)
}

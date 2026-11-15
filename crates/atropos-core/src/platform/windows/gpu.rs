//! GPU utilisation and dedicated memory per process via PDH (spec Phase 5).
//!
//! Counters are added by their English names (`PdhAddEnglishCounterW`), so this works on any
//! display language (Z4). The "GPU Engine" and "GPU Process Memory" objects exist from Windows 10
//! 1709 with a WDDM 2.x driver; without them (no GPU, basic display adapter, some VMs / RDP)
//! opening fails and the GUI shows "—" (Z7). See `docs/notes/windows-apis.md`.

use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::{PCWSTR, w};

use crate::gpu::{GpuSample, aggregate};
use crate::{CoreError, Result};

const ERROR_SUCCESS: u32 = 0;
/// `PDH_INVALID_DATA`: a rate counter after only one collection (the first sample).
const PDH_INVALID_DATA: u32 = 0xC000_0BBA;

#[allow(clippy::cast_possible_wrap)] // as `os` reports it
const fn invalid_data() -> i32 {
    PDH_INVALID_DATA as i32
}

#[allow(clippy::cast_possible_wrap)] // PDH status codes as reported
fn os(api: &'static str, code: u32) -> CoreError {
    CoreError::Os {
        api,
        code: code as i32,
    }
}

/// An open PDH query with the two wildcard counters; closed on drop.
pub struct GpuQuery {
    query: PDH_HQUERY,
    engine: PDH_HCOUNTER,
    memory: PDH_HCOUNTER,
    /// Reused across samples; PDH writes the items and then their names into it.
    buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W>,
}

// SAFETY: the PDH handles are only used through `&mut self` on one thread at a time; PDH
// queries are not tied to the thread that opened them.
unsafe impl Send for GpuQuery {}

impl Drop for GpuQuery {
    fn drop(&mut self) {
        // SAFETY: `query` came from a successful `PdhOpenQueryW` and is closed once; this also
        // frees its counters.
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}

impl GpuQuery {
    /// Opens the query. Utilisation is a rate over two collections, so the first
    /// [`sample`](Self::sample) reports memory only (sampling right after an extra collection
    /// would divide by an interval of ~0 ms and spike).
    ///
    /// # Errors
    /// If PDH or the GPU counter objects are unavailable.
    pub fn open() -> Result<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: a null data source means real-time data; `query` is a valid out-pointer.
        let st = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) };
        if st != ERROR_SUCCESS {
            return Err(os("PdhOpenQueryW", st));
        }
        let mut q = Self {
            query,
            engine: PDH_HCOUNTER::default(),
            memory: PDH_HCOUNTER::default(),
            buf: Vec::new(),
        };
        q.engine = q.add(w!("\\GPU Engine(*)\\Utilization Percentage"))?;
        q.memory = q.add(w!("\\GPU Process Memory(*)\\Dedicated Usage"))?;
        Ok(q)
    }

    fn add(&mut self, path: PCWSTR) -> Result<PDH_HCOUNTER> {
        let mut c = PDH_HCOUNTER::default();
        // SAFETY: `self.query` is open; `path` is a static NUL-terminated string; `c` is a
        // valid out-pointer.
        let st = unsafe { PdhAddEnglishCounterW(self.query, path, 0, &raw mut c) };
        if st == ERROR_SUCCESS {
            Ok(c)
        } else {
            Err(os("PdhAddEnglishCounterW", st))
        }
    }

    fn collect(&mut self) -> Result<()> {
        // SAFETY: `self.query` is open.
        let st = unsafe { PdhCollectQueryData(self.query) };
        if st == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(os("PdhCollectQueryData", st))
        }
    }

    /// Collects and returns one sample per process.
    ///
    /// # Errors
    /// If the collection fails (the objects disappeared, e.g. a driver reset).
    pub fn sample(&mut self) -> Result<Vec<GpuSample>> {
        self.collect()?;
        let engines = match self.read(self.engine) {
            // First sample: no utilisation yet, memory is already valid.
            Err(CoreError::Os { code, .. }) if code == invalid_data() => Vec::new(),
            r => r?,
        };
        let memory = self.read(self.memory)?;
        Ok(aggregate(&engines, &memory))
    }

    /// The counter's instances and values; instances without valid data (the first sample of a
    /// rate) are left out.
    fn read(&mut self, counter: PDH_HCOUNTER) -> Result<Vec<(String, f64)>> {
        let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let fmt = PDH_FMT_DOUBLE;
        for _ in 0..4 {
            let mut bytes = u32::try_from(self.buf.len() * item).unwrap_or(u32::MAX);
            let mut count = 0u32;
            let ptr = (!self.buf.is_empty()).then_some(self.buf.as_mut_ptr());
            // SAFETY: `counter` belongs to the open query; `ptr` (if any) points to `bytes`
            // writable, suitably aligned bytes; the out-pointers are valid.
            let st = unsafe {
                PdhGetFormattedCounterArrayW(counter, fmt, &raw mut bytes, &raw mut count, ptr)
            };
            if st == PDH_MORE_DATA {
                // Room for the names too, rounded up to whole items, plus slack for instances
                // that appear between the two calls.
                let need = (bytes as usize).div_ceil(item) + 16;
                self.buf.resize(
                    need.max(self.buf.len() * 2),
                    PDH_FMT_COUNTERVALUE_ITEM_W::default(),
                );
                continue;
            }
            if st != ERROR_SUCCESS {
                return Err(os("PdhGetFormattedCounterArrayW", st));
            }
            let n = (count as usize).min(self.buf.len());
            let mut out = Vec::with_capacity(n);
            for it in &self.buf[..n] {
                let status = it.FmtValue.CStatus;
                if status != PDH_CSTATUS_VALID_DATA && status != PDH_CSTATUS_NEW_DATA {
                    continue;
                }
                // SAFETY: PDH set `szName` to a NUL-terminated string inside `self.buf`, which
                // is not touched until this loop ends.
                let name = unsafe { it.szName.to_string() }.unwrap_or_default();
                // SAFETY: PDH_FMT_DOUBLE was requested, so `doubleValue` is the active field.
                let v = unsafe { it.FmtValue.Anonymous.doubleValue };
                out.push((name, v));
            }
            return Ok(out);
        }
        Err(os("PdhGetFormattedCounterArrayW", PDH_MORE_DATA))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: opens the query on this machine, samples twice, prints what it saw and the cost.
    /// `cargo test -p atropos-core --lib gpu_live -- --ignored --nocapture`
    #[test]
    #[ignore = "reads this machine's GPU counters; prints timings"]
    fn gpu_live() {
        let t = std::time::Instant::now();
        let mut q = match GpuQuery::open() {
            Ok(q) => q,
            Err(e) => {
                println!("GPU counters unavailable: {e}");
                return;
            }
        };
        let first = q.sample().unwrap();
        println!(
            "open + first sample: {:?}; utilisation in the first sample for {} processes",
            t.elapsed(),
            first.iter().filter(|s| s.util_pct > 0.0).count()
        );
        let mut times = Vec::new();
        let mut last = Vec::new();
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let t = std::time::Instant::now();
            last = q.sample().unwrap();
            times.push(t.elapsed());
        }
        times.sort();
        println!(
            "sample: median {:?}, max {:?}, {} processes",
            times[times.len() / 2],
            times[times.len() - 1],
            last.len()
        );
        for s in last
            .iter()
            .filter(|s| s.util_pct > 0.0 || s.dedicated > 0)
            .take(15)
        {
            println!("{s:?}");
        }
    }
}

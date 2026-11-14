//! A global allocator that counts the bytes Atropos itself has live on the Rust heap.
//!
//! GPU drivers loaded into the process allocate through their own heaps, so this figure is
//! the app's own memory: the quantity the spec §3.2 idle-memory budget governs (ADR 0005).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Wraps [`System`] and tracks live bytes. Install with `#[global_allocator]`.
pub struct CountingAlloc {
    live: AtomicUsize,
}

impl CountingAlloc {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            live: AtomicUsize::new(0),
        }
    }

    /// Bytes currently allocated through this allocator.
    pub fn live_bytes(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    fn track(&self, ptr: *mut u8, add: usize) -> *mut u8 {
        if !ptr.is_null() {
            self.live.fetch_add(add, Ordering::Relaxed);
        }
        ptr
    }
}

impl Default for CountingAlloc {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged; the counter
// is only bookkeeping and never affects the returned pointers.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `GlobalAlloc::alloc`'s contract.
        self.track(unsafe { System.alloc(layout) }, layout.size())
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim, as in `alloc`.
        self.track(unsafe { System.alloc_zeroed(layout) }, layout.size())
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded verbatim; `ptr` was allocated by `System` with `layout`.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            // On failure the old block stays live and the count is unchanged.
            self.live.fetch_sub(layout.size(), Ordering::Relaxed);
            self.live.fetch_add(new_size, Ordering::Relaxed);
        }
        new
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` came from this allocator, which is `System` underneath.
        unsafe { System.dealloc(ptr, layout) };
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_alloc_realloc_and_dealloc() {
        let a = CountingAlloc::new();
        let layout = Layout::from_size_align(1000, 8).unwrap();
        // SAFETY: non-zero layout; the pointer is freed with the matching layout below.
        unsafe {
            let p = a.alloc(layout);
            assert!(!p.is_null());
            assert_eq!(a.live_bytes(), 1000);
            let p = a.realloc(p, layout, 4000);
            assert!(!p.is_null());
            assert_eq!(a.live_bytes(), 4000);
            a.dealloc(p, Layout::from_size_align(4000, 8).unwrap());
        }
        assert_eq!(a.live_bytes(), 0);
    }

    #[test]
    fn counts_zeroed_alloc() {
        let a = CountingAlloc::new();
        let layout = Layout::from_size_align(64, 8).unwrap();
        // SAFETY: as above.
        unsafe {
            let p = a.alloc_zeroed(layout);
            assert_eq!(a.live_bytes(), 64);
            a.dealloc(p, layout);
        }
        assert_eq!(a.live_bytes(), 0);
    }
}

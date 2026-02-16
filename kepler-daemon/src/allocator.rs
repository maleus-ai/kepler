//! Allocator utilities for memory management.
//!
//! When the `jemalloc` feature is enabled, this module provides functions to
//! configure jemalloc's decay rates and purge cached memory pages. This is
//! important for long-running daemons where the allocator may hold onto freed
//! pages indefinitely, causing RSS to stay elevated even after cleanup.

/// Configure the allocator for aggressive memory return.
///
/// With jemalloc: sets dirty and muzzy decay times to 0ms so freed pages
/// are returned to the OS immediately rather than being held in per-arena caches.
///
/// Should be called once at daemon startup.
pub fn configure() {
    #[cfg(all(feature = "jemalloc", not(feature = "dhat-heap")))]
    {
        use tikv_jemalloc_ctl::raw;
        use tracing::{info, warn};

        let val: isize = 0;

        // Set default dirty decay for NEW arenas
        if let Err(e) = unsafe { raw::write(b"arenas.dirty_decay_ms\0", val) } {
            warn!("Failed to set jemalloc arenas.dirty_decay_ms: {}", e);
        }

        // Set default muzzy decay for NEW arenas
        if let Err(e) = unsafe { raw::write(b"arenas.muzzy_decay_ms\0", val) } {
            warn!("Failed to set jemalloc arenas.muzzy_decay_ms: {}", e);
        }

        // Apply to ALL existing arenas (4096 = MALLCTL_ARENAS_ALL)
        if let Err(e) = unsafe { raw::write(b"arena.4096.dirty_decay_ms\0", val) } {
            warn!("Failed to set jemalloc arena.4096.dirty_decay_ms: {}", e);
        }
        if let Err(e) = unsafe { raw::write(b"arena.4096.muzzy_decay_ms\0", val) } {
            warn!("Failed to set jemalloc arena.4096.muzzy_decay_ms: {}", e);
        }

        info!("jemalloc configured: dirty_decay_ms=0, muzzy_decay_ms=0 (all arenas)");
    }
}

/// Purge allocator caches, returning freed pages to the OS.
///
/// With jemalloc: triggers `arena.<i>.purge` on all arenas.
/// Without jemalloc (glibc): calls `malloc_trim(0)`.
///
/// Should be called after major cleanup operations (e.g., config unload)
/// to ensure freed memory is promptly returned to the OS.
pub fn purge_caches() {
    #[cfg(all(feature = "jemalloc", not(feature = "dhat-heap")))]
    {
        use std::ptr;
        use tracing::{debug, warn};

        // arena.4096.purge is a void mallctl — must call mallctl directly with null pointers
        let key = b"arena.4096.purge\0";
        let ret = unsafe {
            tikv_jemalloc_sys::mallctl(
                key.as_ptr().cast(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
            )
        };
        if ret == 0 {
            debug!("jemalloc: purged all arena caches");
        } else {
            warn!("jemalloc: failed to purge arenas (mallctl returned {})", ret);
        }
    }

    #[cfg(all(not(feature = "jemalloc"), unix))]
    {
        use tracing::debug;

        // glibc: release free memory back to the OS
        unsafe {
            libc::malloc_trim(0);
        }
        debug!("glibc: malloc_trim(0) called");
    }
}

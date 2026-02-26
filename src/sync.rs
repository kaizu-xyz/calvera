//! Centralized re-exports of sync primitives.
//!
//! Under normal builds, re-exports from `std`.
//! Under `--cfg loom`, re-exports from `loom` so that loom can intercept
//! all atomic operations and explore all interleavings.
//!
//! All other modules in calvera import from `crate::sync` instead of
//! `std::sync` directly. This keeps the cfg branching in one place.

// ── std path (default) ──────────────────────────────────────────────
#[cfg(not(loom))]
pub(crate) struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(std::cell::UnsafeCell::new(value))
    }

    #[inline(always)]
    pub(crate) fn get(&self) -> *mut T {
        self.0.get()
    }

    #[inline(always)]
    pub(crate) fn get_ref(&self) -> *const T {
        self.0.get() as *const T
    }
}
#[cfg(not(loom))]
pub(crate) use std::sync::Arc;
#[cfg(not(loom))]
pub(crate) use std::sync::Mutex;

#[cfg(not(loom))]
pub(crate) mod atomic {
    pub(crate) use std::sync::atomic::{AtomicI64, AtomicU64, Ordering, fence};
}

#[cfg(not(loom))]
pub(crate) mod thread {
    pub(crate) use std::thread::{Builder, JoinHandle};
}

// ── loom path ───────────────────────────────────────────────────────
#[cfg(loom)]
pub(crate) use loom::sync::Arc;
#[cfg(loom)]
pub(crate) use loom::sync::Mutex;

#[cfg(loom)]
pub(crate) mod atomic {
    pub(crate) use loom::sync::atomic::{AtomicI64, AtomicU64, Ordering, fence};
}

/// Wrapper around `loom::cell::UnsafeCell` that exposes the same `.get() -> *mut T`
/// API as `std::cell::UnsafeCell`. Loom's UnsafeCell uses `.with_mut(|ptr| ...)` instead.
#[cfg(loom)]
pub(crate) struct UnsafeCell<T>(loom::cell::UnsafeCell<T>);

#[cfg(loom)]
impl<T> UnsafeCell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(loom::cell::UnsafeCell::new(value))
    }

    pub(crate) fn get(&self) -> *mut T {
        self.0.with_mut(|ptr| ptr)
    }

    pub(crate) fn get_ref(&self) -> *const T {
        self.0.with(|ptr| ptr)
    }
}

/// Shim for `std::thread::Builder` which loom doesn't provide.
/// Ignores `.name()` (loom doesn't model thread names) and delegates
/// `.spawn()` to `loom::thread::spawn`.
#[cfg(loom)]
pub(crate) mod thread {
    pub(crate) use loom::thread::JoinHandle;

    pub(crate) struct Builder;

    impl Builder {
        pub(crate) fn new() -> Self {
            Self
        }

        pub(crate) fn name(self, _name: String) -> Self {
            self
        }

        pub(crate) fn spawn<F, T>(self, f: F) -> std::io::Result<JoinHandle<T>>
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static,
        {
            Ok(loom::thread::spawn(f))
        }
    }
}

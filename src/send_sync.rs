//! Target-aware `Send + Sync` marker.
//!
//! The pool's trait objects (`Transport`, `Metrics`) must be thread-safe on
//! native targets so the pool can be shared across tasks/threads. On `wasm32`
//! there are no threads and the browser `fetch` future holds non-`Send`
//! `JsValue`s, so requiring `Send + Sync` would make a `FetchTransport`
//! impossible to write.
//!
//! [`MaybeSendSync`] resolves to `Send + Sync` on native and to an empty bound
//! on wasm, with a blanket impl for every type, so a single trait definition
//! works on both targets.

/// `Send + Sync` on native, no-op on `wasm32`.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync> MaybeSendSync for T {}

/// `Send + Sync` on native, no-op on `wasm32`.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}

#[cfg(target_arch = "wasm32")]
impl<T> MaybeSendSync for T {}

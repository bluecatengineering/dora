//! # register_derive
//!
//! This is a macro to derive the `Register` implementation for dora, since
//! its implementation is pretty mechanical, we can simplify things for users by
//! providing a derive macro.
//!
#[doc(hidden)]
pub use register_derive_impl::*;

pub use dora_core::Register;

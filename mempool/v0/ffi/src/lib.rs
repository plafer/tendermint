///! Rust implementation of the CList mempool
///! The current implementation only supports one mempool instantiated.
///! However, the handle-based API is designed to support arbitrarily many.
///! Every function exposed is expected to be accessed be 1 thread at a time.
///! In other words, the go code is expected to guard the access to the FFI
///! with a mutex.
mod mempool;
pub mod ffi;

pub use ffi::*;

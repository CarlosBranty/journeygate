//! The async bridge: guard-cli is synchronous, the adapters are async.

use std::future::Future;

/// Runs `future` to completion on a fresh multi-threaded tokio runtime.
pub fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("cannot start the tokio runtime")
        .block_on(future)
}

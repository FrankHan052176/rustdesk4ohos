//! One application executor for sync NAPI entry points and both peer roles.
use std::sync::OnceLock;
use tokio::runtime::Runtime;

pub(crate) fn runtime() -> Result<&'static Runtime, ()> {
    static RUNTIME: OnceLock<Result<Runtime, ()>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("rd-engine-io")
                .enable_all()
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| ())
}

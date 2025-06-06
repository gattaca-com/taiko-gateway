use std::{future::Future, sync::OnceLock};

use tokio::{runtime::Handle, task::JoinHandle};

static RUNTIME: OnceLock<Handle> = OnceLock::new();

pub fn init_runtime() {
    RUNTIME.get_or_init(build_tokio_runtime);
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match RUNTIME.get() {
        Some(runtime) => runtime.spawn(future),
        None => panic!("runtime has not been initialised!"),
    }
}

pub fn build_tokio_runtime() -> Handle {
    if let Ok(handle) = Handle::try_current() {
        return handle;
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create runtime")
        .handle()
        .clone()
}

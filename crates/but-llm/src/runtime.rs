//! Blocking entry points over the async core.

use std::{future::Future, sync::OnceLock};

static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

/// Run `fut` to completion on a process-wide runtime (2 workers, started on first use) and block until it ends.
///
/// This replaces a runtime per request. The future runs on a scoped helper thread so that blocking is allowed
/// wherever the caller is, including inside another runtime's task. Applications that own a runtime should call
/// the async API of [`crate::LLMProvider`] on it instead.
pub(crate) fn block_on<F, T>(fut: F) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>> + Send,
    T: Send,
{
    let rt = RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("but-llm")
                .enable_all()
                .build()
                .map_err(|err| err.to_string())
        })
        .as_ref()
        .map_err(|err| anyhow::anyhow!("cannot start the LLM runtime: {err}"))?;
    std::thread::scope(|scope| match scope.spawn(|| rt.block_on(fut)).join() {
        Ok(result) => result,
        // Resume rather than mask: the panic came from inside the request.
        Err(panic) => std::panic::resume_unwind(panic),
    })
}

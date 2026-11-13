//! The one tokio runtime shared by the forge, AI and update services (20 §8.1, §8.2).

use std::sync::Arc;

use gitcthulhu_git::CancelToken;

/// A future was dropped because its [`CancelToken`] fired.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// Cheaply cloneable handle to the shared runtime: multi-threaded, 2 workers, all drivers enabled,
/// worker threads named `gitcthulhu-svc`.
///
/// The runtime shuts down when the last clone is dropped. That may happen anywhere, including inside a task
/// running on the runtime itself (a service clone moved into a spawned task); in that case the runtime is
/// shut down in the background instead of panicking like a plain `tokio::runtime::Runtime` would.
#[derive(Clone, Debug)]
pub struct Runtime(Arc<Inner>);

#[derive(Debug)]
struct Inner(Option<tokio::runtime::Runtime>);

impl Drop for Inner {
    fn drop(&mut self) {
        let Some(rt) = self.0.take() else { return };
        if tokio::runtime::Handle::try_current().is_ok() {
            // A regular drop blocks until the workers stopped, which tokio forbids (panics) in an async context.
            rt.shutdown_background();
        } else {
            drop(rt);
        }
    }
}

impl Runtime {
    /// Build the runtime.
    pub fn new() -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("gitcthulhu-svc")
            .enable_all()
            .build()?;
        Ok(Runtime(Arc::new(Inner(Some(rt)))))
    }

    fn rt(&self) -> &tokio::runtime::Runtime {
        self.0
            .0
            .as_ref()
            .expect("BUG: the runtime is only taken out in Drop")
    }

    /// The underlying tokio handle, e.g. to `spawn` background work.
    pub fn handle(&self) -> &tokio::runtime::Handle {
        self.rt().handle()
    }

    /// Run `f` to completion on the runtime, blocking the calling thread.
    ///
    /// Must be called from a thread that is not itself driving a tokio runtime (a GUI background-executor
    /// worker is fine); this is checked with a `debug_assert!`. Several threads may block at the same time.
    pub fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        debug_assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "Runtime::block_on must not be called from within a tokio runtime"
        );
        self.rt().block_on(f)
    }

    /// Race `f` against `cancel`.
    ///
    /// On cancellation the future is dropped (which closes its HTTP connection) before this returns
    /// `Err(Cancelled)`. A token that is already cancelled wins without polling `f` at all.
    pub fn block_on_cancellable<F, T>(&self, cancel: &CancelToken, f: F) -> Result<T, Cancelled>
    where
        F: std::future::Future<Output = T>,
    {
        self.block_on(async {
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(Cancelled),
                v = f => Ok(v),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn block_on_from_worker_thread() {
        let rt = Runtime::new().expect("runtime builds");
        let caller = std::thread::current().id();
        let (out, ran_on) = std::thread::spawn(move || {
            rt.block_on(async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                (7, std::thread::current().id())
            })
        })
        .join()
        .expect("worker thread does not panic");
        assert_eq!(out, 7, "the future's output is returned");
        assert_ne!(
            ran_on, caller,
            "the future ran on the spawned worker, not the test thread"
        );
    }

    #[test]
    fn block_on_from_several_threads_at_once() {
        let rt = Runtime::new().expect("runtime builds");
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let rt = rt.clone();
                std::thread::spawn(move || {
                    rt.block_on(async move {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        i
                    })
                })
            })
            .collect();
        let outs: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("worker thread does not panic"))
            .collect();
        assert_eq!(outs, [0, 1, 2, 3], "every caller gets its own output");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "must not be called from within a tokio runtime")]
    fn block_on_inside_runtime_is_caught_in_debug_builds() {
        let rt = Runtime::new().expect("runtime builds");
        let inner = rt.clone();
        rt.block_on(async move { inner.block_on(async {}) });
    }

    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn block_on_cancellable_drops_future() {
        let rt = Runtime::new().expect("runtime builds");
        let cancel = CancelToken::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let canceller = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                cancel.cancel();
            })
        };
        let guard = SetOnDrop(dropped.clone());
        let started = Instant::now();
        let res = rt.block_on_cancellable(&cancel, async move {
            let _guard = guard;
            tokio::time::sleep(Duration::from_secs(60)).await;
            "finished"
        });
        let waited = started.elapsed();
        canceller.join().expect("canceller finishes");
        assert!(
            matches!(res, Err(Cancelled)),
            "a cancelled run reports Cancelled, got {res:?}"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "the future was dropped before block_on_cancellable returned"
        );
        assert!(
            waited < Duration::from_secs(30),
            "cancellation interrupts the wait instead of letting the future finish ({waited:?})"
        );
    }

    #[test]
    fn block_on_cancellable_with_cancelled_token_never_polls() {
        let rt = Runtime::new().expect("runtime builds");
        let cancel = CancelToken::new();
        cancel.cancel();
        let polled = Arc::new(AtomicBool::new(false));
        let res = rt.block_on_cancellable(&cancel, {
            let polled = polled.clone();
            async move {
                polled.store(true, Ordering::SeqCst);
            }
        });
        assert!(
            matches!(res, Err(Cancelled)),
            "an already cancelled token wins"
        );
        assert!(
            !polled.load(Ordering::SeqCst),
            "the future is not polled once the token is cancelled"
        );
    }

    #[test]
    fn block_on_cancellable_returns_value_when_not_cancelled() {
        let rt = Runtime::new().expect("runtime builds");
        let res = rt.block_on_cancellable(&CancelToken::new(), async { 3 });
        assert_eq!(
            res.expect("not cancelled"),
            3,
            "the future's output is returned"
        );
    }

    #[test]
    fn last_clone_dropped_inside_a_task_does_not_panic() {
        let rt = Runtime::new().expect("runtime builds");
        let (tx, rx) = std::sync::mpsc::channel();
        let in_task = rt.clone();
        rt.handle().spawn(async move {
            // Wait until the outer handle is gone, so this drop is the last one.
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(in_task);
            tx.send(()).ok();
        });
        drop(rt);
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the task dropped the last runtime handle without panicking");
    }
}

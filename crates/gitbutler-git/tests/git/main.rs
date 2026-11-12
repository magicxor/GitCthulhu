mod refspec;

mod silent_remote {
    use std::{net::TcpListener, time::Duration};

    use but_testsupport::gix_testtools::tempfile::TempDir;
    use gitbutler_git::tokio::TokioExecutor;

    /// A remote that accepts the connection and then never answers must fail the fetch
    /// instead of hanging it, and everything queued behind it, forever.
    ///
    /// The repository sets `http.lowSpeedTime` to keep the test short. Git ignores it unless
    /// `http.lowSpeedLimit` is set too, so the fetch only gives up if that default is supplied.
    #[tokio::test]
    async fn fetch_from_http_remote_that_never_answers_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accept every connection and keep it open without ever responding.
        std::thread::spawn(move || listener.incoming().collect::<Vec<_>>());

        let tmp = TempDir::new().unwrap();
        // Plain `git` invocations: no shell is needed (and `bash` may be a WSL shim on Windows).
        let url = format!("http://127.0.0.1:{port}/repo.git");
        for args in [
            vec!["init", "-q"],
            vec!["remote", "add", "origin", url.as_str()],
            vec!["config", "http.lowSpeedTime", "2"],
        ] {
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(tmp.path())
                .status()
                .expect("git can be spawned");
            assert!(status.success(), "git {args:?} succeeds");
        }

        let err = tokio::time::timeout(
            Duration::from_secs(30),
            gitbutler_git::fetch(
                tmp.path(),
                TokioExecutor,
                "origin",
                None::<fn(String) -> std::future::Ready<Option<String>>>,
            ),
        )
        .await
        .expect("a fetch from a remote that never answers gives up on its own")
        .unwrap_err();
        assert!(
            err.to_string().contains("Operation too slow"),
            "Git aborts the stalled transfer: {err}"
        );
    }
}

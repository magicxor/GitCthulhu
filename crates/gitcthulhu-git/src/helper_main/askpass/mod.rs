//! The askpass helper program: Git (or OpenSSH) starts it as `SSH_ASKPASS` with the prompt as first argument; it
//! forwards the prompt to the application's askpass server and prints the answer to stdout.
//!
//! Environment: `GITCTHULHU_ASKPASS_PIPE` (socket path or pipe name) and `GITCTHULHU_ASKPASS_SECRET`, both set by
//! `gitbutler_git::askpass_harness::AskpassSession::env()`.
//!
//! The wire protocol is line based: secret, prompt (see `gitbutler_git::askpass_harness::encode_prompt`), then the
//! server's one-line answer. A server that closes the connection without an answer means "refused": the helper exits
//! with a failure status and prints nothing, so Git fails by itself.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_pipe;

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::ExitCode,
};

use gitbutler_git::askpass_harness::encode_prompt;

#[cfg(unix)]
use self::unix::connect;
#[cfg(windows)]
use self::windows::connect;

const NOT_FOR_HUMANS: &str =
    "this program is only meant to be started by GitCthulhu's Git integration";

pub(super) fn run() -> ExitCode {
    let (Some(pipe), Some(secret)) = (
        std::env::var_os("GITCTHULHU_ASKPASS_PIPE"),
        std::env::var_os("GITCTHULHU_ASKPASS_SECRET"),
    ) else {
        eprintln!(
            "{NOT_FOR_HUMANS} (missing GITCTHULHU_ASKPASS_PIPE or GITCTHULHU_ASKPASS_SECRET)"
        );
        return ExitCode::FAILURE;
    };
    let Some(prompt) = std::env::args_os().nth(1) else {
        eprintln!("{NOT_FOR_HUMANS} (missing prompt argument)");
        return ExitCode::FAILURE;
    };
    let (pipe, secret, prompt) = (
        pipe.to_string_lossy(),
        secret.to_string_lossy(),
        prompt.to_string_lossy(),
    );

    let (reader, writer) = match connect(&pipe) {
        Ok(halves) => halves,
        Err(err) => {
            eprintln!("gitcthulhu-askpass: cannot connect to the application: {err}");
            return ExitCode::FAILURE;
        }
    };
    match exchange(reader, writer, &secret, &prompt) {
        Ok(Some(answer)) => {
            let mut stdout = io::stdout().lock();
            // `answer` has its line break already.
            match stdout
                .write_all(answer.as_bytes())
                .and_then(|()| stdout.flush())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("gitcthulhu-askpass: cannot write the answer: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        // The application refused to answer.
        Ok(None) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("gitcthulhu-askpass: communication with the application failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Send `secret` and `prompt`, then read the answer line (with its line break). `None` if the server closed the
/// connection without answering.
fn exchange<R: Read, W: Write>(
    reader: R,
    mut writer: W,
    secret: &str,
    prompt: &str,
) -> io::Result<Option<String>> {
    writer.write_all(format!("{secret}\n{}\n", encode_prompt(prompt)).as_bytes())?;
    writer.flush()?;

    let mut answer = String::new();
    if BufReader::new(reader).read_line(&mut answer)? == 0 {
        return Ok(None);
    }
    if !answer.ends_with('\n') {
        answer.push('\n');
    }
    Ok(Some(answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_sends_secret_and_single_line_prompt_and_returns_the_answer() {
        let mut sent = Vec::new();
        let answer = exchange(&b"hunter2\n"[..], &mut sent, "s3cret", "a\nb\r\nc")
            .expect("in-memory i/o works");
        assert_eq!(
            String::from_utf8(sent).expect("utf-8"),
            "s3cret\na\u{1F}b\u{1F}c\n",
            "secret first, then the prompt on one line"
        );
        assert_eq!(
            answer.as_deref(),
            Some("hunter2\n"),
            "the answer is relayed"
        );
    }

    #[test]
    fn closed_connection_means_refusal() {
        let mut sent = Vec::new();
        let answer = exchange(&b""[..], &mut sent, "s", "p").expect("in-memory i/o works");
        assert_eq!(answer, None, "no answer line => refused");
    }
}

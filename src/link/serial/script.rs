//! Scripted UART input for the serial monitor (`-m` with `--uart-in`).
//!
//! The script's bytes go to the board a line at a time, a byte at a time with
//! a short gap (the board's UART receiver is polled and its FIFO is small).
//! With a prompt, each line waits until the board has printed the prompt since
//! the previous line was sent.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// The most board output kept for prompt matching.
const RECEIVED_LIMIT: usize = 64 * 1024;

/// A script to type at the monitored board.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Script<'a> {
    /// The bytes to send.
    pub(crate) bytes: &'a [u8],
    /// Send each line only after the board prints this (empty: don't wait).
    pub(crate) prompt: &'a [u8],
}

/// The delays between script bytes and lines.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pacing {
    /// Between bytes of a line.
    pub(crate) byte_gap: Duration,
    /// After each line but the last, when there is no prompt to wait for.
    pub(crate) line_gap: Duration,
    /// Between looks at the received output while waiting for the prompt.
    pub(crate) poll: Duration,
}

impl Pacing {
    /// The delays used with the board.
    pub(crate) const BOARD: Self = Self {
        byte_gap: Duration::from_millis(3),
        line_gap: Duration::from_millis(200),
        poll: Duration::from_millis(5),
    };
}

/// How a script run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScriptEnd {
    /// Every line was sent (and, with a prompt, the prompt came back after the last).
    Finished,
    /// The monitor stopped first, or the board could not be written to.
    Stopped,
}

/// The tail of what the board has sent, shared between the monitor loop (which
/// adds to it) and the script (which looks for the prompt).
#[derive(Debug, Default)]
pub(crate) struct Received(Mutex<Vec<u8>>);

impl Received {
    /// Append board output, keeping only the last [`RECEIVED_LIMIT`] bytes.
    pub(crate) fn push(&self, data: &[u8]) {
        let mut buf = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        buf.extend_from_slice(data);
        if buf.len() > RECEIVED_LIMIT {
            let excess = buf.len() - RECEIVED_LIMIT;
            buf.drain(..excess);
        }
    }

    /// Whether `prompt` has been received; if so, forget everything received.
    pub(crate) fn take_prompt(&self, prompt: &[u8]) -> bool {
        let mut buf = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let found = prompt.is_empty() || buf.windows(prompt.len()).any(|w| w == prompt);
        if found {
            buf.clear();
        }
        found
    }
}

/// The script's lines, each with its `\n` (the last may lack one).
pub(crate) fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|&b| b == b'\n').collect()
}

/// Sleep for `total`, in short steps so a stop is noticed.
fn sleep_while_running(total: Duration, running: &AtomicBool) {
    let step = Duration::from_millis(20);
    let mut left = total;
    while !left.is_zero() && running.load(Ordering::Relaxed) {
        let now = left.min(step);
        thread::sleep(now);
        left -= now;
    }
}

/// Wait until `prompt` has been received (and forget it); false if stopped first.
fn wait_for_prompt(prompt: &[u8], received: &Received, running: &AtomicBool, poll: Duration) -> bool {
    while running.load(Ordering::Relaxed) {
        if received.take_prompt(prompt) {
            return true;
        }
        thread::sleep(poll);
    }
    false
}

/// Send `script` to `port`, pacing it as described in the module docs, while
/// `running` stays set.
pub(crate) fn run_script(script: Script<'_>, received: &Received, port: &mut dyn Write, running: &AtomicBool, pacing: Pacing) -> ScriptEnd {
    let lines = split_lines(script.bytes);
    let waits = !script.prompt.is_empty();
    for (index, line) in lines.iter().enumerate() {
        if waits && !wait_for_prompt(script.prompt, received, running, pacing.poll) {
            return ScriptEnd::Stopped;
        }
        for byte in *line {
            if !running.load(Ordering::Relaxed) || port.write_all(&[*byte]).and_then(|()| port.flush()).is_err() {
                return ScriptEnd::Stopped;
            }
            thread::sleep(pacing.byte_gap);
        }
        if !waits && index + 1 < lines.len() {
            sleep_while_running(pacing.line_gap, running);
        }
    }
    if waits && !wait_for_prompt(script.prompt, received, running, pacing.poll) {
        return ScriptEnd::Stopped;
    }
    ScriptEnd::Finished
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const FAST: Pacing = Pacing {
        byte_gap: Duration::ZERO,
        line_gap: Duration::ZERO,
        poll: Duration::from_millis(1),
    };

    #[test]
    fn lines_keep_their_newlines() {
        assert_eq!(split_lines(b"ls\npwd\n"), vec![&b"ls\n"[..], &b"pwd\n"[..]]);
        assert_eq!(split_lines(b"a\n\nb"), vec![&b"a\n"[..], &b"\n"[..], &b"b"[..]]);
        assert_eq!(split_lines(b"").len(), 0);
    }

    #[test]
    fn prompt_match_clears_the_buffer() {
        let rx = Received::default();
        assert!(!rx.take_prompt(b"$ "));
        rx.push(b"hello\r\n$");
        assert!(!rx.take_prompt(b"$ "));
        rx.push(b" ");
        assert!(rx.take_prompt(b"$ "), "prompt split across pushes");
        assert!(!rx.take_prompt(b"$ "), "consumed");
        assert!(rx.take_prompt(b""), "empty prompt always matches");
    }

    #[test]
    fn received_buffer_is_bounded() {
        let rx = Received::default();
        rx.push(b"$ ");
        rx.push(&vec![b'x'; RECEIVED_LIMIT]);
        assert!(!rx.take_prompt(b"$ "), "old output fell off the front");
        rx.push(b"$ ");
        assert!(rx.0.lock().unwrap().len() <= RECEIVED_LIMIT);
        assert!(rx.take_prompt(b"$ "));
    }

    #[test]
    fn script_without_prompt_sends_everything() {
        let rx = Received::default();
        let running = AtomicBool::new(true);
        let mut out = Vec::new();
        let script = Script {
            bytes: b"a\nb\n",
            prompt: b"",
        };
        assert_eq!(run_script(script, &rx, &mut out, &running, FAST), ScriptEnd::Finished);
        assert_eq!(out, b"a\nb\n");
    }

    #[test]
    fn script_with_prompt_waits_for_each_prompt() {
        let rx = Arc::new(Received::default());
        let running = Arc::new(AtomicBool::new(true));
        let out = Arc::new(Mutex::new(Vec::new()));

        let handle = {
            let (rx, running, out) = (Arc::clone(&rx), Arc::clone(&running), Arc::clone(&out));
            thread::spawn(move || {
                let script = Script {
                    bytes: b"a\nb\n",
                    prompt: b"$ ",
                };
                run_script(script, &rx, &mut Shared(out), &running, FAST)
            })
        };
        let sent = || out.lock().unwrap().clone();
        thread::sleep(Duration::from_millis(30));
        assert!(sent().is_empty(), "nothing before the first prompt");
        rx.push(b"$ ");
        while sent() != b"a\n" {
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(30));
        assert_eq!(sent(), b"a\n", "second line waits for another prompt");
        rx.push(b"a\r\n$ ");
        while sent() != b"a\nb\n" {
            thread::sleep(Duration::from_millis(1));
        }
        thread::sleep(Duration::from_millis(30));
        assert!(!handle.is_finished(), "waits for the prompt after the last line");
        rx.push(b"$ ");
        assert_eq!(handle.join().unwrap(), ScriptEnd::Finished);
    }

    #[test]
    fn stopping_ends_a_waiting_script() {
        let rx = Received::default();
        let running = AtomicBool::new(false);
        let mut out = Vec::new();
        let script = Script {
            bytes: b"a\n",
            prompt: b"$ ",
        };
        assert_eq!(run_script(script, &rx, &mut out, &running, FAST), ScriptEnd::Stopped);
        assert_eq!(out.len(), 0);
    }
}

//! Forward Claude Code hook events from the box to a host-side command.
//!
//! iTerm2's Claude Code integration is a hook, `~/.config/iterm2/cc-status`:
//! a signed macOS binary that drives iTerm2 through `it2` and its API socket.
//! Neither can run in (or reach from) the Linux guest, so the in-box hook
//! (`custom/cbox-hook.sh`) instead hands each event back to Claude Code as a
//! hook `terminalSequence` -- `OSC 777 ; cbox-hook ; <base64 JSON> BEL`. Claude
//! writes that to its terminal, so it rides the stdout stream cbox already
//! pumps. `Forwarder::filter` strips it back out and feeds the JSON to the
//! host command on stdin, which runs in this process's environment: the
//! host terminal's own `ITERM_SESSION_ID`, for `up` and `exec` alike.
//!
//! Anything in the box that can write to the terminal can forge one of these,
//! so the host command must treat its input as untrusted; for cc-status the
//! worst case is a spoofed tab status.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const MARKER: &[u8] = b"\x1b]777;cbox-hook;";
/// Claude Code caps a hook's terminalSequence at 4096 bytes, so a longer
/// "payload" is not one of ours: stop buffering and pass it through.
const MAX_PAYLOAD: usize = 8192;
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on how long ending a session waits for queued events.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Streaming extractor for marker sequences, which can straddle chunks.
#[derive(Default)]
struct HookFilter {
    buf: Vec<u8>,
    in_seq: bool,
}

impl HookFilter {
    /// Returns the bytes to show; decoded event payloads go to `events`.
    fn feed(&mut self, input: &[u8], events: &mut Vec<Vec<u8>>) -> Vec<u8> {
        self.buf.extend_from_slice(input);
        let mut out = Vec::with_capacity(self.buf.len());
        loop {
            if self.in_seq {
                let end = self.buf.iter().enumerate().find_map(|(i, &b)| match b {
                    0x07 => Some((i, 1)),
                    0x1b if self.buf.get(i + 1) == Some(&b'\\') => Some((i, 2)),
                    _ => None,
                });
                match end {
                    Some((i, term_len)) => {
                        if let Ok(json) = BASE64.decode(&self.buf[..i]) {
                            events.push(json);
                        }
                        self.buf.drain(..i + term_len);
                        self.in_seq = false;
                    }
                    None if self.buf.len() > MAX_PAYLOAD => {
                        out.extend_from_slice(MARKER);
                        out.append(&mut self.buf);
                        self.in_seq = false;
                        return out;
                    }
                    None => return out,
                }
            } else if let Some(i) = self.buf.windows(MARKER.len()).position(|w| w == MARKER) {
                out.extend_from_slice(&self.buf[..i]);
                self.buf.drain(..i + MARKER.len());
                self.in_seq = true;
            } else {
                // Hold back a tail that could be the start of a marker split
                // across chunks; the rest of any real escape sequence follows
                // in the next chunk, so this only ever delays a few bytes.
                let keep = (1..MARKER.len())
                    .rev()
                    .find(|&n| self.buf.ends_with(&MARKER[..n]))
                    .unwrap_or(0);
                let cut = self.buf.len() - keep;
                out.extend(self.buf.drain(..cut));
                return out;
            }
        }
    }
}

pub struct Forwarder {
    filter: HookFilter,
    worker: Option<(mpsc::UnboundedSender<Vec<u8>>, JoinHandle<()>)>,
}

impl Forwarder {
    /// Must be called inside a tokio runtime: events run on a spawned task,
    /// one at a time, so a PreToolUse can never land after its PostToolUse.
    pub fn spawn() -> Self {
        let worker = hook_command().map(|cmd| {
            let (tx, rx) = mpsc::unbounded_channel();
            (tx, tokio::spawn(run(cmd, rx)))
        });
        Self { filter: HookFilter::default(), worker }
    }

    /// Let queued events finish before the session returns. The final Stop
    /// arrives just before the guest exits, and the callers exit (or drop the
    /// runtime, killing `kill_on_drop` children) right after -- without this
    /// the tab is left showing "working".
    pub async fn finish(self) {
        if let Some((tx, handle)) = self.worker {
            drop(tx);
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, handle).await;
        }
    }

    /// Strip hook markers out of guest output, forwarding their payloads.
    /// Markers are stripped even with no host command configured.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut events = Vec::new();
        let out = self.filter.feed(input, &mut events);
        if let Some((tx, _)) = &self.worker {
            for event in events {
                let _ = tx.send(event);
            }
        }
        out
    }
}

/// `CBOX_HOOK_COMMAND` overrides the host command (set it empty to disable);
/// otherwise iTerm2's cc-status, if it's installed.
fn hook_command() -> Option<PathBuf> {
    if let Some(cmd) = std::env::var_os("CBOX_HOOK_COMMAND") {
        return (!cmd.is_empty()).then(|| PathBuf::from(cmd));
    }
    let default = PathBuf::from(std::env::var_os("HOME")?).join(".config/iterm2/cc-status");
    default.is_file().then_some(default)
}

async fn run(cmd: PathBuf, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(json) = rx.recv().await {
        // stdout/stderr go nowhere: this process owns the terminal the
        // guest is drawing on.
        let child = tokio::process::Command::new(&cmd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn();
        let Ok(mut child) = child else { continue };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&json).await;
        }
        let _ = tokio::time::timeout(HOOK_TIMEOUT, child.wait()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(json: &str, term: &[u8]) -> Vec<u8> {
        [MARKER, BASE64.encode(json).as_bytes(), term].concat()
    }

    #[test]
    fn plain_output_passes_through_untouched() {
        let mut f = HookFilter::default();
        let mut ev = Vec::new();
        assert_eq!(f.feed(b"hello \x1b]0;title\x07world", &mut ev), b"hello \x1b]0;title\x07world");
        assert!(ev.is_empty());
    }

    #[test]
    fn a_marker_is_stripped_and_decoded_with_either_terminator() {
        for term in [&b"\x07"[..], b"\x1b\\"] {
            let mut f = HookFilter::default();
            let mut ev = Vec::new();
            let input = [&b"a"[..], &seq(r#"{"e":1}"#, term), b"b"].concat();
            assert_eq!(f.feed(&input, &mut ev), b"ab");
            assert_eq!(ev, vec![br#"{"e":1}"#.to_vec()]);
        }
    }

    #[test]
    fn a_marker_split_at_every_position_is_still_extracted() {
        let input = [&b"x"[..], &seq(r#"{"hook_event_name":"Stop"}"#, b"\x07"), b"y"].concat();
        for cut in 0..=input.len() {
            let mut f = HookFilter::default();
            let mut ev = Vec::new();
            let mut out = f.feed(&input[..cut], &mut ev);
            out.extend(f.feed(&input[cut..], &mut ev));
            assert_eq!(out, b"xy", "cut at {cut}");
            assert_eq!(ev, vec![br#"{"hook_event_name":"Stop"}"#.to_vec()], "cut at {cut}");
        }
    }

    #[test]
    fn an_unterminated_marker_is_eventually_passed_through() {
        let mut f = HookFilter::default();
        let mut ev = Vec::new();
        let junk = vec![b'A'; MAX_PAYLOAD + 1];
        let out = f.feed(&[MARKER, &junk].concat(), &mut ev);
        assert_eq!(out, [MARKER, &junk].concat());
        assert!(ev.is_empty());
        assert_eq!(f.feed(b"after", &mut ev), b"after");
    }

    #[tokio::test]
    async fn finish_waits_for_queued_events_before_returning() {
        // The last events (Stop, SessionEnd) arrive just before the session
        // ends; finish() must deliver them rather than drop the queue.
        let dir = std::env::temp_dir().join(format!("cbox-hookfwd-drain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let cmd = dir.join("slow-hook");
        // Not written in-process: a test on another thread that forks at the
        // wrong moment would inherit our write fd to the script, and Linux
        // refuses to exec a file open for writing (ETXTBSY), which `run`
        // swallows, so the log never appears. Writing a plain data file and
        // letting a `cp` child create the script keeps the write fd out of
        // this process entirely.
        let src = dir.join("slow-hook.src");
        std::fs::write(&src, format!("#!/bin/sh\nsleep 0.2\ncat >> '{}'\necho >> '{}'\n", log.display(), log.display()))
            .unwrap();
        assert!(std::process::Command::new("cp").arg(&src).arg(&cmd).status().unwrap().success());
        std::fs::set_permissions(&cmd, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let (tx, rx) = mpsc::unbounded_channel();
        let mut fwd =
            Forwarder { filter: HookFilter::default(), worker: Some((tx, tokio::spawn(run(cmd, rx)))) };
        let input = [seq(r#"{"e":"Stop"}"#, b"\x07"), seq(r#"{"e":"SessionEnd"}"#, b"\x07")].concat();
        assert!(fwd.filter(&input).is_empty());
        fwd.finish().await;

        assert_eq!(std::fs::read_to_string(&log).unwrap(), "{\"e\":\"Stop\"}\n{\"e\":\"SessionEnd\"}\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_payload_that_is_not_base64_is_dropped() {
        let mut f = HookFilter::default();
        let mut ev = Vec::new();
        assert_eq!(f.feed(&[MARKER, b"!!not base64!!\x07ok"].concat(), &mut ev), b"ok");
        assert!(ev.is_empty());
    }
}

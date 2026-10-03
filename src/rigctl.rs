//! Rig PTT via rigctld: a background poller thread keeps a TCP connection to
//! a rigctld server (e.g. `rigctld-wsjtx.exe`), asks for the PTT state once
//! per poll, and publishes the result to the audio path and the GUI.
//!
//! Wire format (rigctld "default" protocol: no banner on connect, no ACK):
//! the client sends the short PTT command `t\n`; the server answers with one
//! line carrying the PTT state - `0` RX, `1` TX, `2` TX mic, `3` TX data - or
//! `RPRT <n>` when the rig itself reports an error.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Published PTT state: `0` = RX, `1` = TX, `2` = TX mic, `3` = TX data,
/// `DISCONNECTED` = no live rigctld connection.
pub const DISCONNECTED: u8 = u8::MAX;

/// The rig is keyed for any of these states (transmit in any mode).
pub fn is_keyed(state: u8) -> bool {
    matches!(state, 1..=3)
}

/// How often to ask for the PTT state while connected.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How often to retry while a target is set but the connection is down.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
/// How long to wait for a rigctld answer before declaring the connection
/// dead. Rigs polled over a serial line can be slow, so keep this generous.
const READ_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a TCP connect may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// PTT state shared between the poller thread, the capture callback, and the
/// GUI.
pub struct Rigctl {
    /// Last PTT state (see DISCONNECTED). Written by the poller thread; the
    /// capture callback reads it with a Relaxed load, once per frame.
    pub ptt: Arc<AtomicU8>,
    /// (host, port) the poller thread should be connected to; None means no
    /// rig is being tracked. Written by the GUI thread, read by the poller.
    target: Arc<Mutex<Option<(String, u16)>>>,
    /// One-line status for the GUI (never touched by the audio path).
    pub message: Arc<Mutex<String>>,
}

impl Rigctl {
    pub fn new() -> Self {
        Self {
            ptt: Arc::new(AtomicU8::new(DISCONNECTED)),
            target: Arc::default(),
            message: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Start the background poller thread (detached; lives for the process).
    pub fn spawn_poller(&self) {
        let ptt = Arc::clone(&self.ptt);
        let target = Arc::clone(&self.target);
        let message = Arc::clone(&self.message);
        thread::spawn(move || poll_loop(&ptt, &target, &message));
    }

    /// Point the poller thread at a rigctld server.
    pub fn connect_to(&self, host: &str, port: u16) {
        *self.target.lock().unwrap() = Some((host.to_string(), port));
    }

    /// Tell the poller thread to drop the connection.
    pub fn disconnect(&self) {
        *self.target.lock().unwrap() = None;
    }

    /// Whether an endpoint is currently being tracked (GUI thread only).
    pub fn has_target(&self) -> bool {
        self.target.lock().unwrap().is_some()
    }
}

fn poll_loop(ptt: &AtomicU8, target: &Mutex<Option<(String, u16)>>, message: &Mutex<String>) {
    let mut stream: Option<TcpStream> = None;
    loop {
        let wait = match target.lock().unwrap().clone() {
            None => {
                if stream.take().is_some() {
                    set_message(message, "rigctld: disconnected");
                }
                ptt.store(DISCONNECTED, Ordering::Relaxed);
                RECONNECT_INTERVAL
            }
            Some((host, port)) => {
                let alive = match stream.as_mut() {
                    Some(s) => match read_ptt(s) {
                        Ok(Some(v)) => {
                            ptt.store(v, Ordering::Relaxed);
                            true
                        }
                        // The rig reported an error; the TCP link is still
                        // fine, so keep the last state and go on polling.
                        Ok(None) => {
                            set_message(message, "rigctld: rig error, holding last PTT state");
                            true
                        }
                        Err(e) => {
                            stream = None;
                            ptt.store(DISCONNECTED, Ordering::Relaxed);
                            set_message(message, &format!("rigctld: {e}"));
                            false
                        }
                    },
                    None => match connect(&host, port) {
                        Ok(s) => {
                            set_message(
                                message,
                                &format!("rigctld: connected to {host}:{port}"),
                            );
                            stream = Some(s);
                            true
                        }
                        Err(e) => {
                            ptt.store(DISCONNECTED, Ordering::Relaxed);
                            set_message(message, &format!("rigctld: {host}:{port}: {e}"));
                            false
                        }
                    },
                };
                if alive { POLL_INTERVAL } else { RECONNECT_INTERVAL }
            }
        };
        thread::sleep(wait);
    }
}

fn set_message(message: &Mutex<String>, text: &str) {
    *message.lock().unwrap() = text.to_string();
}

fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("no address for host")?;
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Send the short PTT command and parse the one-line answer. `Ok(Some(v))`
/// is a new PTT state, `Ok(None)` means the rig reported an error (keep the
/// last state), `Err` means the connection is dead.
fn read_ptt(stream: &mut TcpStream) -> Result<Option<u8>, String> {
    stream.write_all(b"t\n").map_err(|e| e.to_string())?;
    let line = read_line(stream)?;
    parse_ptt_line(&line)
}

/// Read until the end of the current line (responses are `\n`-terminated).
fn read_line(stream: &mut TcpStream) -> Result<String, String> {
    let mut line = Vec::with_capacity(16);
    let mut buf = [0u8; 64];
    loop {
        let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".to_string());
        }
        match buf[..n].iter().position(|&b| b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&buf[..i]);
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            None => {
                line.extend_from_slice(&buf);
                if line.len() > 128 {
                    return Err("malformed response".to_string());
                }
            }
        }
    }
}

/// Parse one PTT answer line: a bare `0`-`3`, or `RPRT <n>` when the rig
/// reports an error.
fn parse_ptt_line(line: &str) -> Result<Option<u8>, String> {
    match line {
        "0" => Ok(Some(0)),
        "1" => Ok(Some(1)),
        "2" => Ok(Some(2)),
        "3" => Ok(Some(3)),
        l if l.starts_with("RPRT") => Ok(None),
        other => Err(format!("unexpected response: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ptt_states() {
        assert_eq!(parse_ptt_line("0"), Ok(Some(0)));
        assert_eq!(parse_ptt_line("1"), Ok(Some(1)));
        assert_eq!(parse_ptt_line("2"), Ok(Some(2)));
        assert_eq!(parse_ptt_line("3"), Ok(Some(3)));
    }

    #[test]
    fn rpt_means_keep_last_state() {
        assert_eq!(parse_ptt_line("RPRT -5"), Ok(None));
        assert_eq!(parse_ptt_line("RPRT -110"), Ok(None));
    }

    #[test]
    fn anything_else_is_malformed() {
        assert!(parse_ptt_line("").is_err());
        assert!(parse_ptt_line("4").is_err());
        assert!(parse_ptt_line("garbage").is_err());
    }

    #[test]
    fn disconnected_is_not_a_ptt_state() {
        assert!(!matches!(DISCONNECTED, 0..=3));
        assert!(!is_keyed(DISCONNECTED));
    }
}

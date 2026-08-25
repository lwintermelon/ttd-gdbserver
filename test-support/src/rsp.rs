//! Minimal GDB RSP client shared by all integration test targets.
//!
//! Sending a packet and decoding the reply (escapes + RLE) is fiddly enough
//! that re-implementing it per test file would hide protocol mistakes. The
//! client also carries the higher-level helpers the suites use to poke
//! Delve/custom commands (`qRRCmd`, `monitor`, `vRun`).

use std::io::{Read, Write};
use std::net::TcpStream;

#[allow(dead_code)]
pub struct RspClient {
    pub stream: TcpStream,
}

#[allow(dead_code)]
impl RspClient {
    pub fn connect(addr: std::net::SocketAddr) -> Self {
        Self {
            stream: TcpStream::connect(addr).unwrap(),
        }
    }

    /// Send `$payload#ck`.
    pub fn send(&mut self, payload: &str) {
        let mut pkt = String::with_capacity(payload.len() + 4);
        pkt.push('$');
        pkt.push_str(payload);
        let sum: u8 = payload.bytes().fold(0u8, |a, b| a.wrapping_add(b));
        pkt.push('#');
        pkt.push_str(&format!("{:02x}", sum));
        self.stream.write_all(pkt.as_bytes()).unwrap();
    }

    /// Read one packet response, skipping bare +/- acks. Returns the
    /// decoded body (escapes undone, run-length expanded).
    pub fn recv(&mut self) -> Vec<u8> {
        let mut b = [0u8; 1];
        loop {
            self.stream.read_exact(&mut b).unwrap();
            if b[0] == b'$' {
                break;
            }
            assert!(b[0] == b'+' || b[0] == b'-', "unexpected byte {:#x}", b[0]);
        }
        let mut body = Vec::new();
        loop {
            self.stream.read_exact(&mut b).unwrap();
            if b[0] == b'#' {
                break;
            }
            body.push(b[0]);
        }
        let mut ck = [0u8; 2];
        self.stream.read_exact(&mut ck).unwrap();
        let sum: u8 = body.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(
            format!("{:02x}", sum),
            std::str::from_utf8(&ck).unwrap().to_lowercase()
        );
        decode(&body)
    }

    pub fn roundtrip(&mut self, payload: &str) -> String {
        self.send(payload);
        String::from_utf8(self.recv()).unwrap()
    }

    /// Send `payload` and return the raw decoded reply bytes.
    pub fn roundtrip_bytes(&mut self, payload: &str) -> Vec<u8> {
        self.send(payload);
        self.recv()
    }

    /// Send a bare ACK byte (`+`) ahead of a raw packet. Most tests run
    /// after `QStartNoAckMode`, but some RSP clients (and deliberately
    /// low-level tests) send it anyway; gdbstub accepts it as a no-op.
    pub fn ack(&mut self) {
        self.stream.write_all(b"+").unwrap();
    }

    /// Send a `qRRCmd` body and decode the hex-encoded textual reply.
    pub fn qrrcmd(&mut self, body: &str) -> String {
        let reply = self.roundtrip(&format!("qRRCmd:{body}"));
        String::from_utf8(from_hex(&reply)).expect("qRRCmd reply must be hex text")
    }

    /// Send a `qRRCmd` whose command is itself hex-encoded (the old
    /// Delve client form) and decode the textual reply.
    pub fn qrrcmd_hex(&mut self, cmd: &str) -> String {
        let encoded: String = cmd.bytes().map(|b| format!("{:02x}", b)).collect();
        self.qrrcmd(&encoded)
    }

    /// `qRRCmd when` — the current trace position, formatted `SEQ:STEPS`.
    pub fn when(&mut self) -> String {
        self.qrrcmd("when:-1")
    }

    /// Send Delve's restart form `vRun;;<hex(ascii argument)>` and return
    /// the raw stop reply.
    pub fn vrun(&mut self, arg: &str) -> String {
        let hex: String = arg.bytes().map(|b| format!("{:02x}", b)).collect();
        self.roundtrip(&format!("vRun;;{hex}"))
    }

    /// Send a `monitor` (`qRcmd`) command and accumulate its console
    /// output. gdb merges the `O<hex>` chunks into one console line, so
    /// no separator is added.
    pub fn monitor(&mut self, cmd: &str) -> String {
        let hex: String = cmd.bytes().map(|b| format!("{:02x}", b)).collect();
        self.ack();
        self.send(&format!("qRcmd,{hex}"));

        let mut text = String::new();
        loop {
            let body = self.recv();
            let s = std::str::from_utf8(&body).unwrap_or("");
            if s == "OK" {
                break;
            }
            if let Some(rest) = s.strip_prefix('O') {
                if rest.len() % 2 == 0 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                    text.push_str(&String::from_utf8_lossy(&from_hex(rest)));
                } else {
                    text.push_str(rest);
                }
            } else {
                text.push_str(s);
                break;
            }
        }
        text
    }
}

/// Undo `}` escapes and `*` run-length (mirrors the client side of RSP).
#[allow(dead_code)]
pub fn decode(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'}' if i + 1 < body.len() => {
                out.push(body[i + 1] ^ 0x20);
                i += 2;
            }
            b'*' if i + 1 < body.len() && !out.is_empty() => {
                let n = (body[i + 1].wrapping_sub(29)) as usize;
                let last = out[out.len() - 1];
                out.extend(std::iter::repeat_n(last, n));
                i += 2;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[allow(dead_code)]
pub fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

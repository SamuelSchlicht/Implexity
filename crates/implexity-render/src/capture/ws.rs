// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use base64::Engine as _;
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    mask_seed: u64,
}

pub(crate) enum Incoming {
    Text(String),
    Idle,
}

fn io_err(m: impl Into<String>) -> std::io::Error {
    std::io::Error::other(m.into())
}

impl Client {
    pub(crate) fn connect(url: &str) -> std::io::Result<Self> {
        let rest = url.strip_prefix("ws://").ok_or_else(|| io_err("DevTools URL is not ws://"))?;
        let (authority, path) =
            rest.split_once('/').map_or((rest, "/".to_owned()), |(a, p)| (a, format!("/{p}")));
        let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
        if !matches!(host, "127.0.0.1" | "[::1]" | "localhost") {
            return Err(io_err("DevTools endpoint is not loopback"));
        }
        let stream = TcpStream::connect(authority)?;
        stream.set_nodelay(true)?;
        let seed = u64::from_le_bytes(
            implexity_io::atomic::os_random_bytes(8)
                .ok()
                .and_then(|b| b.try_into().ok())
                .unwrap_or_else(|| (u64::from(std::process::id()) << 17).to_le_bytes()),
        );
        let key_bytes = implexity_io::atomic::os_random_bytes(16).unwrap_or_else(|_| {
            seed.to_le_bytes().iter().chain(seed.to_be_bytes().iter()).copied().collect()
        });
        let key = base64::engine::general_purpose::STANDARD.encode(&key_bytes);
        let mut writer = stream.try_clone()?;
        write!(
            writer,
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        writer.flush()?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status)?;
        if !status.starts_with("HTTP/1.1 101") {
            return Err(io_err(format!("DevTools handshake refused: {}", status.trim())));
        }

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(io_err("DevTools closed the connection during the handshake"));
            }
            if line.trim_end().is_empty() {
                break;
            }
        }
        Ok(Self { reader, writer, mask_seed: seed })
    }

    fn next_mask(&mut self) -> [u8; 4] {

        let mut x = self.mask_seed;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.mask_seed = x;
        let m = x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
        [m[0], m[1], m[2], m[3]]
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let n = payload.len();
        if n < 126 {
            frame.push(0x80 | u8::try_from(n).unwrap_or(0));
        } else if n <= 0xffff {
            frame.push(0x80 | 0x7e);
            frame.extend_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
        } else {
            frame.push(0x80 | 0x7f);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask = self.next_mask();
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.writer.write_all(&frame)?;
        self.writer.flush()
    }

    pub(crate) fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        self.send_frame(0x1, text.as_bytes())
    }

    pub(crate) fn recv(&mut self, timeout: Duration) -> std::io::Result<Incoming> {
        let mut message: Vec<u8> = Vec::new();
        loop {
            self.reader.get_ref().set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
            let mut head = [0u8; 2];
            match self.reader.read_exact(&mut head) {
                Ok(()) => {}
                Err(e)
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    if message.is_empty() {
                        return Ok(Incoming::Idle);
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }

            self.reader.get_ref().set_read_timeout(Some(Duration::from_mins(1)))?;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0f;
            if head[1] & 0x80 != 0 {
                return Err(io_err("DevTools sent a masked frame"));
            }
            let len = match head[1] & 0x7f {
                126 => {
                    let mut b = [0u8; 2];
                    self.reader.read_exact(&mut b)?;
                    usize::from(u16::from_be_bytes(b))
                }
                127 => {
                    let mut b = [0u8; 8];
                    self.reader.read_exact(&mut b)?;
                    usize::try_from(u64::from_be_bytes(b)).unwrap_or(usize::MAX)
                }
                n => usize::from(n),
            };
            if len > MAX_MESSAGE_BYTES || message.len() + len > MAX_MESSAGE_BYTES {
                return Err(io_err("DevTools message exceeds the bound"));
            }
            let mut payload = vec![0u8; len];
            self.reader.read_exact(&mut payload)?;
            match opcode {
                0x0 | 0x1 => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(message)
                            .map(Incoming::Text)
                            .map_err(|_| io_err("DevTools sent invalid UTF-8"));
                    }
                }
                0x8 => return Err(io_err("DevTools closed the connection")),
                0x9 => self.send_frame(0xA, &payload)?,
                0xA => {}
                other => return Err(io_err(format!("unexpected DevTools opcode {other}"))),
            }
        }
    }
}

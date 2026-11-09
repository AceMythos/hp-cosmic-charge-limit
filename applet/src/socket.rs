//! Tiny client for the root daemon's control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::Deserialize;

pub const SOCKET_PATH: &str = "/run/hp-charge-limit.sock";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Status {
    pub capacity: Option<u32>,
    pub status: String,
    pub ac: bool,
    pub mode: Option<u32>,
    #[serde(default)]
    pub mode_name: Option<String>,
    pub enabled: bool,
    pub start: u32,
    pub stop: u32,
    pub interval: u32,
}

pub fn call(request: &str) -> Result<Status, String> {
    let mut stream = UnixStream::connect(SOCKET_PATH)
        .map_err(|error| format!("daemon not reachable: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;

    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|error| format!("write failed: {error}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| error.to_string())?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| format!("read failed: {error}"))?;
    if line.trim().is_empty() {
        return Err("daemon closed the connection".into());
    }

    let value: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|error| format!("bad reply: {error}"))?;
    if value.get("ok").and_then(|ok| ok.as_bool()) != Some(true) {
        let message = value
            .get("error")
            .and_then(|error| error.as_str())
            .unwrap_or("daemon rejected the request");
        return Err(message.to_string());
    }
    serde_json::from_value(value).map_err(|error| format!("bad status: {error}"))
}

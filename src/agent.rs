//! The launchd agent, and how the terminal commands talk to a running daemon.
//!
//! Nothing here serves anything: `main` calls `daemon::run` directly. This is
//! the client side — the health shape both ends share, a loopback GET to read
//! it, the permanent-failure marker, and thin `launchctl` wrappers.

use crate::paths;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

/// What `GET /healthz` answers. `live` means HTTP is up; `ready` means the
/// current store is open and the startup reconcile has finished.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Health {
    pub name: String,
    pub version: String,
    pub pid: u32,
    pub state_path: String,
    pub generation: u64,
    pub live: bool,
    pub ready: bool,
    pub uptime_seconds: u64,
    pub rss_bytes: u64,
    pub files_indexed: u64,
    pub files_total: u64,
}

/// A one-line reason written when the daemon stops for something a restart will
/// not fix, so `status` can explain a silent endpoint.
pub fn permanent_failure_marker() -> Option<std::path::PathBuf> {
    paths::state_dir().ok().map(|dir| dir.join("last-permanent-failure.txt"))
}

pub fn last_permanent_failure() -> Option<String> {
    let path = permanent_failure_marker()?;
    std::fs::read_to_string(path).ok().map(|text| text.trim().to_string()).filter(|t| !t.is_empty())
}

pub fn record_permanent_failure(reason: &str) {
    if let Some(path) = permanent_failure_marker() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, reason.as_bytes());
    }
}

pub fn clear_permanent_failure() {
    if let Some(path) = permanent_failure_marker() {
        let _ = std::fs::remove_file(path);
    }
}

/// One plain HTTP GET on loopback. A dependency-free client is enough for a
/// single fixed endpoint on the same machine.
pub fn fetch_health() -> Option<Health> {
    let address = format!("127.0.0.1:{}", paths::PORT);
    let mut stream = TcpStream::connect_timeout(
        &address.parse().ok()?,
        Duration::from_millis(1500),
    )
    .ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let request = format!(
        "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
        paths::PORT
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    let text = String::from_utf8_lossy(&response);
    let body = text.split("\r\n\r\n").nth(1)?;
    serde_json::from_str(body.trim()).ok()
}

fn launchctl(arguments: &[&str]) -> Option<std::process::Output> {
    Command::new("/bin/launchctl").args(arguments).output().ok()
}

fn service_target() -> String {
    format!("gui/{}/{}", user_id(), paths::LAUNCHD_LABEL)
}

fn domain_target() -> String {
    format!("gui/{}", user_id())
}

fn user_id() -> String {
    Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn agent_is_loaded() -> bool {
    launchctl(&["print", &service_target()])
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn bootout_agent() {
    launchctl(&["bootout", &service_target()]);
}

pub fn bootstrap_agent() {
    if let Ok(plist) = paths::plist_path() {
        launchctl(&["bootstrap", &domain_target(), &plist.to_string_lossy()]);
    }
}

pub fn kickstart_agent() {
    launchctl(&["kickstart", "-k", &service_target()]);
}

/// Resident set size of a process, from `ps`. Only used for reporting.
pub fn resident_bytes(pid: u32) -> u64 {
    let Ok(output) = Command::new("/bin/ps").args(["-o", "rss=", "-p", &pid.to_string()]).output()
    else {
        return 0;
    };
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .map(|kilobytes| kilobytes * 1024)
        .unwrap_or(0)
}

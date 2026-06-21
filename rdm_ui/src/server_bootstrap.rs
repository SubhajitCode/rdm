use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const SERVER_HOST: &str = "127.0.0.1";
const SERVER_PORT: u16 = 8597;

pub fn ensure_server_running() -> Result<(), String> {
    if server_is_reachable() {
        return Ok(());
    }

    let server_bin = find_server_binary();
    Command::new(&server_bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start rdmd from {:?}: {}", server_bin, e))?;

    for _ in 0..40 {
        if server_is_reachable() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "rdmd did not become ready on {}:{} after launch",
        SERVER_HOST, SERVER_PORT
    ))
}

fn server_is_reachable() -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("{}:{}", SERVER_HOST, SERVER_PORT)
            .parse()
            .expect("valid socket address"),
        Duration::from_millis(200),
    )
    .is_ok()
}

fn find_server_binary() -> PathBuf {
    let bin_name = if cfg!(windows) { "rdmd.exe" } else { "rdmd" };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(bin_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(separator) {
            let candidate = PathBuf::from(dir).join(bin_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    PathBuf::from(bin_name)
}

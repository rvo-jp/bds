//! Isolated acceptance-test fixture; never used for a real world.
use std::{
    fs::OpenOptions,
    io::{self, BufRead, Write},
    net::TcpListener,
};
fn main() {
    let properties = std::fs::read_to_string("server.properties").unwrap();
    let port = properties
        .lines()
        .find_map(|l| l.strip_prefix("server-port="))
        .unwrap();
    let mode = std::fs::read_to_string("mode").unwrap_or_default();
    if mode.trim() == "no-listener" {
        std::fs::write("worlds/data", "new-version-write").unwrap();
    }
    let _listener = if mode.trim() == "no-listener" {
        None
    } else {
        Some(TcpListener::bind(format!("127.0.0.1:{port}")).unwrap())
    };
    #[cfg(target_os = "linux")]
    if mode.trim() == "uninspectable" {
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) }, 0);
    }
    if mode.trim() == "stderr-start" {
        eprintln!("[2026-10-31 00:00:00:000 INFO] Server started.");
    } else {
        println!("[2026-10-31 00:00:00:000 INFO] Server started.");
    }
    io::stdout().flush().unwrap();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        writeln!(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open("worlds/commands.log")
                .unwrap(),
            "{line}"
        )
        .unwrap();
        if line == "close-output" {
            unsafe {
                libc::close(libc::STDOUT_FILENO);
                libc::close(libc::STDERR_FILENO);
            }
            continue;
        }
        if line == "stop" {
            if mode.trim() == "large-stop-output" {
                io::stdout().write_all(&vec![b'x'; 1024 * 1024]).unwrap();
                io::stdout().flush().unwrap();
            }
            break;
        }
        if line == "list" && mode.trim() != "no-console" {
            println!("[2026-10-31 00:00:00:000 INFO] There are 0/10 players online:");
            io::stdout().flush().unwrap();
        }
    }
}

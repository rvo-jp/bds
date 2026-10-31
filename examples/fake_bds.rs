//! Isolated acceptance-test fixture; never used for a real world.
use std::{
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
    println!("[2026-10-31 00:00:00:000 INFO] Server started.");
    io::stdout().flush().unwrap();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
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

use crate::{
    config::{Config, port},
    util,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::unix::{
        io::AsRawFd,
        net::{UnixDatagram, UnixListener, UnixStream},
    },
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn signal_stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Status {
    pub invocation: String,
    pub boot: String,
    pub pid: u32,
    pub ready: bool,
    pub failed: bool,
    pub stopped: bool,
    pub child_exit: Option<i32>,
    pub checked_at: u64,
    pub port: u16,
    pub reason: String,
}
pub fn notify(text: &str) -> Result<()> {
    if let Ok(path) = std::env::var("NOTIFY_SOCKET") {
        if path.starts_with('@') {
            bail!("抽象notifyソケットはこの構成では使用しません")
        }
        UnixDatagram::unbound()?.send_to(text.as_bytes(), path)?;
    }
    Ok(())
}
pub fn command(c: &Config, text: &str) -> Result<()> {
    ensure!(
        text.len() <= 16384 && !text.contains(['\n', '\r', '\0']),
        "コンソールコマンドが不正です"
    );
    let mut s = UnixStream::connect(c.socket())?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(&serde_json::to_vec(&serde_json::json!({"command":text}))?)?;
    s.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    s.take(1024).read_to_string(&mut response)?;
    ensure!(response == "ok\n", "コンソールに送信できません");
    Ok(())
}
pub fn status(c: &Config) -> Result<Status> {
    util::read_json(&c.paths.runtime.join("status.json"))
}
pub fn healthy(c: &Config) -> bool {
    status(c)
        .map(|s| {
            s.ready
                && !s.failed
                && s.boot == util::boot()
                && util::now().saturating_sub(s.checked_at) <= 15
        })
        .unwrap_or(false)
}
#[cfg(target_os = "linux")]
pub fn owns_listener(pid: u32, port: u16) -> bool {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    let mut sockets = std::collections::HashSet::new();
    for e in entries.flatten() {
        if let Ok(p) = fs::read_link(e.path()) {
            let t = p.to_string_lossy();
            if let Some(s) = t.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
                sockets.insert(s.to_string());
            }
        }
    }
    for kind in ["tcp", "tcp6"] {
        if let Ok(text) = fs::read_to_string(format!("/proc/{pid}/net/{kind}")) {
            for line in text.lines().skip(1) {
                let f: Vec<_> = line.split_whitespace().collect();
                if f.len() > 9
                    && f[3] == "0A"
                    && f[1]
                        .split_once(':')
                        .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                        == Some(port)
                    && sockets.contains(f[9])
                {
                    return true;
                }
            }
        }
    }
    false
}
#[cfg(not(target_os = "linux"))]
pub fn owns_listener(_: u32, _: u16) -> bool {
    false
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "非同期入出力を設定できません"
    );
    Ok(())
}
fn flush(child: &mut Child, queue: &mut VecDeque<u8>) -> Result<()> {
    if queue.is_empty() {
        return Ok(());
    }
    let b = queue.make_contiguous();
    match child.stdin.as_mut().context("stdinがありません")?.write(b) {
        Ok(n) => {
            queue.drain(..n);
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn enqueue(queue: &mut VecDeque<u8>, text: &str) -> Result<()> {
    ensure!(
        queue.len() + text.len() < 65536,
        "コンソール送信キューが満杯です"
    );
    queue.extend(text.as_bytes());
    queue.push_back(b'\n');
    Ok(())
}
// Status/log failures must not stop us consuming the pipe that BDS needs to exit.
// Discard output here: forwarding it to another failing sink could block again.
fn stop_after_error(
    child: &mut Child,
    output: &mut std::process::ChildStdout,
    queue: &mut VecDeque<u8>,
) -> Result<std::process::ExitStatus> {
    queue.clear();
    // End a command whose prefix may already have entered the pipe.
    enqueue(queue, "")?;
    enqueue(queue, "stop")?;
    loop {
        let _ = flush(child, queue);
        for _ in 0..16 {
            let mut fd = libc::pollfd {
                fd: output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut fd, 1, 0) } <= 0 {
                break;
            }
            let mut buffer = [0u8; 8192];
            match output.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        if let Some(exit) = child.try_wait()? {
            return Ok(exit);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn info(line: &str) -> Option<&str> {
    let (_, body) = line.split_once("INFO]")?;
    Some(body.trim())
}
fn player(line: &str) -> Option<String> {
    let body = info(line)?;
    for (prefix, suffix) in [
        ("Player connected: ", " がログインしました。"),
        ("Player disconnected: ", " がログアウトしました。"),
    ] {
        if let Some(rest) = body.strip_prefix(prefix)
            && let Some((name, _)) = rest.split_once(", xuid:")
        {
            return Some(format!("{name}{suffix}"));
        }
    }
    None
}
fn publish(c: &Config, s: &mut Status) -> Result<()> {
    s.checked_at = util::now();
    util::json(&c.paths.runtime.join("status.json"), s, 0o644)
}
fn failure(c: &Config, s: &mut Status, reason: &str, logs: &VecDeque<String>) -> Result<()> {
    s.ready = false;
    s.failed = true;
    s.reason = reason.into();
    publish(c, s)?;
    let d = c.paths.diagnostics.clone();
    fs::create_dir_all(&d)?;
    util::json(
        &d.join(format!("{}.json", util::id()?)),
        &serde_json::json!({"status":s,"recent_output":logs}),
        0o600,
    )?;
    let mut paths: Vec<_> = fs::read_dir(&d)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    let remove = paths.len().saturating_sub(20);
    for p in paths.into_iter().take(remove) {
        fs::remove_file(p)?;
    }
    eprintln!("BDS異常: {reason}");
    Ok(())
}
pub fn run(c: &Config) -> Result<()> {
    ensure!(cfg!(target_os = "linux"), "BDS監督はLinux専用です");
    STOP.store(false, Ordering::Relaxed);
    fs::create_dir_all(&c.paths.runtime)?;
    let _lock = util::Lock::acquire(&c.paths.runtime.join("supervisor.lock"), 0)?;
    let p = port(c)?;
    let executable = c.current().join("bedrock_server");
    ensure!(executable.is_file(), "BDS本体がありません");
    let recovered = status(c).map(|s| s.failed).unwrap_or(false);
    if c.socket().exists() {
        let _ = fs::remove_file(c.socket());
    }
    let listener = UnixListener::bind(c.socket())?;
    util::chmod(&c.socket(), 0o600)?;
    listener.set_nonblocking(true)?;
    unsafe {
        libc::signal(
            libc::SIGTERM,
            signal_stop as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGINT, signal_stop as *const () as libc::sighandler_t);
    }
    let (tx, rx) = mpsc::sync_channel::<String>(64);
    let webhook = c.paths.webhook_file.clone();
    let network = std::thread::Builder::new()
        .name("notifier".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            while let Ok(text) = rx.recv() {
                if util::webhook(&webhook, &text).is_err() {
                    eprintln!("Discord通知に失敗しました（サーバー操作は継続します）");
                }
            }
        })?;
    let mut child = Command::new(&executable)
        .current_dir(c.current())
        .env("LD_LIBRARY_PATH", c.current())
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut s = Status {
        invocation: std::env::var("INVOCATION_ID").unwrap_or_default(),
        boot: util::boot(),
        pid: child.id(),
        port: p,
        ..Status::default()
    };
    let mut output = child.stdout.take().unwrap();

    let started_at = Instant::now();
    let mut next_check = Instant::now();
    let mut next_publish = Instant::now();
    let mut started = false;
    let mut stable: Option<Instant> = None;
    let mut pending: Option<Instant> = None;
    let mut reply: Option<Instant> = None;
    let mut misses = 0u32;
    let mut stopping = false;
    let mut queue = VecDeque::new();
    let mut buffer = Vec::new();
    let mut logs = VecDeque::new();
    let outcome = (|| -> Result<()> {
        nonblocking(output.as_raw_fd())?;
        nonblocking(child.stdin.as_ref().unwrap().as_raw_fd())?;
        loop {
            let now = Instant::now();
            if STOP.load(Ordering::Relaxed) && !stopping {
                stopping = true;
                s.ready = false;
                enqueue(&mut queue, "stop")?;
                publish(c, &mut s)?;
                let _ = notify("STOPPING=1\nSTATUS=正常停止中");
            }
            let _ = flush(&mut child, &mut queue);
            let mut fds = [
                libc::pollfd {
                    fd: output.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            unsafe {
                libc::poll(fds.as_mut_ptr(), 2, 250);
            }
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                        let mut input = vec![];
                        let request = Read::by_ref(&mut stream)
                            .take(17000)
                            .read_to_end(&mut input)
                            .ok()
                            .and_then(|_| serde_json::from_slice::<serde_json::Value>(&input).ok());
                        let accepted = request
                            .and_then(|v| v["command"].as_str().map(str::to_string))
                            .map(|text| {
                                if text == "stop" {
                                    STOP.store(true, Ordering::Relaxed);
                                    Ok(())
                                } else if stopping {
                                    bail!("停止中です")
                                } else {
                                    ensure!(
                                        !text.contains(['\n', '\r', '\0']),
                                        "コマンドが不正です"
                                    );
                                    enqueue(&mut queue, &text)
                                }
                            })
                            .unwrap_or_else(|| bail!("要求が不正です"));
                        let _ = stream.write_all(if accepted.is_ok() {
                            b"ok\n"
                        } else {
                            b"error\n"
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            loop {
                let mut b = [0u8; 8192];
                match output.read(&mut b) {
                    Ok(0) => break,
                    Ok(n) => {
                        buffer.extend_from_slice(&b[..n]);
                        while let Some(pos) = buffer.iter().position(|&x| x == b'\n') {
                            let line = String::from_utf8_lossy(&buffer[..pos])
                                .trim_end_matches('\r')
                                .to_string();
                            buffer.drain(..=pos);
                            writeln!(std::io::stdout().lock(), "{line}")?;
                            logs.push_back(line.chars().take(4096).collect());
                            if logs.len() > 200 {
                                logs.pop_front();
                            }
                            if info(&line) == Some("Server started.") {
                                started = true;
                            }
                            if info(&line).is_some_and(|x| {
                                x.starts_with("There are ") && x.contains(" players online:")
                            }) && pending.is_some()
                            {
                                pending = None;
                                reply = Some(Instant::now());
                                misses = 0;
                            }
                            if let Some(event) = player(&line) {
                                let _ = tx.try_send(event);
                            }
                        }
                        if buffer.len() > 65536 {
                            buffer.clear();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            if let Some(exit) = child.try_wait()? {
                s.ready = false;
                s.child_exit = exit.code();
                s.stopped = stopping && !s.failed && exit.success();
                if !stopping {
                    failure(c, &mut s, "BDSが予期せず終了しました", &logs)?;
                }
                publish(c, &mut s)?;
                return if s.stopped {
                    Ok(())
                } else {
                    bail!("BDSが異常終了しました")
                };
            }
            let mut timed_out = false;
            if !stopping
                && pending
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(c.server.response_seconds))
            {
                pending = None;
                misses += 1;
                stable = None;
                timed_out = true;
            }
            if !stopping && now >= next_check {
                next_check =
                    now + Duration::from_secs(if s.ready { c.server.health_seconds } else { 1 });
                let listening = owns_listener(child.id(), p);
                if !listening {
                    misses += u32::from(!timed_out);
                    stable = None;
                } else if started && pending.is_none() {
                    enqueue(&mut queue, "list")?;
                    pending = Some(now);
                }
                let responsive = started
                    && listening
                    && reply.is_some_and(|t| {
                        t.elapsed()
                            <= Duration::from_secs(
                                c.server.health_seconds + c.server.response_seconds,
                            )
                    })
                    && misses == 0;
                if !s.ready {
                    if responsive {
                        let t = stable.get_or_insert(now);
                        if t.elapsed() >= Duration::from_secs(c.server.stable_seconds) {
                            s.ready = true;
                            publish(c, &mut s)?;
                            notify("READY=1\nSTATUS=起動・TCP受付・コンソール応答を確認済み")?;
                            let _ = tx.try_send(
                                if recovered {
                                    "Minecraftサーバーが自動復旧しました。"
                                } else {
                                    "サーバーが起動しました。"
                                }
                                .into(),
                            );
                        }
                    } else {
                        stable = None;
                    }
                }
            }
            if !stopping
                && ((!s.ready
                    && stable.is_none()
                    && started_at.elapsed() >= Duration::from_secs(c.server.startup_seconds))
                    || (s.ready && misses >= c.server.failure_limit))
            {
                let was_ready = s.ready;
                failure(
                    c,
                    &mut s,
                    if was_ready {
                        "接続受付またはコンソール応答が連続して失敗しました"
                    } else {
                        "起動期限内に接続受付・応答を確認できませんでした"
                    },
                    &logs,
                )?;
                let _ = tx.try_send(
                    "Minecraftサーバーの異常を検出しました。正常停止後に制限内で復旧を試みます。"
                        .into(),
                );
                stopping = true;
                enqueue(&mut queue, "stop")?;
                let _ = notify("STOPPING=1\nSTATUS=異常を記録して正常停止中");
            }
            if now >= next_publish {
                publish(c, &mut s)?;
                next_publish = now + Duration::from_secs(5);
            }
        }
    })();
    if outcome.is_err() && child.try_wait()?.is_none() {
        s.failed = true;
        s.ready = false;
        let _ = publish(c, &mut s);
        s.child_exit = stop_after_error(&mut child, &mut output, &mut queue)?.code();
        let _ = publish(c, &mut s);
    }
    let _ = fs::remove_file(c.socket());
    drop(tx);
    drop(network);
    outcome
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notifications() {
        assert_eq!(
            player("[2026-10-31 12:00:00:000 INFO] Player connected: Steve, xuid: 123"),
            Some("Steve がログインしました。".into())
        );
        assert!(player("chat Server started.").is_none());
    }
    #[test]
    fn bounded_queue() {
        let mut q = VecDeque::new();
        assert!(enqueue(&mut q, &"x".repeat(65536)).is_err());
        assert!(enqueue(&mut q, "list").is_ok());
    }
    #[test]
    fn error_shutdown_drains_large_child_output() {
        let mut child = Command::new("python3")
            .args(["-c", "import sys; assert next(line.strip() for line in sys.stdin if line.strip()) == 'stop'; sys.stdout.buffer.write(b'x'*1048576); sys.stdout.flush()"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let pid = child.id();
        let mut output = child.stdout.take().unwrap();
        nonblocking(output.as_raw_fd()).unwrap();
        nonblocking(child.stdin.as_ref().unwrap().as_raw_fd()).unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut queue = VecDeque::from(b"obsolete-command\n".to_vec());
            let result = stop_after_error(&mut child, &mut output, &mut queue);
            let _ = tx.send(result.map(|exit| exit.success()));
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        if result.is_err() {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        worker.join().unwrap();
        assert!(result.unwrap().unwrap());
    }
}

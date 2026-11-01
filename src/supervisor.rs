use crate::{
    config::{Config, port},
    signals::Signals,
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
    sync::mpsc,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Status {
    pub invocation: String,
    pub boot: String,
    pub pid: u32,
    pub ready: bool,
    pub failed: bool,
    pub stopped: bool,
    pub child_exit: Option<i32>,
    pub changed_at: u64,
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
pub fn running(c: &Config) -> bool {
    status(c).is_ok_and(|s| {
        s.ready
            && !s.failed
            && !s.stopped
            && s.boot == util::boot()
            && util::prop(&c.server.unit, "ActiveState").is_ok_and(|state| state == "active")
            && util::prop(&c.server.unit, "InvocationID").is_ok_and(|id| id == s.invocation)
    })
}
/// Failure to inspect the kernel is a manager fault, not evidence of a dead BDS.
#[derive(Debug)]
pub struct ObservationError(anyhow::Error);
impl std::fmt::Display for ObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "起動確認処理の異常（自動再試行を停止）: {:#}", self.0)
    }
}
impl std::error::Error for ObservationError {}

#[cfg(target_os = "linux")]
pub fn owns_listener(pid: u32, port: u16) -> Result<bool> {
    inspect_listener(std::path::Path::new("/proc"), pid, port)
        .map_err(|e| ObservationError(e).into())
}
#[cfg(target_os = "linux")]
fn inspect_listener(proc: &std::path::Path, pid: u32, port: u16) -> Result<bool> {
    let root = proc.join(pid.to_string());
    let mut sockets = std::collections::HashSet::new();
    for e in fs::read_dir(root.join("fd")).context("BDSのFD一覧を読めません")? {
        let e = e?;
        let path = match fs::read_link(e.path()) {
            Ok(path) => path,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // Concurrent close.
            Err(e) => return Err(e).context("BDSのFDを確認できません"),
        };
        let text = path.to_string_lossy();
        if let Some(inode) = text
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
        {
            sockets.insert(inode.to_string());
        }
    }
    let mut found = false;
    for kind in ["tcp", "tcp6"] {
        let text = match fs::read_to_string(root.join("net").join(kind)) {
            Ok(text) => text,
            Err(e) if kind == "tcp6" && e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).context("TCP待受表を読めません"),
        };
        ensure!(
            text.lines()
                .next()
                .is_some_and(|l| l.contains("local_address")),
            "TCP待受表の形式が不正です"
        );
        for line in text.lines().skip(1).filter(|l| !l.trim().is_empty()) {
            let f: Vec<_> = line.split_whitespace().collect();
            ensure!(f.len() > 9, "TCP待受表の行が不正です");
            let local = f[1].split_once(':').context("TCPアドレスが不正です")?;
            let bound = u16::from_str_radix(local.1, 16).context("TCPポートが不正です")?;
            found |= f[3] == "0A" && bound == port && sockets.contains(f[9]);
        }
    }
    Ok(found)
}
#[cfg(not(target_os = "linux"))]
pub fn owns_listener(_: u32, _: u16) -> Result<bool> {
    Err(ObservationError(anyhow::anyhow!("TCP待受の確認はLinux専用です")).into())
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
        Ok(0) => bail!("コンソール入力を送信できません"),
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
fn wait(fds: &mut [libc::pollfd], deadline: Option<Instant>) -> Result<()> {
    let timeout = deadline.map_or(-1, |deadline| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            0
        } else {
            remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32
        }
    });
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    Ok(())
}
// Even if state/log writes fail, consume BDS output until it can exit.
fn stop_after_error(
    child: &mut Child,
    output: &mut (impl Read + AsRawFd),
    queue: &mut VecDeque<u8>,
    mut signals: Option<&mut Signals>,
) -> Result<std::process::ExitStatus> {
    queue.clear();
    // End a command whose prefix may already have entered the pipe.
    enqueue(queue, "")?;
    enqueue(queue, "stop")?;
    let mut output_open = true;
    loop {
        if let Some(signals) = signals.as_deref_mut() {
            let _ = signals.receive()?;
        }
        if flush(child, queue).is_err() {
            queue.clear();
            child.stdin.take();
        }
        if output_open {
            for _ in 0..16 {
                let mut buffer = [0u8; 8192];
                match output.read(&mut buffer) {
                    Ok(0) => {
                        output_open = false;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        output_open = false;
                        break;
                    }
                }
            }
        }
        if let Some(exit) = child.try_wait()? {
            return Ok(exit);
        }
        if !output_open && queue.is_empty() {
            return Ok(child.wait()?);
        }
        let mut fds = [
            libc::pollfd {
                fd: if output_open { output.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if queue.is_empty() {
                    -1
                } else {
                    child.stdin.as_ref().map_or(-1, AsRawFd::as_raw_fd)
                },
                events: libc::POLLOUT,
                revents: 0,
            },
            libc::pollfd {
                fd: signals.as_ref().map_or(-1, |signals| signals.fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        wait(&mut fds, None)?;
    }
}
struct Startup {
    started: bool,
    confirmed: bool,
    pending: Option<Instant>,
    stable: Option<Instant>,
    next_check: Instant,
    expires: Instant,
}
impl Startup {
    fn new(c: &Config) -> Self {
        let now = Instant::now();
        Self {
            started: false,
            confirmed: false,
            pending: None,
            stable: None,
            next_check: now,
            expires: now + Duration::from_secs(c.server.startup_seconds),
        }
    }
    fn log(&mut self, line: &str) {
        if info(line) == Some("Server started.") {
            self.started = true;
        }
        if self.pending.is_some()
            && info(line).is_some_and(|body| {
                body.starts_with("There are ") && body.contains(" players online:")
            })
        {
            self.pending = None;
            self.confirmed = true;
            self.next_check = Instant::now();
        }
    }
    fn deadline(&self, c: &Config) -> Instant {
        let mut deadline = self.stable.map_or(self.expires, |stable| {
            stable + Duration::from_secs(c.server.stable_seconds)
        });
        if self.started {
            deadline = deadline.min(self.next_check);
        }
        if let Some(pending) = self.pending {
            deadline = deadline.min(pending + Duration::from_secs(c.server.response_seconds));
        }
        deadline
    }
    fn check(&mut self, c: &Config, pid: u32, queue: &mut VecDeque<u8>) -> Result<bool> {
        let now = Instant::now();
        ensure!(
            self.stable.is_some() || now < self.expires,
            "起動期限内に接続受付・応答を確認できませんでした"
        );
        if self.pending.is_some_and(|pending| {
            pending.elapsed() >= Duration::from_secs(c.server.response_seconds)
        }) {
            self.pending = None;
        }
        let stable_due = self
            .stable
            .is_some_and(|stable| stable.elapsed() >= Duration::from_secs(c.server.stable_seconds));
        if !self.started || (now < self.next_check && !stable_due) {
            return Ok(false);
        }
        self.next_check = now + Duration::from_secs(1);
        if !owns_listener(pid, port(c)?)? {
            self.stable = None;
            self.confirmed = false;
            self.pending = None;
        } else if self.confirmed {
            let stable = self.stable.get_or_insert(now);
            return Ok(stable.elapsed() >= Duration::from_secs(c.server.stable_seconds));
        } else if self.pending.is_none() {
            enqueue(queue, "list")?;
            self.pending = Some(now);
        }
        Ok(false)
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
    s.changed_at = util::now();
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
    let mut signals = Signals::new()?;
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
    // Keep stderr in the same bounded event stream as stdout, including startup errors.
    let (mut output, child_output) = UnixStream::pair()?;
    let mut command = Command::new(&executable);
    command
        .current_dir(crate::releases::working(c, &crate::releases::current(c)?))
        .env("LD_LIBRARY_PATH", c.current())
        .env(
            "HOME",
            crate::releases::working(c, &crate::releases::current(c)?),
        )
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::piped())
        .stdout(Stdio::from(std::os::fd::OwnedFd::from(
            child_output.try_clone()?,
        )))
        .stderr(Stdio::from(std::os::fd::OwnedFd::from(child_output)));
    signals.configure_child(&mut command);
    let mut child = command.spawn()?;
    drop(command);
    let mut s = Status {
        invocation: std::env::var("INVOCATION_ID").unwrap_or_default(),
        boot: util::boot(),
        pid: child.id(),
        port: p,
        ..Status::default()
    };
    let mut startup = Startup::new(c);
    let mut stopping = false;
    let mut stop_requested = false;
    let mut output_open = true;
    let mut queue = VecDeque::new();
    let mut buffer = Vec::new();
    let mut logs = VecDeque::new();
    let mut startup_error: Option<anyhow::Error> = None;
    let outcome = (|| -> Result<()> {
        nonblocking(output.as_raw_fd())?;
        nonblocking(child.stdin.as_ref().unwrap().as_raw_fd())?;
        publish(c, &mut s)?;
        loop {
            stop_requested |= signals.receive()?;
            if stop_requested && !stopping {
                stopping = true;
                s.ready = false;
                enqueue(&mut queue, "stop")?;
                publish(c, &mut s)?;
                let _ = notify("STOPPING=1\nSTATUS=正常停止中");
            }
            flush(&mut child, &mut queue)?;
            for _ in 0..4 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                        stream.set_write_timeout(Some(Duration::from_millis(250)))?;
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
                                    stop_requested = true;
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
            if output_open {
                for _ in 0..16 {
                    let mut bytes = [0u8; 8192];
                    match output.read(&mut bytes) {
                        Ok(0) => {
                            output_open = false;
                            break;
                        }
                        Ok(n) => {
                            buffer.extend_from_slice(&bytes[..n]);
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
                                if !s.ready && !stopping {
                                    startup.log(&line);
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
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
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
                    Err(startup_error
                        .take()
                        .unwrap_or_else(|| anyhow::anyhow!("BDSが異常終了しました")))
                };
            }
            if stop_requested && !stopping {
                continue;
            }
            if !stopping && !s.ready {
                match startup.check(c, child.id(), &mut queue) {
                    Ok(true) => {
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
                    Ok(false) => {}
                    Err(error) => {
                        // A child can exit between try_wait and the /proc read.
                        let error = if error.is::<ObservationError>() && child.try_wait()?.is_some()
                        {
                            anyhow::anyhow!("BDSが起動確認中に終了しました")
                        } else {
                            error
                        };
                        failure(c, &mut s, &format!("{error:#}"), &logs)?;
                        let _ = tx.try_send(if error.is::<ObservationError>() {
                            "管理側の起動確認処理に失敗しました。Minecraftを正常停止し、自動再試行を中止します。".into()
                        } else {
                            "Minecraftの起動確認に失敗しました。正常停止後に制限内で復旧を試みます。".into()
                        });
                        startup_error = Some(error);
                        stopping = true;
                        enqueue(&mut queue, "stop")?;
                        let _ = notify("STOPPING=1\nSTATUS=起動失敗を記録して正常停止中");
                    }
                }
            }
            let mut fds = [
                libc::pollfd {
                    fd: if output_open { output.as_raw_fd() } else { -1 },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if stopping { -1 } else { listener.as_raw_fd() },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if queue.is_empty() {
                        -1
                    } else {
                        child.stdin.as_ref().unwrap().as_raw_fd()
                    },
                    events: libc::POLLOUT,
                    revents: 0,
                },
                libc::pollfd {
                    fd: signals.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // Only startup has deadlines. Once ready/stopping, wait for real events.
            wait(
                &mut fds,
                if !s.ready && !stopping {
                    Some(startup.deadline(c))
                } else {
                    None
                },
            )?;
        }
    })();
    if let Err(error) = &outcome {
        s.failed = true;
        s.ready = false;
        if s.reason.is_empty() {
            s.reason = format!("{error:#}");
        }
        let _ = publish(c, &mut s);
        let exit = if let Some(exit) = child.try_wait()? {
            exit
        } else {
            stop_after_error(&mut child, &mut output, &mut queue, Some(&mut signals))?
        };
        s.child_exit = exit.code();
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
            let result = stop_after_error(&mut child, &mut output, &mut queue, None);
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

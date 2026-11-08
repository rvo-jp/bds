//! TCP ownership and pre-launch binding. No network requests or resident polling.
use anyhow::Result;

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), expect(dead_code))]
pub(crate) enum Availability {
    Available,
    Closing,
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use anyhow::{Context, bail, ensure};
    use std::{
        collections::HashSet,
        fs, io,
        mem::size_of,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        path::Path,
    };

    struct Entry {
        port: u16,
        state: u8,
        inode: u64,
    }

    fn entries(root: &Path) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for kind in ["tcp", "tcp6"] {
            let text = match fs::read_to_string(root.join(kind)) {
                Ok(text) => text,
                Err(e) if kind == "tcp6" && e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).context("TCP待受表を読めません"),
            };
            ensure!(
                text.lines()
                    .next()
                    .is_some_and(|l| l.contains("local_address")),
                "TCP待受表の形式が不正です"
            );
            for line in text.lines().skip(1).filter(|l| !l.trim().is_empty()) {
                let fields: Vec<_> = line.split_whitespace().collect();
                ensure!(fields.len() > 9, "TCP待受表の行が不正です");
                let (_, port) = fields[1].split_once(':').context("TCPアドレスが不正です")?;
                entries.push(Entry {
                    port: u16::from_str_radix(port, 16).context("TCPポートが不正です")?,
                    state: u8::from_str_radix(fields[3], 16).context("TCP状態が不正です")?,
                    inode: fields[9].parse().context("TCPソケット番号が不正です")?,
                });
            }
        }
        Ok(entries)
    }

    pub(crate) fn inspect_listener(proc: &Path, pid: u32, port: u16) -> Result<bool> {
        let root = proc.join(pid.to_string());
        let mut sockets = HashSet::new();
        for e in fs::read_dir(root.join("fd")).context("BDSのFD一覧を読めません")? {
            let e = e?;
            let path = match fs::read_link(e.path()) {
                Ok(path) => path,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).context("BDSのFDを確認できません"),
            };
            if let Some(inode) = path
                .to_string_lossy()
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                sockets.insert(
                    inode
                        .parse::<u64>()
                        .context("BDSのソケット番号が不正です")?,
                );
            }
        }
        Ok(entries(&root.join("net"))?
            .iter()
            .any(|e| e.state == 0x0a && e.port == port && sockets.contains(&e.inode)))
    }

    // Mirror BDS's dual-stack wildcard bind, without SO_REUSEADDR. A successful
    // bind is immediately closed without listen/accept, so it creates no TIME_WAIT.
    fn bind(port: u16, family: i32) -> io::Result<OwnedFd> {
        let raw = unsafe { libc::socket(family, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let result = if family == libc::AF_INET6 {
            let dual_stack: libc::c_int = 0;
            if unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::IPPROTO_IPV6,
                    libc::IPV6_V6ONLY,
                    (&dual_stack as *const libc::c_int).cast(),
                    size_of::<libc::c_int>() as libc::socklen_t,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut address: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            address.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            address.sin6_port = port.to_be();
            unsafe {
                libc::bind(
                    socket.as_raw_fd(),
                    (&address as *const libc::sockaddr_in6).cast(),
                    size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        } else {
            let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            address.sin_family = libc::AF_INET as libc::sa_family_t;
            address.sin_port = port.to_be();
            unsafe {
                libc::bind(
                    socket.as_raw_fd(),
                    (&address as *const libc::sockaddr_in).cast(),
                    size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(socket)
    }

    fn bind_available(port: u16) -> io::Result<OwnedFd> {
        match bind(port, libc::AF_INET6) {
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EAFNOSUPPORT | libc::EPROTONOSUPPORT | libc::ENOPROTOOPT)
                ) =>
            {
                bind(port, libc::AF_INET)
            }
            result => result,
        }
    }

    pub(crate) fn availability(port: u16) -> Result<Availability> {
        match bind_available(port) {
            Ok(_) => return Ok(Availability::Available),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {}
            Err(e) => return Err(e).with_context(|| format!("TCP {port} を確保できません")),
        }
        let entries = entries(Path::new("/proc/net"))?;
        let matching: Vec<_> = entries.iter().filter(|e| e.port == port).collect();
        ensure!(
            !matching.iter().any(|e| e.state == 0x0a),
            "TCP {port} は別の待受に使用されています"
        );
        if matching
            .iter()
            .any(|e| matches!(e.state, 0x04 | 0x05 | 0x06 | 0x09 | 0x0b))
        {
            return Ok(Availability::Closing);
        }
        // A closing socket can disappear between bind and reading /proc.
        match bind_available(port) {
            Ok(_) => Ok(Availability::Available),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                bail!("TCP {port} は使用中ですが、終了待ちの接続を確認できません")
            }
            Err(e) => Err(e).with_context(|| format!("TCP {port} を確保できません")),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rejects_a_foreign_listener_and_a_bound_socket_without_listen() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            assert!(
                availability(port)
                    .unwrap_err()
                    .to_string()
                    .contains("別の待受")
            );
            drop(listener);
            let bound = bind(port, libc::AF_INET).unwrap();
            assert!(
                availability(port)
                    .unwrap_err()
                    .to_string()
                    .contains("終了待ち")
            );
            drop(bound);
            assert_eq!(availability(port).unwrap(), Availability::Available);
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{availability, inspect_listener};

#[cfg(not(target_os = "linux"))]
pub(crate) fn availability(_: u16) -> Result<Availability> {
    anyhow::bail!("TCP起動準備はLinux専用です")
}

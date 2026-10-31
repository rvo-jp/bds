//! Linux delivers stop requests and child exits through the same poll as I/O.
#[cfg(target_os = "linux")]
mod imp {
    use anyhow::{Result, ensure};
    use std::{
        fs::File,
        os::{
            fd::{AsRawFd, FromRawFd, RawFd},
            unix::process::CommandExt,
        },
        process::Command,
    };
    pub struct Signals {
        file: File,
        previous: libc::sigset_t,
    }
    impl Signals {
        // Call before starting any thread, so every thread inherits this mask.
        pub fn new() -> Result<Self> {
            unsafe {
                let mut mask = std::mem::zeroed();
                let mut previous = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGCHLD] {
                    libc::sigaddset(&mut mask, signal);
                }
                let error = libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut previous);
                ensure!(error == 0, "シグナル待機の設定に失敗しました: {error}");
                let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
                if fd < 0 {
                    let error = std::io::Error::last_os_error();
                    libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
                    return Err(error.into());
                }
                Ok(Self {
                    file: File::from_raw_fd(fd),
                    previous,
                })
            }
        }
        pub fn fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }
        pub fn configure_child(&self, child: &mut Command) {
            let previous = self.previous;
            unsafe {
                child.pre_exec(move || {
                    if libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        pub fn receive(&mut self) -> Result<bool> {
            let mut stop = false;
            loop {
                let mut info = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
                let n = unsafe {
                    libc::read(
                        self.fd(),
                        info.as_mut_ptr().cast(),
                        std::mem::size_of::<libc::signalfd_siginfo>(),
                    )
                };
                if n < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        return Ok(stop);
                    }
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error.into());
                }
                ensure!(
                    n as usize == std::mem::size_of::<libc::signalfd_siginfo>(),
                    "シグナル通知が不正です"
                );
                let info = unsafe { info.assume_init() };
                stop |= matches!(info.ssi_signo as i32, libc::SIGTERM | libc::SIGINT);
            }
        }
    }
    impl Drop for Signals {
        fn drop(&mut self) {
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
            }
        }
    }
}
#[cfg(not(target_os = "linux"))]
mod imp {
    use anyhow::{Result, bail};
    pub struct Signals;
    impl Signals {
        pub fn new() -> Result<Self> {
            bail!("シグナル待機はLinux専用です")
        }
        pub fn fd(&self) -> i32 {
            -1
        }
        pub fn configure_child(&self, _: &mut std::process::Command) {}
        pub fn receive(&mut self) -> Result<bool> {
            bail!("シグナル待機はLinux専用です")
        }
    }
}
pub use imp::Signals;

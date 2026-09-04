//! Private, bounded supervision for commands whose output is sensitive.
//!
//! The supervisor owns the child process group and captures both standard
//! output streams with supervisor-owned nonblocking readers. A descendant that
//! inherits a pipe cannot make the caller wait forever after the process group
//! has been terminated. The captured buffers are zeroized when dropped and are
//! only exposed to crate-internal callers.

use std::io::Read;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Coarse failures from bounded child supervision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureError {
    /// The child could not be spawned or its pipes were unavailable.
    Spawn,
    /// A child, reader, or wait operation failed.
    Failed,
    /// The process or capture did not finish before the absolute deadline.
    TimedOut,
    /// One output stream exceeded the caller's bound.
    OutputTooLarge,
}

/// The bounded result of a supervised command.
pub(crate) struct CapturedProcess {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Zeroizing<Vec<u8>>,
    pub(crate) stderr: Zeroizing<Vec<u8>>,
}

/// Spawns `command`, captures both output streams, and enforces one absolute
/// deadline. The command must request piped stdout and stderr.
pub(crate) fn run_bounded_capture(
    mut command: Command,
    timeout: Duration,
    output_limit: usize,
) -> Result<CapturedProcess, CaptureError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    #[cfg(not(unix))]
    {
        return run_bounded_capture_non_unix(command, timeout, output_limit);
    }

    #[cfg(unix)]
    {
        configure_private_process_group(&mut command);
        let mut child = command.spawn().map_err(|_| CaptureError::Spawn)?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut child);
                return Err(CaptureError::Spawn);
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                terminate_child(&mut child);
                return Err(CaptureError::Spawn);
            }
        };

        let mut stdout_reader = match NonblockingReader::new(stdout, output_limit) {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                return Err(error);
            }
        };
        let mut stderr_reader = match NonblockingReader::new(stderr, output_limit) {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                return Err(error);
            }
        };

        let mut status = None;
        let mut terminal_error = None;

        loop {
            if status.is_none() {
                match child_status(&mut child) {
                    Ok(Some(exit_status)) => status = Some(exit_status),
                    Ok(None) => {}
                    Err(_) => {
                        terminal_error = Some(CaptureError::Failed);
                        terminate_child(&mut child);
                        break;
                    }
                }
            }

            if stdout_reader.is_done() && stderr_reader.is_done() {
                break;
            }
            if Instant::now() >= deadline {
                terminal_error = Some(CaptureError::TimedOut);
                if status.is_none() {
                    terminate_child(&mut child);
                }
                break;
            }

            let events =
                match poll_readers(stdout_reader.raw_fd(), stderr_reader.raw_fd(), deadline) {
                    Ok(events) => events,
                    Err(error) => {
                        terminal_error = Some(error);
                        if status.is_none() {
                            terminate_child(&mut child);
                        }
                        break;
                    }
                };

            if events.stdout != 0
                && let Err(error) = stdout_reader.read_available(output_limit, deadline)
            {
                terminal_error = Some(error);
                if status.is_none() {
                    terminate_child(&mut child);
                }
                break;
            }
            if events.stderr != 0
                && let Err(error) = stderr_reader.read_available(output_limit, deadline)
            {
                terminal_error = Some(error);
                if status.is_none() {
                    terminate_child(&mut child);
                }
                break;
            }
        }

        let stdout = stdout_reader.into_output();
        let stderr = stderr_reader.into_output();
        if let Some(error) = terminal_error {
            return Err(error);
        }
        let status = status.ok_or(CaptureError::Failed)?;
        Ok(CapturedProcess {
            status,
            stdout,
            stderr,
        })
    }
}

#[cfg(not(unix))]
fn run_bounded_capture_non_unix(
    mut command: Command,
    _timeout: Duration,
    _output_limit: usize,
) -> Result<CapturedProcess, CaptureError> {
    // Anonymous-pipe reads are not cancellable through the standard library on
    // non-Unix targets. Fail closed after dropping the pipes instead of
    // leaving a blocking reader thread or a secret buffer behind.
    configure_private_process_group(&mut command);
    let mut child = command.spawn().map_err(|_| CaptureError::Spawn)?;
    drop(child.stdout.take());
    drop(child.stderr.take());
    terminate_child(&mut child);
    Err(CaptureError::Failed)
}

fn configure_private_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session gives the child and all ordinary descendants a private
        // process group that can be terminated together.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(unix)]
const READ_BUFFER_BYTES: usize = 8 * 1024;

#[cfg(unix)]
struct NonblockingReader<R> {
    reader: Option<R>,
    output: Zeroizing<Vec<u8>>,
    done: bool,
}

#[cfg(unix)]
impl<R> NonblockingReader<R>
where
    R: AsRawFd + Read,
{
    fn new(reader: R, output_limit: usize) -> Result<Self, CaptureError> {
        set_nonblocking(reader.as_raw_fd()).map_err(|_| CaptureError::Failed)?;
        Ok(Self {
            reader: Some(reader),
            output: Zeroizing::new(Vec::with_capacity(output_limit.min(READ_BUFFER_BYTES))),
            done: false,
        })
    }

    fn raw_fd(&self) -> RawFd {
        self.reader.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn read_available(
        &mut self,
        output_limit: usize,
        deadline: Instant,
    ) -> Result<(), CaptureError> {
        let mut buffer = Zeroizing::new([0_u8; READ_BUFFER_BYTES]);
        loop {
            if Instant::now() >= deadline {
                return Err(CaptureError::TimedOut);
            }
            let count = match self.reader.as_mut() {
                Some(reader) => match reader.read(&mut *buffer) {
                    Ok(count) => count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                    Err(_) => {
                        self.reader.take();
                        self.done = true;
                        return Err(CaptureError::Failed);
                    }
                },
                None => return Ok(()),
            };
            if count == 0 {
                self.reader.take();
                self.done = true;
                return Ok(());
            }
            if self.output.len().saturating_add(count) > output_limit {
                self.reader.take();
                self.done = true;
                return Err(CaptureError::OutputTooLarge);
            }
            self.output.extend_from_slice(&buffer[..count]);
        }
    }

    fn into_output(self) -> Zeroizing<Vec<u8>> {
        self.output
    }
}

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

#[cfg(unix)]
fn set_nonblocking(fd: RawFd) -> Result<(), std::io::Error> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
struct PollEvents {
    stdout: libc::c_short,
    stderr: libc::c_short,
}

#[cfg(unix)]
fn poll_readers(
    stdout_fd: RawFd,
    stderr_fd: RawFd,
    deadline: Instant,
) -> Result<PollEvents, CaptureError> {
    let mut descriptors = [
        libc::pollfd {
            fd: stdout_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: stderr_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout = remaining
        .min(Duration::from_millis(5))
        .as_millis()
        .try_into()
        .unwrap_or(i32::MAX)
        .max(1);
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout,
        )
    };
    if result == -1 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(PollEvents {
                stdout: 0,
                stderr: 0,
            });
        }
        return Err(CaptureError::Failed);
    }
    if descriptors
        .iter()
        .any(|descriptor| descriptor.revents & libc::POLLNVAL != 0)
    {
        return Err(CaptureError::Failed);
    }
    Ok(PollEvents {
        stdout: descriptors[0].revents,
        stderr: descriptors[1].revents,
    })
}

fn child_status(child: &mut Child) -> Result<Option<ExitStatus>, std::io::Error> {
    #[cfg(unix)]
    {
        let child_id = child.id() as libc::id_t;
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child_id,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { info.si_pid() } == 0 {
            Ok(None)
        } else {
            // WNOWAIT leaves the direct child waitable while its process
            // group is terminated. Reap only after group cleanup so the
            // child's PID remains bound and cannot be reused for an unrelated
            // group before the kill.
            terminate_process_group(child);
            Ok(Some(child.wait()?))
        }
    }
    #[cfg(not(unix))]
    {
        child.try_wait()
    }
}

fn terminate_child(child: &mut Child) {
    terminate_process_group(child);
    // Keep the direct-child kill even when group termination succeeds: it is
    // the fallback for a child that leaves its group before cleanup.
    let _ = child.kill();
    let _ = child.wait();
}

fn terminate_process_group(child: &Child) {
    #[cfg(unix)]
    {
        let process_group = child.id() as libc::pid_t;
        if process_group > 0 {
            unsafe {
                let _ = libc::kill(-process_group, libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn escaped_pipe_holder_helper() {
        if std::env::var_os("NAGI_PIPE_ESCAPE_HELPER").is_none() {
            return;
        }
        let pid_path = std::env::var_os("NAGI_PIPE_ESCAPE_PID").expect("escaped helper PID path");
        let release_path =
            std::env::var_os("NAGI_PIPE_ESCAPE_RELEASE").expect("escaped helper release path");
        let marker_path =
            std::env::var_os("NAGI_PIPE_ESCAPE_MARKER").expect("escaped helper marker path");
        assert!(unsafe { libc::setsid() } >= 0, "escaped helper must detach");
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        std::fs::write(&pid_path, std::process::id().to_string()).expect("escaped helper PID file");
        while !std::path::Path::new(&release_path).exists() {
            std::thread::sleep(Duration::from_millis(1));
        }

        let stdout_result = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                b"escaped-stdout\n".as_ptr().cast(),
                b"escaped-stdout\n".len(),
            )
        };
        let stderr_result = unsafe {
            libc::write(
                libc::STDERR_FILENO,
                b"escaped-stderr\n".as_ptr().cast(),
                b"escaped-stderr\n".len(),
            )
        };
        std::fs::write(&marker_path, format!("{stdout_result}:{stderr_result}"))
            .expect("escaped helper marker file");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    #[cfg(unix)]
    struct EscapedProcessCleanup {
        pid_path: std::path::PathBuf,
        release_path: std::path::PathBuf,
        marker_path: std::path::PathBuf,
        pid: Option<libc::pid_t>,
    }

    #[cfg(unix)]
    impl Drop for EscapedProcessCleanup {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.release_path, b"release");
            let pid = self.pid.or_else(|| {
                std::fs::read_to_string(&self.pid_path)
                    .ok()
                    .and_then(|value| value.trim().parse().ok())
            });
            if let Some(pid) = pid {
                unsafe {
                    let _ = libc::kill(pid, libc::SIGKILL);
                }
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline {
                    let result = unsafe { libc::kill(pid, 0) };
                    if result == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let _ = std::fs::remove_file(&self.pid_path);
            let _ = std::fs::remove_file(&self.release_path);
            let _ = std::fs::remove_file(&self.marker_path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn escaped_descendant_does_not_keep_supervisor_pipes_or_readers() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock")
            .as_nanos();
        let path = |suffix: &str| {
            std::env::temp_dir().join(format!(
                "nagi-process-supervisor-{}-{nonce}-{suffix}",
                std::process::id()
            ))
        };
        let pid_path = path("pid");
        let release_path = path("release");
        let marker_path = path("marker");
        let mut cleanup = EscapedProcessCleanup {
            pid_path: pid_path.clone(),
            release_path: release_path.clone(),
            marker_path: marker_path.clone(),
            pid: None,
        };

        let helper = std::env::current_exe().expect("current test executable");
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", r#"
                "$NAGI_PIPE_ESCAPE_HELPER" --exact process_supervisor::tests::escaped_pipe_holder_helper --nocapture &
                while [ ! -s "$NAGI_PIPE_ESCAPE_PID" ]; do :; done
                printf 'ready\n'
                exit 0
            "#])
            .env("NAGI_PIPE_ESCAPE_HELPER", helper)
            .env("NAGI_PIPE_ESCAPE_PID", &pid_path)
            .env("NAGI_PIPE_ESCAPE_RELEASE", &release_path)
            .env("NAGI_PIPE_ESCAPE_MARKER", &marker_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let started = Instant::now();
        let captured = run_bounded_capture(command, Duration::from_millis(150), 1024);
        cleanup.pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|value| value.trim().parse().ok());
        assert!(cleanup.pid.is_some(), "escaped helper PID must be recorded");
        assert!(matches!(captured, Err(CaptureError::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(1));

        std::fs::write(&release_path, b"release").expect("release escaped helper");
        let marker_deadline = Instant::now() + Duration::from_secs(1);
        let marker = loop {
            if let Ok(value) = std::fs::read_to_string(&marker_path) {
                break value;
            }
            assert!(
                Instant::now() < marker_deadline,
                "escaped helper marker timeout"
            );
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(marker, "-1:-1");
    }

    #[cfg(unix)]
    #[test]
    fn parent_exit_with_a_pipe_holding_descendant_is_bounded() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 5 & printf 'ready\\n'; exit 0"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let started = Instant::now();
        let captured = run_bounded_capture(command, Duration::from_secs(2), 1024)
            .expect("private process group must close inherited pipes");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(captured.status.success());
        assert_eq!(&*captured.stdout, b"ready\n");
    }

    #[cfg(unix)]
    #[test]
    fn output_is_bounded_without_waiting_for_the_child() {
        let mut command = Command::new("/usr/bin/yes");
        command
            .arg("x")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        assert!(matches!(
            run_bounded_capture(command, Duration::from_secs(1), 1024),
            Err(CaptureError::OutputTooLarge)
        ));
    }
}

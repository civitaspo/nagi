//! Private, bounded supervision for commands whose output is sensitive.
//!
//! The supervisor owns the child process group and captures both standard
//! output streams on bounded reader threads. Reader handles are deliberately
//! detached: a descendant that inherits a pipe cannot make the caller wait
//! forever after the process group has been terminated. The captured buffers
//! are zeroized when dropped and are only exposed to crate-internal callers.

use std::io::Read;
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread;
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

    let stdout_receiver = spawn_bounded_reader(stdout, output_limit);
    let stderr_receiver = spawn_bounded_reader(stderr, output_limit);
    let mut stdout_result = None;
    let mut stderr_result = None;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut status = None;
    let mut terminal_error = None;

    loop {
        poll_reader(&stdout_receiver, &mut stdout_result);
        poll_reader(&stderr_receiver, &mut stderr_result);
        if let Some(error) = reader_error(&stdout_result).or_else(|| reader_error(&stderr_result)) {
            terminal_error = Some(error);
            terminate_child(&mut child);
            break;
        }

        match child_status(&mut child) {
            Ok(Some(exit_status)) => {
                status = Some(exit_status);
                break;
            }
            Ok(None) => {}
            Err(_) => {
                terminal_error = Some(CaptureError::Failed);
                terminate_child(&mut child);
                break;
            }
        }

        let now = Instant::now();
        if now >= deadline {
            terminal_error = Some(CaptureError::TimedOut);
            terminate_child(&mut child);
            break;
        }
        thread::sleep(Duration::from_millis(5).min(deadline.saturating_duration_since(now)));
    }

    if status.is_none() {
        status = child.wait().ok();
    }

    let stdout = match receive_reader(stdout_receiver, stdout_result, deadline) {
        Ok(value) => value,
        Err(error) => {
            terminal_error.get_or_insert(error);
            Zeroizing::new(Vec::new())
        }
    };
    let stderr = match receive_reader(stderr_receiver, stderr_result, deadline) {
        Ok(value) => value,
        Err(error) => {
            terminal_error.get_or_insert(error);
            Zeroizing::new(Vec::new())
        }
    };

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

fn spawn_bounded_reader<R>(
    mut reader: R,
    output_limit: usize,
) -> Receiver<Result<Zeroizing<Vec<u8>>, CaptureError>>
where
    R: Read + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = read_bounded(&mut reader, output_limit);
        let _ = sender.send(result);
    });
    receiver
}

fn read_bounded<R: Read>(
    reader: &mut R,
    output_limit: usize,
) -> Result<Zeroizing<Vec<u8>>, CaptureError> {
    let mut output = Zeroizing::new(Vec::with_capacity(output_limit.min(8 * 1024)));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|_| CaptureError::Failed)?;
        if count == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(count) > output_limit {
            return Err(CaptureError::OutputTooLarge);
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

fn poll_reader(
    receiver: &Receiver<Result<Zeroizing<Vec<u8>>, CaptureError>>,
    result: &mut Option<Result<Zeroizing<Vec<u8>>, CaptureError>>,
) {
    if result.is_some() {
        return;
    }
    match receiver.try_recv() {
        Ok(value) => *result = Some(value),
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => *result = Some(Err(CaptureError::Failed)),
    }
}

fn reader_error(result: &Option<Result<Zeroizing<Vec<u8>>, CaptureError>>) -> Option<CaptureError> {
    result.as_ref().and_then(|value| match value {
        Ok(_) => None,
        Err(error) => Some(*error),
    })
}

fn receive_reader(
    receiver: Receiver<Result<Zeroizing<Vec<u8>>, CaptureError>>,
    result: Option<Result<Zeroizing<Vec<u8>>, CaptureError>>,
    deadline: Instant,
) -> Result<Zeroizing<Vec<u8>>, CaptureError> {
    if let Some(result) = result {
        return result;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(remaining) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(CaptureError::TimedOut),
        Err(RecvTimeoutError::Disconnected) => Err(CaptureError::Failed),
    }
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

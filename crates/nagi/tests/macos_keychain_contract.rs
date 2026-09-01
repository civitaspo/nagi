#![cfg(all(target_os = "macos", feature = "macos-keychain-contract"))]

use std::fs;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONTRACT_ENV: &str = "NAGI_CONTRACT_MACOS";
const PHASE_ENV: &str = "NAGI_KEYCHAIN_CONTRACT_PHASE";
const SERVICE_ENV: &str = "NAGI_KEYCHAIN_CONTRACT_SERVICE";
const SERVICE_PREFIX: &str = "dev.nagi.contract.synthetic.";
const RECORD_A: &[u8] = b"nagi-keychain-contract-record-a";
const RECORD_B: &[u8] = b"nagi-keychain-contract-record-b";
const CHILD_DEADLINE: Duration = Duration::from_secs(2);
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_CHILD_OUTPUT_BYTES: u64 = 64 * 1024;

struct EmptyCwd {
    path: PathBuf,
}

impl EmptyCwd {
    fn new() -> Result<Self, ()> {
        let base = std::env::temp_dir();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ())?
            .as_nanos();
        for attempt in 0..16 {
            let path = base.join(format!(
                "nagi-keychain-contract-{}-{timestamp}-{attempt}",
                std::process::id()
            ));
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(()),
            }
        }
        Err(())
    }

    fn is_empty(&self) -> Result<bool, ()> {
        fs::read_dir(&self.path)
            .map_err(|_| ())?
            .next()
            .transpose()
            .map(|entry| entry.is_none())
            .map_err(|_| ())
    }
}

impl Drop for EmptyCwd {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone, Copy)]
enum ContractFailure {
    ChildFailed,
    SecretOutput,
    WorkingDirectory,
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn read_child_pipe<R>(reader: R) -> thread::JoinHandle<Result<Vec<u8>, ()>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .take(MAX_CHILD_OUTPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        Ok(bytes)
    })
}

fn reap_after_kill(child: &mut Child) {
    let _ = child.kill();
    let deadline = Instant::now() + CHILD_DEADLINE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => thread::sleep(CHILD_POLL_INTERVAL),
        }
    }
    // The process has already received SIGKILL; wait only performs the final
    // reap so a timed-out child cannot remain as a zombie.
    let _ = child.wait();
}

fn run_child(cwd: &Path, service: &str, phase: &str) -> Result<(bool, Vec<u8>, Vec<u8>), ()> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nagi"))
        .arg("__contract")
        .arg("macos-keychain")
        .env_clear()
        .env(CONTRACT_ENV, "1")
        .env(PHASE_ENV, phase)
        .env(SERVICE_ENV, service)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| ())?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            reap_after_kill(&mut child);
            return Err(());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            reap_after_kill(&mut child);
            return Err(());
        }
    };
    let stdout_reader = read_child_pipe(stdout);
    let stderr_reader = read_child_pipe(stderr);

    let deadline = Instant::now() + CHILD_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(CHILD_POLL_INTERVAL),
            Ok(None) | Err(_) => {
                reap_after_kill(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(());
            }
        }
    };
    let stdout = stdout_reader.join().map_err(|_| ())?.map_err(|_| ())?;
    let stderr = stderr_reader.join().map_err(|_| ())?.map_err(|_| ())?;
    Ok((status.success(), stdout, stderr))
}

fn run_phase(cwd: &Path, service: &str, phase: &str) -> Result<(), ContractFailure> {
    let (success, stdout, stderr) =
        run_child(cwd, service, phase).map_err(|_| ContractFailure::ChildFailed)?;
    if stdout.len() > MAX_CHILD_OUTPUT_BYTES as usize
        || stderr.len() > MAX_CHILD_OUTPUT_BYTES as usize
        || contains(&stdout, RECORD_A)
        || contains(&stdout, RECORD_B)
        || contains(&stderr, RECORD_A)
        || contains(&stderr, RECORD_B)
    {
        return Err(ContractFailure::SecretOutput);
    }
    if !success {
        return Err(ContractFailure::ChildFailed);
    }
    Ok(())
}

#[test]
#[ignore = "touches a unique synthetic default file-based Keychain item"]
fn macos_keychain_round_trip_uses_only_a_synthetic_locator() {
    let executable = Path::new(env!("CARGO_BIN_EXE_nagi"));
    assert!(executable.is_file());
    assert!(
        !executable
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(".app")
    );
    assert_eq!(
        executable.file_name().and_then(|name| name.to_str()),
        Some("nagi")
    );

    let cwd = EmptyCwd::new().expect("create an empty contract working directory");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("contract clock")
        .as_nanos();
    let service = format!("{SERVICE_PREFIX}{}-{suffix}", std::process::id());
    assert!(service.starts_with(SERVICE_PREFIX));

    let sequence = (|| {
        for phase in [
            "absent", "write", "read-a", "update", "read-b", "delete", "absent",
        ] {
            run_phase(&cwd.path, &service, phase)?;
            if !cwd
                .is_empty()
                .map_err(|_| ContractFailure::WorkingDirectory)?
            {
                return Err(ContractFailure::WorkingDirectory);
            }
        }
        Ok::<(), ContractFailure>(())
    })();

    // Attempt exact cleanup even when an earlier child failed.  The hidden
    // delete phase is idempotent for an already-absent synthetic item.
    let cleanup_delete = run_phase(&cwd.path, &service, "delete");
    let cleanup_delete_empty = cwd.is_empty();
    let cleanup_absent = run_phase(&cwd.path, &service, "absent");
    let cleanup_absent_empty = cwd.is_empty();
    if sequence.is_err()
        || cleanup_delete.is_err()
        || cleanup_absent.is_err()
        || cleanup_delete_empty != Ok(true)
        || cleanup_absent_empty != Ok(true)
    {
        panic!("synthetic macOS Keychain contract failed closed");
    }
}

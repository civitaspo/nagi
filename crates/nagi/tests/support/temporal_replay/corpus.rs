use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::sanitizer::{assert_history_json_sanitized_with_build_id, sanitized_history};
use super::workflows::{RunChain, workflow_build_id};
use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SanitizerManifest {
    name: String,
    version: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusFileManifest {
    name: String,
    bytes: usize,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CorpusManifest {
    schema_version: u32,
    fixture: String,
    sanitizer: SanitizerManifest,
    workflow_type: String,
    task_queue: String,
    legacy_definition: String,
    current_definition: String,
    producer_revision: String,
    producer_revision_clean: bool,
    producer_revision_attestation: String,
    test_binary_sha256: String,
    temporal_cli_platform: String,
    temporal_cli_sha256: String,
    temporal_cli_version: String,
    rust_toolchain: String,
    temporal_rust_sdk: String,
    pub(crate) build_id: String,
    patch_id: String,
    patch_marker_count: u32,
    worker_versioning_mode: String,
    deployment_versioning: String,
    routing: String,
    history_files: Vec<CorpusFileManifest>,
}

fn current_uid() -> u32 {
    // The live contract runs only on macOS and the checked corpus test runs on
    // Unix CI. `geteuid` avoids spawning an unbounded helper for ownership.
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and is provided by libc on Unix.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn assert_path_components_are_not_symlinks(path: &Path, leaf_may_be_absent: bool) {
    use std::path::Component;

    assert!(path.is_absolute(), "corpus path must be absolute");
    let mut current = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(component) => {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => assert!(
                        !metadata.file_type().is_symlink(),
                        "corpus path component must not be a symlink"
                    ),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && leaf_may_be_absent
                            && current == path =>
                    {
                        return;
                    }
                    Err(error) => panic!("stat corpus path component: {error}"),
                }
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                panic!("corpus path must be canonical")
            }
        }
    }
}

fn assert_private_metadata(metadata: &fs::Metadata, expected_mode: u32) {
    assert!(metadata.is_file() || metadata.is_dir());
    assert!(!metadata.file_type().is_symlink());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        assert_eq!(metadata.uid(), current_uid());
        if metadata.is_file() {
            assert_eq!(metadata.nlink(), 1);
        }
        assert_eq!(metadata.permissions().mode() & 0o777, expected_mode);
    }
}

fn assert_checked_metadata(metadata: &fs::Metadata, expected_mode: u32) {
    assert!(metadata.is_file() || metadata.is_dir());
    assert!(!metadata.file_type().is_symlink());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        assert_eq!(metadata.uid(), current_uid());
        if metadata.is_file() {
            assert_eq!(metadata.nlink(), 1);
        }
        let mode = metadata.permissions().mode() & 0o777;
        // Git checkouts cannot represent 0600/0700 exactly: the checked
        // corpus is therefore accepted with repository read bits, but never
        // with group/other write bits or with weaker owner permissions.
        assert_eq!(mode & 0o700, expected_mode);
        assert_eq!(mode & 0o022, 0);
    }
}

fn ensure_private_directory(path: &Path) {
    assert_path_components_are_not_symlinks(path, true);
    assert!(
        fs::symlink_metadata(path).is_err(),
        "corpus directory must start absent"
    );
    fs::create_dir(path).expect("create private corpus directory");
    let mut permissions = fs::metadata(path)
        .expect("stat private corpus directory")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o700);
    }
    fs::set_permissions(path, permissions).expect("set private corpus directory mode");
    let metadata = fs::symlink_metadata(path).expect("restat private corpus directory");
    assert_private_metadata(&metadata, 0o700);
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    assert!(bytes.len() <= MAX_CORPUS_FILE_BYTES);
    let parent = path.parent().expect("corpus file parent");
    assert_path_components_are_not_symlinks(parent, false);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Set the initial mode in the create syscall, before another actor can
        // observe or replace the newly-created leaf.
        options.mode(0o600);
    }
    let mut file = options.open(path).expect("create private corpus file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file
            .metadata()
            .expect("stat opened corpus file")
            .permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)
            .expect("set opened corpus file mode");
    }
    file.write_all(bytes).expect("write private corpus file");
    file.write_all(b"\n")
        .expect("terminate private corpus file");
    file.flush().expect("flush private corpus file");
    file.sync_all().expect("sync private corpus file");
    let metadata = file.metadata().expect("restat opened corpus file");
    assert_private_metadata(&metadata, 0o600);
}

fn bounded_file_bytes(path: &Path, checked_corpus: bool) -> Vec<u8> {
    // The final path component is opened with O_NOFOLLOW on Unix. The
    // component walk above remains a preflight check only: a same-UID actor
    // can replace a parent component after that check, so this helper does
    // not claim to close that broader path-component race.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).expect("open bounded corpus file");
    let before = file.metadata().expect("stat opened bounded corpus file");
    if checked_corpus {
        assert_checked_metadata(&before, 0o600);
    } else {
        assert_private_metadata(&before, 0o600);
    }
    assert!(before.len() <= MAX_CORPUS_FILE_BYTES as u64);
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_CORPUS_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .expect("read bounded corpus file");
    assert!(bytes.len() <= MAX_CORPUS_FILE_BYTES);
    let after = file.metadata().expect("restat opened bounded corpus file");
    if checked_corpus {
        assert_checked_metadata(&after, 0o600);
    } else {
        assert_private_metadata(&after, 0o600);
    }
    assert_eq!(before.len(), after.len());
    bytes
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("write corpus SHA-256");
    }
    encoded
}

fn reviewed_temporal_cli_sha256(platform: &str) -> String {
    assert!(matches!(platform, "macos-arm64" | "macos-x64"));
    let provenance: serde_json::Value =
        serde_json::from_str(TEMPORAL_CLI_PROVENANCE).expect("Temporal CLI provenance JSON");
    assert_eq!(
        provenance
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64),
        Some(1)
    );
    assert_eq!(
        provenance.get("tool").and_then(serde_json::Value::as_str),
        Some("aqua:temporalio/cli")
    );
    assert_eq!(
        provenance
            .get("version")
            .and_then(serde_json::Value::as_str),
        Some("1.8.2")
    );
    provenance
        .get("artifacts")
        .and_then(serde_json::Value::as_object)
        .and_then(|artifacts| artifacts.get(platform))
        .and_then(serde_json::Value::as_object)
        .and_then(|artifact| artifact.get("binarySha256"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .filter(|digest| is_hex(digest, 64))
        .expect("reviewed Temporal CLI artifact digest")
}

fn expected_history_names() -> BTreeSet<String> {
    [LEGACY_A_FILE, LEGACY_B_FILE, CURRENT_A_FILE, CURRENT_B_FILE]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn validate_private_corpus_directory(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    assert_path_components_are_not_symlinks(directory, false);
    let checked_corpus = directory == checked_corpus_directory();
    let metadata = fs::symlink_metadata(directory).expect("stat replay corpus");
    if checked_corpus {
        assert_checked_metadata(&metadata, 0o700);
    } else {
        assert_private_metadata(&metadata, 0o700);
    }
    let expected_names = expected_history_names();
    let mut names = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(directory).expect("read replay corpus") {
        let entry = entry.expect("read replay corpus entry");
        let name = entry
            .file_name()
            .into_string()
            .expect("corpus file names must be UTF-8");
        assert!(name == MANIFEST_FILE || expected_names.contains(&name));
        assert!(names.insert(name.clone()), "duplicate corpus file name");
        let bytes = bounded_file_bytes(&entry.path(), checked_corpus);
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .expect("corpus size overflow");
        assert!(total_bytes <= MAX_CORPUS_TOTAL_BYTES);
        files.insert(name, bytes);
    }
    let mut expected_all = expected_names;
    expected_all.insert(MANIFEST_FILE.to_owned());
    assert_eq!(names, expected_all, "corpus file count or names changed");
    files
}

pub(crate) fn checked_corpus_directory() -> PathBuf {
    let manifest_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let directory = manifest_directory.join("tests/fixtures/temporal-replay");
    assert_path_components_are_not_symlinks(&directory, false);
    directory
}

fn validate_manifest(
    manifest: &CorpusManifest,
    files: &BTreeMap<String, Vec<u8>>,
    require_current_build_id: bool,
) {
    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert_eq!(manifest.fixture, "synthetic.temporal-replay.v1");
    assert_eq!(manifest.sanitizer.name, SANITIZER_NAME);
    assert_eq!(manifest.sanitizer.version, SANITIZER_VERSION);
    assert_eq!(manifest.workflow_type, WORKFLOW_TYPE);
    assert_eq!(manifest.task_queue, TASK_QUEUE);
    assert_eq!(
        manifest.legacy_definition,
        "source-level pre-patch definition"
    );
    assert_eq!(
        manifest.current_definition,
        "source-level patched definition"
    );
    assert!(is_hex(&manifest.producer_revision, 40));
    assert!(manifest.producer_revision_clean);
    assert_eq!(
        manifest.producer_revision_attestation,
        "clean-signed-commit-object"
    );
    assert!(is_hex(&manifest.test_binary_sha256, 64));
    assert!(matches!(
        manifest.temporal_cli_platform.as_str(),
        "macos-arm64" | "macos-x64"
    ));
    assert!(is_hex(&manifest.temporal_cli_sha256, 64));
    assert_eq!(
        manifest.temporal_cli_sha256,
        reviewed_temporal_cli_sha256(&manifest.temporal_cli_platform)
    );
    assert_eq!(manifest.temporal_cli_version, "1.8.2");
    assert_eq!(manifest.rust_toolchain, "1.98.0");
    assert_eq!(manifest.temporal_rust_sdk, "0.7.0");
    assert!(manifest.build_id.starts_with(BUILD_ID_PREFIX));
    assert!(is_hex(&manifest.build_id[BUILD_ID_PREFIX.len()..], 64));
    assert_eq!(manifest.patch_id, PATCH_ID);
    assert_eq!(manifest.patch_marker_count, 2);
    assert_eq!(manifest.worker_versioning_mode, "UNVERSIONED");
    assert_eq!(manifest.deployment_versioning, "not_exercised");
    assert_eq!(manifest.routing, "not_exercised");

    let expected_names = expected_history_names();
    let actual_names = manifest
        .history_files
        .iter()
        .map(|file| file.name.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_names, expected_names);
    assert_eq!(manifest.history_files.len(), expected_names.len());
    let mut total_bytes = 0usize;
    for file in &manifest.history_files {
        let bytes = files
            .get(&file.name)
            .expect("manifest must bind an existing history file");
        assert_eq!(file.bytes, bytes.len());
        assert!(file.bytes <= MAX_CORPUS_FILE_BYTES);
        assert!(is_hex(&file.sha256, 64));
        assert_eq!(file.sha256, sha256_bytes(bytes));
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .expect("corpus size overflow");
    }
    assert!(total_bytes <= MAX_CORPUS_TOTAL_BYTES);

    for (environment_name, actual) in [
        (PRODUCER_REVISION_ENV, &manifest.producer_revision),
        (TEST_BINARY_SHA256_ENV, &manifest.test_binary_sha256),
        (TEMPORAL_CLI_PLATFORM_ENV, &manifest.temporal_cli_platform),
        (TEMPORAL_CLI_SHA256_ENV, &manifest.temporal_cli_sha256),
        (TEMPORAL_CLI_VERSION_ENV, &manifest.temporal_cli_version),
    ] {
        if let Ok(expected) = std::env::var(environment_name) {
            assert_eq!(
                expected, *actual,
                "manifest binding changed for {environment_name}"
            );
        }
    }
    // A live export must bind the manifest to the exact source-derived Build
    // ID advertised by its workers. Checked corpora intentionally retain
    // historical Build IDs so they remain replayable after source changes.
    if require_current_build_id {
        assert_eq!(manifest.build_id, workflow_build_id());
    }
}

pub(crate) fn load_private_corpus(
    directory: &Path,
    require_current_build_id: bool,
) -> (CorpusManifest, BTreeMap<String, Vec<u8>>) {
    let files = validate_private_corpus_directory(directory);
    let manifest_bytes = files
        .get(MANIFEST_FILE)
        .expect("corpus must contain a manifest");
    assert!(manifest_bytes.len() <= MAX_CORPUS_FILE_BYTES);
    let manifest: CorpusManifest =
        serde_json::from_slice(manifest_bytes).expect("corpus manifest must decode");
    let history_files = files
        .iter()
        .filter(|(name, _)| name.as_str() != MANIFEST_FILE)
        .map(|(name, bytes)| (name.clone(), bytes.clone()))
        .collect::<BTreeMap<_, _>>();
    validate_manifest(&manifest, &history_files, require_current_build_id);
    for (name, bytes) in &history_files {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).unwrap_or_else(|error| panic!("{name} JSON: {error}"));
        assert_history_json_sanitized_with_build_id(&value, &manifest.build_id);
        let parsed = temporalio_client::WorkflowHistory::from_json(bytes)
            .unwrap_or_else(|error| panic!("{name} must decode as WorkflowHistory: {error}"));
        let reparsed_value: serde_json::Value = serde_json::from_slice(
            &parsed
                .to_json()
                .unwrap_or_else(|error| panic!("{name} must reserialize: {error}")),
        )
        .unwrap_or_else(|error| panic!("{name} reserialized JSON: {error}"));
        assert_eq!(
            reparsed_value, value,
            "{name} must roundtrip byte semantics"
        );
    }
    (manifest, history_files)
}

pub(crate) fn export_corpus(directory: &Path, legacy: &RunChain, current: &RunChain) {
    ensure_private_directory(directory);
    let history_bytes = [
        (
            LEGACY_A_FILE,
            sanitized_history(
                &legacy.history_a,
                &legacy.run_a,
                &legacy.run_b,
                LEGACY_RUN_A,
                LEGACY_RUN_B,
            ),
        ),
        (
            LEGACY_B_FILE,
            sanitized_history(
                &legacy.history_b,
                &legacy.run_a,
                &legacy.run_b,
                LEGACY_RUN_A,
                LEGACY_RUN_B,
            ),
        ),
        (
            CURRENT_A_FILE,
            sanitized_history(
                &current.history_a,
                &current.run_a,
                &current.run_b,
                CURRENT_RUN_A,
                CURRENT_RUN_B,
            ),
        ),
        (
            CURRENT_B_FILE,
            sanitized_history(
                &current.history_b,
                &current.run_a,
                &current.run_b,
                CURRENT_RUN_A,
                CURRENT_RUN_B,
            ),
        ),
    ];
    for (name, bytes) in &history_bytes {
        write_private_file(&directory.join(name), bytes);
    }

    let history_files = history_bytes
        .iter()
        .map(|(name, bytes)| CorpusFileManifest {
            name: (*name).to_owned(),
            // The newline written by write_private_file is part of the corpus
            // file and therefore part of its bound digest.
            bytes: bytes.len() + 1,
            sha256: sha256_bytes(&[bytes.as_slice(), b"\n"].concat()),
        })
        .collect::<Vec<_>>();
    let manifest = CorpusManifest {
        schema_version: SCHEMA_VERSION,
        fixture: "synthetic.temporal-replay.v1".to_owned(),
        sanitizer: SanitizerManifest {
            name: SANITIZER_NAME.to_owned(),
            version: SANITIZER_VERSION,
        },
        workflow_type: WORKFLOW_TYPE.to_owned(),
        task_queue: TASK_QUEUE.to_owned(),
        legacy_definition: "source-level pre-patch definition".to_owned(),
        current_definition: "source-level patched definition".to_owned(),
        producer_revision: required_env(PRODUCER_REVISION_ENV),
        producer_revision_clean: true,
        producer_revision_attestation: "clean-signed-commit-object".to_owned(),
        test_binary_sha256: required_env(TEST_BINARY_SHA256_ENV),
        temporal_cli_platform: required_env(TEMPORAL_CLI_PLATFORM_ENV),
        temporal_cli_sha256: required_env(TEMPORAL_CLI_SHA256_ENV),
        temporal_cli_version: required_env(TEMPORAL_CLI_VERSION_ENV),
        rust_toolchain: "1.98.0".to_owned(),
        temporal_rust_sdk: "0.7.0".to_owned(),
        build_id: workflow_build_id(),
        patch_id: PATCH_ID.to_owned(),
        patch_marker_count: 2,
        worker_versioning_mode: "UNVERSIONED".to_owned(),
        deployment_versioning: "not_exercised".to_owned(),
        routing: "not_exercised".to_owned(),
        history_files,
    };
    let manifest_bytes = serde_json::to_vec(&manifest).expect("encode corpus manifest");
    write_private_file(&directory.join(MANIFEST_FILE), &manifest_bytes);
    let (parsed_manifest, files) = load_private_corpus(directory, true);
    assert_eq!(parsed_manifest, manifest);
    assert_eq!(files.len(), 4);
}

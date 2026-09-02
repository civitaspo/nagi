#[cfg(target_os = "macos")]
pub(crate) const ADDRESS_ENV: &str = "NAGI_TEMPORAL_REPLAY_ADDRESS";
#[cfg(target_os = "macos")]
pub(crate) const NAMESPACE_ENV: &str = "NAGI_TEMPORAL_REPLAY_NAMESPACE";
#[cfg(target_os = "macos")]
pub(crate) const CORPUS_DIR_ENV: &str = "NAGI_TEMPORAL_REPLAY_CORPUS_DIR";
#[cfg(target_os = "macos")]
pub(crate) const PHASE_ENV: &str = "NAGI_TEMPORAL_REPLAY_PHASE";
pub(crate) const PRODUCER_REVISION_ENV: &str = "NAGI_TEMPORAL_REPLAY_PRODUCER_REVISION";
pub(crate) const TEST_BINARY_SHA256_ENV: &str = "NAGI_TEMPORAL_REPLAY_TEST_BINARY_SHA256";
pub(crate) const TEMPORAL_CLI_SHA256_ENV: &str = "NAGI_TEMPORAL_REPLAY_TEMPORAL_CLI_SHA256";
pub(crate) const TEMPORAL_CLI_VERSION_ENV: &str = "NAGI_TEMPORAL_REPLAY_TEMPORAL_CLI_VERSION";
#[cfg(target_os = "macos")]
pub(crate) const BOOTSTRAP_DIR_ENV: &str = "NAGI_TEMPORAL_REPLAY_BOOTSTRAP_DIR";
pub(crate) const TEMPORAL_CLI_PLATFORM_ENV: &str = "NAGI_TEMPORAL_REPLAY_TEMPORAL_CLI_PLATFORM";
pub(crate) const TEMPORAL_CLI_PROVENANCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/temporal-cli-provenance.json"
));

#[cfg(target_os = "macos")]
pub(crate) const CLIENT_IDENTITY: &str = "nagi-contract-replay-client-v1";
#[cfg(target_os = "macos")]
pub(crate) const WORKER_IDENTITY: &str = "nagi-contract-replay-worker-v1";
pub(crate) const WORKFLOW_TYPE: &str = "ReplayCompatibilityWorkflow";
pub(crate) const TASK_QUEUE: &str = "nagi-contract-replay-v1";
#[cfg(target_os = "macos")]
pub(crate) const LEGACY_WORKFLOW_ID: &str = "nagi-contract-replay-legacy-v1";
#[cfg(target_os = "macos")]
pub(crate) const CURRENT_WORKFLOW_ID: &str = "nagi-contract-replay-current-v1";
pub(crate) const PATCH_ID: &str = "nagi-replay-patch-v1";
pub(crate) const MISMATCHED_PATCH_ID: &str = "nagi-replay-patch-mismatched-v1";
pub(crate) const BUILD_ID_PREFIX: &str = "nagi/0.1.0/";
pub(crate) const SANITIZED_BUILD_ID: &str =
    "nagi/0.1.0/0000000000000000000000000000000000000000000000000000000000000000";
// These values are payload metadata only. They deliberately do not describe
// the worker that produced the live history or opt this corpus into Worker
// Deployment Versioning.
pub(crate) const SANITIZED_IDENTITY: &str = "nagi-replay-corpus-v1";
pub(crate) const SANITIZED_DEPLOYMENT_NAME: &str = "synthetic-replay-deployment-v1";
pub(crate) const SANITIZED_DEPLOYMENT_VERSION: &str = "synthetic-replay-deployment-version-v1";
pub(crate) const SANITIZED_SERIES_NAME: &str = "synthetic-replay-series-v1";
pub(crate) const SANITIZED_PINNED_VERSION: &str = "synthetic-replay-pinned-version-v1";

pub(crate) const CORPUS_WORKFLOW_ID: &str = "synthetic-replay-corpus-v1";
pub(crate) const LEGACY_RUN_A: &str = "00000000-0000-4000-8000-000000000001";
pub(crate) const LEGACY_RUN_B: &str = "00000000-0000-4000-8000-000000000002";
pub(crate) const CURRENT_RUN_A: &str = "00000000-0000-4000-8000-000000000003";
pub(crate) const CURRENT_RUN_B: &str = "00000000-0000-4000-8000-000000000004";

pub(crate) const LEGACY_A_FILE: &str = "legacy-a.history.json";
pub(crate) const LEGACY_B_FILE: &str = "legacy-b.history.json";
pub(crate) const CURRENT_A_FILE: &str = "current-a.history.json";
pub(crate) const CURRENT_B_FILE: &str = "current-b.history.json";
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const SCHEMA_VERSION: u32 = 1;
pub(crate) const SANITIZER_NAME: &str = "nagi-temporal-replay-sanitizer";
pub(crate) const SANITIZER_VERSION: u32 = 1;
pub(crate) const MAX_CORPUS_FILE_BYTES: usize = 65_536;
pub(crate) const MAX_CORPUS_TOTAL_BYTES: usize = 262_144;

#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
pub(crate) struct ReplayInput {
    pub(crate) generation: u32,
    pub(crate) carried_state: String,
    pub(crate) build_id: String,
}

#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
pub(crate) struct ReplayResult {
    pub(crate) carried_state: String,
    pub(crate) patch_active: bool,
    pub(crate) build_id: String,
}

mod corpus;
mod sanitizer;
mod workflows;

pub(crate) use corpus::checked_corpus_directory;
#[cfg(target_os = "macos")]
pub(crate) use corpus::export_corpus;
pub(crate) use workflows::replay::replay_private_corpus;
#[cfg(target_os = "macos")]
pub(crate) use workflows::{
    assert_chain, connect_client, required_env, run_current_chain, run_legacy_chain,
};

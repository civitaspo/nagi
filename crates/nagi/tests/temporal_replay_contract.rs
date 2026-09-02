#![cfg(feature = "temporal-replay-contract")]

//! Live Temporal replay compatibility witness.
//!
//! The live test is intentionally macOS-only and is launched by the closed
//! `temporal-sdk-contract.sh` wrapper. It creates histories in a pinned local
//! sidecar, verifies the complete Continue-As-New chain, and writes a
//! deterministic sanitized corpus. The live export is private and temporary;
//! only the separately checked, sanitized corpus under `tests/fixtures` is
//! public. The replay phase consumes either corpus without a server. The
//! `workflows` support module provides separate source-level legacy/current
//! definitions, while `sanitizer` owns the public-history safety boundary; an
//! environment flag never pretends to be an older release artifact.

#[path = "support/temporal_replay/mod.rs"]
mod temporal_replay;

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "current_thread")]
async fn temporal_replay_contract_exercises_live() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let corpus_directory = std::path::PathBuf::from(temporal_replay::required_env(
                temporal_replay::CORPUS_DIR_ENV,
            ));
            match temporal_replay::required_env(temporal_replay::PHASE_ENV).as_str() {
                "export" => {
                    let (client, namespace) = temporal_replay::connect_client().await;
                    let runtime = temporalio_sdk::Runtime::new_assume_tokio(Default::default())
                        .expect("create SDK runtime");
                    let legacy =
                        temporal_replay::run_legacy_chain(&client, &namespace, &runtime).await;
                    let current =
                        temporal_replay::run_current_chain(&client, &namespace, &runtime).await;
                    temporal_replay::assert_chain(
                        &legacy,
                        &legacy.run_a,
                        false,
                        "synthetic:legacy",
                    );
                    temporal_replay::assert_chain(
                        &current,
                        &current.run_a,
                        true,
                        "synthetic:current",
                    );
                    temporal_replay::export_corpus(&corpus_directory, &legacy, &current);
                    if let Ok(bootstrap_directory) =
                        std::env::var(temporal_replay::BOOTSTRAP_DIR_ENV)
                    {
                        temporal_replay::export_corpus(
                            &std::path::PathBuf::from(bootstrap_directory),
                            &legacy,
                            &current,
                        );
                    }
                }
                "replay" => {
                    temporal_replay::replay_private_corpus(&corpus_directory).await;
                    if let Ok(bootstrap_directory) =
                        std::env::var(temporal_replay::BOOTSTRAP_DIR_ENV)
                    {
                        temporal_replay::replay_private_corpus(&std::path::PathBuf::from(
                            bootstrap_directory,
                        ))
                        .await;
                    }
                }
                phase => panic!("unexpected Temporal replay contract phase: {phase}"),
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn temporal_replay_checked_corpus() {
    tokio::task::LocalSet::new()
        .run_until(async {
            temporal_replay::replay_private_corpus(&temporal_replay::checked_corpus_directory())
                .await
        })
        .await;
}

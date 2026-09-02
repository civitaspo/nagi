use std::path::Path;

use serde_json::Value;
use temporalio_client::WorkflowHistory;
use temporalio_sdk::workflow_replayer::{
    WorkflowReplayError, WorkflowReplayFailure, WorkflowReplayer, WorkflowReplayerOptions,
};

use super::super::corpus::load_private_corpus;
use super::super::sanitizer::assert_history_json_sanitized_with_build_id;
use super::super::*;
use super::{
    LegacyReplayWorkflow, MismatchedPatchReplayWorkflow, ReplayCompatibilityWorkflow,
    assert_completed, continue_as_new_run_id, patch_markers, validate_history_shape,
};

async fn replay_with_current(history: WorkflowHistory) -> Result<(), WorkflowReplayError> {
    let options = WorkflowReplayerOptions::new()
        .register_workflow::<ReplayCompatibilityWorkflow>()
        .expect("register current replay workflow")
        .build();
    WorkflowReplayer::new(options)
        .expect("create pinned Temporal WorkflowReplayer")
        .replay_workflow(history)
        .await
}

async fn replay_with_legacy(history: WorkflowHistory) -> Result<(), WorkflowReplayError> {
    let options = WorkflowReplayerOptions::new()
        .register_workflow::<LegacyReplayWorkflow>()
        .expect("register legacy replay workflow")
        .build();
    WorkflowReplayer::new(options)
        .expect("create pinned Temporal WorkflowReplayer")
        .replay_workflow(history)
        .await
}

async fn replay_with_mismatched_patch(history: WorkflowHistory) -> Result<(), WorkflowReplayError> {
    let options = WorkflowReplayerOptions::new()
        .register_workflow::<MismatchedPatchReplayWorkflow>()
        .expect("register mismatched replay workflow")
        .build();
    WorkflowReplayer::new(options)
        .expect("create pinned Temporal WorkflowReplayer")
        .replay_workflow(history)
        .await
}

fn assert_nondeterminism(error: WorkflowReplayError) {
    assert!(matches!(
        error,
        WorkflowReplayError::Replay(WorkflowReplayFailure::Nondeterminism { .. })
    ));
}

pub(crate) async fn replay_private_corpus(directory: &Path) {
    let (manifest, files) = load_private_corpus(directory, false);
    let load_history = |name: &str| {
        let bytes = files
            .get(name)
            .unwrap_or_else(|| panic!("corpus manifest omitted {name}"));
        let value: Value = serde_json::from_slice(bytes).expect("history JSON must decode");
        assert_history_json_sanitized_with_build_id(&value, &manifest.build_id);
        WorkflowHistory::from_json(bytes).expect("history JSON must reparse as WorkflowHistory")
    };
    let legacy_a = load_history(LEGACY_A_FILE);
    let legacy_b = load_history(LEGACY_B_FILE);
    let current_a = load_history(CURRENT_A_FILE);
    let current_b = load_history(CURRENT_B_FILE);
    validate_history_shape(
        &legacy_a,
        CORPUS_WORKFLOW_ID,
        LEGACY_RUN_A,
        LEGACY_RUN_A,
        None,
    );
    validate_history_shape(
        &legacy_b,
        CORPUS_WORKFLOW_ID,
        LEGACY_RUN_B,
        LEGACY_RUN_A,
        Some(LEGACY_RUN_A),
    );
    validate_history_shape(
        &current_a,
        CORPUS_WORKFLOW_ID,
        CURRENT_RUN_A,
        CURRENT_RUN_A,
        None,
    );
    validate_history_shape(
        &current_b,
        CORPUS_WORKFLOW_ID,
        CURRENT_RUN_B,
        CURRENT_RUN_A,
        Some(CURRENT_RUN_A),
    );
    assert_eq!(patch_markers(&legacy_a), Vec::new());
    assert_eq!(patch_markers(&legacy_b), Vec::new());
    assert_eq!(
        patch_markers(&current_a),
        vec![(PATCH_ID.to_owned(), false)]
    );
    assert_eq!(
        patch_markers(&current_b),
        vec![(PATCH_ID.to_owned(), false)]
    );
    assert_eq!(continue_as_new_run_id(&legacy_a), LEGACY_RUN_B);
    assert_eq!(continue_as_new_run_id(&current_a), CURRENT_RUN_B);
    assert_completed(&legacy_b);
    assert_completed(&current_b);

    replay_with_current(legacy_a.clone())
        .await
        .expect("current definition must replay the pre-marker history");
    replay_with_current(legacy_b.clone())
        .await
        .expect("current definition must replay the pre-marker continued run");
    replay_with_current(current_a.clone())
        .await
        .expect("current definition must replay the marker history");
    replay_with_current(current_b.clone())
        .await
        .expect("current definition must replay the marker continued run");

    replay_with_legacy(legacy_a)
        .await
        .expect("legacy definition must replay its pre-marker history");
    replay_with_legacy(legacy_b)
        .await
        .expect("legacy definition must replay its pre-marker continued run");
    assert_nondeterminism(
        replay_with_legacy(current_a)
            .await
            .expect_err("legacy definition must reject a marker history"),
    );
    assert_nondeterminism(
        replay_with_mismatched_patch(current_b)
            .await
            .expect_err("mismatched patch ID must reject a marker history"),
    );
}

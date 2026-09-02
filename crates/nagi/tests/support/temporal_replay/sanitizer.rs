use std::collections::{BTreeMap, BTreeSet};

use chrono::DateTime;
use serde_json::Value;
use temporalio_client::WorkflowHistory;

use super::{
    BUILD_ID_PREFIX, CORPUS_WORKFLOW_ID, CURRENT_RUN_A, CURRENT_RUN_B, LEGACY_RUN_A, LEGACY_RUN_B,
    SANITIZED_BUILD_ID, SANITIZED_DEPLOYMENT_NAME, SANITIZED_DEPLOYMENT_VERSION,
    SANITIZED_IDENTITY, SANITIZED_PINNED_VERSION, SANITIZED_SERIES_NAME,
};

fn replace_run_id(value: &str, run_ids: &BTreeMap<String, String>) -> String {
    if value.is_empty() {
        return String::new();
    }
    if let Some(canonical) = run_ids.get(value) {
        return canonical.clone();
    }
    if [LEGACY_RUN_A, LEGACY_RUN_B, CURRENT_RUN_A, CURRENT_RUN_B].contains(&value) {
        return value.to_owned();
    }
    panic!("history contains an unbound workflow run ID");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicKey {
    WorkflowId,
    Identity,
    RequestId,
    RunId,
    BuildId,
    BuildIds,
    WorkerDeploymentVersion,
    WorkerDeploymentName,
    DeploymentName,
    SeriesName,
    PinnedVersion,
}

fn dynamic_key(key: &str) -> Option<DynamicKey> {
    Some(match key {
        "workflow_id" | "workflowId" => DynamicKey::WorkflowId,
        "identity"
        | "worker_identity"
        | "workerIdentity"
        | "last_worker_identity"
        | "lastWorkerIdentity"
        | "last_modifier_identity"
        | "lastModifierIdentity"
        | "manager_identity"
        | "managerIdentity" => DynamicKey::Identity,
        "request_id" | "requestId" | "attached_request_id" | "attachedRequestId" => {
            DynamicKey::RequestId
        }
        "run_id"
        | "runId"
        | "original_execution_run_id"
        | "originalExecutionRunId"
        | "first_execution_run_id"
        | "firstExecutionRunId"
        | "continued_execution_run_id"
        | "continuedExecutionRunId"
        | "new_execution_run_id"
        | "newExecutionRunId"
        | "first_run_id"
        | "firstRunId"
        | "reset_run_id"
        | "resetRunId"
        | "base_run_id"
        | "baseRunId"
        | "new_run_id"
        | "newRunId" => DynamicKey::RunId,
        "build_id"
        | "buildId"
        | "binary_checksum"
        | "binaryChecksum"
        | "inherited_build_id"
        | "inheritedBuildId"
        | "assigned_build_id"
        | "assignedBuildId"
        | "source_build_id"
        | "sourceBuildId"
        | "target_build_id"
        | "targetBuildId"
        | "worker_build_id"
        | "workerBuildId"
        | "deployment_build_id"
        | "deploymentBuildId"
        | "last_independently_assigned_build_id"
        | "lastIndependentlyAssignedBuildId" => DynamicKey::BuildId,
        "build_ids" | "buildIds" => DynamicKey::BuildIds,
        "worker_deployment_version"
        | "workerDeploymentVersion"
        | "last_worker_deployment_version"
        | "lastWorkerDeploymentVersion"
        | "parent_pinned_worker_deployment_version"
        | "parentPinnedWorkerDeploymentVersion" => DynamicKey::WorkerDeploymentVersion,
        "worker_deployment_name" | "workerDeploymentName" => DynamicKey::WorkerDeploymentName,
        "deployment_name" | "deploymentName" => DynamicKey::DeploymentName,
        "series_name" | "seriesName" | "deployment_series_name" | "deploymentSeriesName" => {
            DynamicKey::SeriesName
        }
        "pinned_version"
        | "pinnedVersion"
        | "parent_pinned_deployment_version"
        | "parentPinnedDeploymentVersion" => DynamicKey::PinnedVersion,
        _ => return None,
    })
}

fn is_metadata_container_key(key: &str) -> bool {
    matches!(
        key,
        "source"
            | "source_version_stamp"
            | "sourceVersionStamp"
            | "worker"
            | "worker_version"
            | "workerVersion"
            | "deployment"
            | "deployment_version"
            | "deploymentVersion"
            | "versioning_override"
            | "versioningOverride"
            | "inherited_pinned_version"
            | "inheritedPinnedVersion"
            | "last_deployment_version"
            | "lastDeploymentVersion"
            | "target_deployment_version"
            | "targetDeploymentVersion"
            | "source_deployment_version"
            | "sourceDeploymentVersion"
    )
}

fn is_marker_attributes(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("marker_name") || object.contains_key("markerName")
}

fn is_marker_payload_field(marker_attributes: bool, key: &str) -> bool {
    marker_attributes && matches!(key, "details")
}

fn expect_string<'a>(value: &'a Value, field: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("history {field} must remain a string"))
}

fn set_canonical_string(value: &mut Value, canonical: &str, field: &str) {
    expect_string(value, field);
    *value = Value::String(canonical.to_owned());
}

fn set_canonical_metadata_string(value: &mut Value, canonical: &str, field: &str) {
    let original = expect_string(value, field);
    // Empty values are the protobuf representation of an unversioned or
    // otherwise absent deployment field. Keep them empty so sanitization does
    // not opt an unversioned history into a synthetic deployment.
    if !original.is_empty() {
        *value = Value::String(canonical.to_owned());
    }
}

fn set_canonical_build_value(value: &mut Value, field: &str) {
    match value {
        Value::String(_) => set_canonical_metadata_string(value, SANITIZED_BUILD_ID, field),
        Value::Array(values) => {
            for value in values {
                set_canonical_metadata_string(value, SANITIZED_BUILD_ID, field);
            }
        }
        _ => panic!("history {field} must remain a string or string array"),
    }
}

fn collect_request_ids(value: &Value, request_ids: &mut BTreeSet<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_request_ids(value, request_ids);
            }
        }
        Value::Object(object) => {
            let marker_attributes = is_marker_attributes(object);
            for (key, value) in object {
                if is_marker_payload_field(marker_attributes, key) {
                    // Marker details are encoded payload bytes. Their content
                    // is opaque to this sanitizer and must survive unchanged.
                    continue;
                }
                if matches!(dynamic_key(key), Some(DynamicKey::RequestId)) {
                    request_ids.insert(
                        value
                            .as_str()
                            .expect("WorkflowHistory request_id must be a string")
                            .to_owned(),
                    );
                }
                collect_request_ids(value, request_ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[derive(Default)]
struct OriginalDynamicValues {
    all_nonempty_values: BTreeSet<String>,
    workflow_ids: BTreeSet<String>,
    identities: BTreeSet<String>,
    request_ids: BTreeSet<String>,
    run_ids: BTreeSet<String>,
    build_ids: BTreeSet<String>,
    worker_deployment_versions: BTreeSet<String>,
    deployment_names: BTreeSet<String>,
    series_names: BTreeSet<String>,
    pinned_versions: BTreeSet<String>,
}

fn remember_dynamic_value(
    category: &mut BTreeSet<String>,
    all_nonempty_values: &mut BTreeSet<String>,
    value: &str,
) {
    category.insert(value.to_owned());
    if !value.is_empty() {
        all_nonempty_values.insert(value.to_owned());
    }
}

fn collect_original_dynamic_values(value: &Value, original: &mut OriginalDynamicValues) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_original_dynamic_values(value, original);
            }
        }
        Value::Object(object) => {
            let marker_attributes = is_marker_attributes(object);
            for (key, value) in object {
                if is_marker_payload_field(marker_attributes, key) {
                    // Marker details are opaque encoded payload bytes, not
                    // history metadata that this pass is allowed to inspect.
                    continue;
                }
                match dynamic_key(key) {
                    Some(DynamicKey::WorkflowId) => {
                        remember_dynamic_value(
                            &mut original.workflow_ids,
                            &mut original.all_nonempty_values,
                            expect_string(value, "workflow_id"),
                        );
                    }
                    Some(DynamicKey::Identity) => {
                        remember_dynamic_value(
                            &mut original.identities,
                            &mut original.all_nonempty_values,
                            expect_string(value, "identity"),
                        );
                    }
                    Some(DynamicKey::RequestId) => {
                        remember_dynamic_value(
                            &mut original.request_ids,
                            &mut original.all_nonempty_values,
                            expect_string(value, "request_id"),
                        );
                    }
                    Some(DynamicKey::RunId) => {
                        remember_dynamic_value(
                            &mut original.run_ids,
                            &mut original.all_nonempty_values,
                            expect_string(value, "run_id"),
                        );
                    }
                    Some(DynamicKey::BuildId) => match value {
                        Value::String(value) => {
                            remember_dynamic_value(
                                &mut original.build_ids,
                                &mut original.all_nonempty_values,
                                value,
                            );
                        }
                        _ => panic!("history build_id must remain a string"),
                    },
                    Some(DynamicKey::BuildIds) => match value {
                        Value::Array(values) => {
                            for value in values {
                                remember_dynamic_value(
                                    &mut original.build_ids,
                                    &mut original.all_nonempty_values,
                                    expect_string(value, "build_ids"),
                                );
                            }
                        }
                        _ => panic!("history build_ids must remain a string array"),
                    },
                    Some(DynamicKey::WorkerDeploymentVersion) => {
                        remember_dynamic_value(
                            &mut original.worker_deployment_versions,
                            &mut original.all_nonempty_values,
                            expect_string(value, "worker_deployment_version"),
                        );
                    }
                    Some(DynamicKey::WorkerDeploymentName | DynamicKey::DeploymentName) => {
                        remember_dynamic_value(
                            &mut original.deployment_names,
                            &mut original.all_nonempty_values,
                            expect_string(value, "deployment_name"),
                        );
                    }
                    Some(DynamicKey::SeriesName) => {
                        remember_dynamic_value(
                            &mut original.series_names,
                            &mut original.all_nonempty_values,
                            expect_string(value, "series_name"),
                        );
                    }
                    Some(DynamicKey::PinnedVersion) => {
                        remember_dynamic_value(
                            &mut original.pinned_versions,
                            &mut original.all_nonempty_values,
                            expect_string(value, "pinned_version"),
                        );
                    }
                    None if is_metadata_container_key(key) => {
                        assert!(
                            value.is_object() || value.is_null(),
                            "history metadata container must remain an object or null"
                        );
                    }
                    None => {}
                }
                collect_original_dynamic_values(value, original);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_uuid_shape(value: &[u8]) -> bool {
    value.len() == 36
        && value.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn contains_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(36).any(is_uuid_shape)
}

fn contains_forbidden_text(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "/users/",
        "/private/",
        "/home/",
        "/tmp/",
        "/var/folders/",
        "\\users\\",
        "\\private\\",
        "\\home\\",
        "\\tmp\\",
        "authorization",
        "bearer",
        "access_token",
        "access-token",
        "refresh_token",
        "refresh-token",
        "client_secret",
        "client-secret",
        "api_key",
        "api-key",
        "password",
        "credential",
        "secret",
    ]
    .iter()
    .any(|fragment| value.contains(fragment))
}

fn assert_safe_text(value: &str, allowed_run_ids: &BTreeSet<String>) {
    assert!(
        !contains_forbidden_text(value),
        "history contains forbidden text"
    );
    if contains_uuid(value) {
        assert!(
            allowed_run_ids.contains(value),
            "history contains an unexpected UUID"
        );
    }
}

fn assert_canonical_string<'a>(value: &'a Value, expected: &str, field: &str) -> &'a str {
    let actual = expect_string(value, field);
    assert!(actual == expected, "history {field} is not canonical");
    actual
}

fn assert_empty_or_canonical_string<'a>(value: &'a Value, expected: &str, field: &str) -> &'a str {
    let actual = expect_string(value, field);
    assert!(
        actual.is_empty() || actual == expected,
        "history {field} is not empty or canonical"
    );
    actual
}

fn assert_not_original(actual: &str, originals: &BTreeSet<String>, canonical: &str) {
    assert!(
        actual.is_empty() || actual == canonical || !originals.contains(actual),
        "sanitized history retains an original dynamic value"
    );
}

fn is_canonical_dynamic_value(
    value: &str,
    request_ids: &BTreeMap<String, String>,
    allowed_run_ids: &BTreeSet<String>,
) -> bool {
    value == CORPUS_WORKFLOW_ID
        || value == SANITIZED_IDENTITY
        || value == SANITIZED_BUILD_ID
        || value == SANITIZED_DEPLOYMENT_VERSION
        || value == SANITIZED_DEPLOYMENT_NAME
        || value == SANITIZED_SERIES_NAME
        || value == SANITIZED_PINNED_VERSION
        || request_ids.values().any(|canonical| canonical == value)
        || allowed_run_ids.contains(value)
}

fn contains_noncanonical_build_id(value: &str) -> bool {
    value.match_indices(BUILD_ID_PREFIX).any(|(offset, _)| {
        let suffix = &value.as_bytes()[offset + BUILD_ID_PREFIX.len()..];
        let Some(digest) = suffix.get(..64) else {
            return false;
        };
        digest.iter().all(u8::is_ascii_hexdigit) && digest.iter().any(|byte| *byte != b'0')
    })
}

fn assert_sanitized_json(
    value: &Value,
    original: &OriginalDynamicValues,
    request_ids: &BTreeMap<String, String>,
    allowed_run_ids: &BTreeSet<String>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                assert_sanitized_json(value, original, request_ids, allowed_run_ids);
            }
        }
        Value::Object(object) => {
            let marker_attributes = is_marker_attributes(object);
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let canonical_request_ids = request_ids.values().cloned().collect::<BTreeSet<_>>();
            for key in keys {
                assert_safe_text(key, allowed_run_ids);
                let value = object
                    .get(key)
                    .expect("history object key disappeared during validation");
                if is_marker_payload_field(marker_attributes, key) {
                    // The marker details were intentionally kept byte-for-byte
                    // opaque by both collection and sanitization.
                    continue;
                }
                match dynamic_key(key) {
                    Some(DynamicKey::WorkflowId) => {
                        let actual = assert_canonical_string(value, CORPUS_WORKFLOW_ID, key);
                        assert_not_original(actual, &original.workflow_ids, CORPUS_WORKFLOW_ID);
                    }
                    Some(DynamicKey::Identity) => {
                        let actual = assert_canonical_string(value, SANITIZED_IDENTITY, key);
                        assert_not_original(actual, &original.identities, SANITIZED_IDENTITY);
                    }
                    Some(DynamicKey::RequestId) => {
                        let actual = expect_string(value, key);
                        assert!(
                            is_canonical_request_id(actual)
                                && canonical_request_ids.contains(actual),
                            "history request_id is not canonical"
                        );
                        assert!(
                            actual.is_empty() || !original.request_ids.contains(actual),
                            "history retains an original request ID"
                        );
                    }
                    Some(DynamicKey::RunId) => {
                        let actual = expect_string(value, key);
                        assert!(
                            actual.is_empty() || allowed_run_ids.contains(actual),
                            "history run_id is not canonical"
                        );
                        if !actual.is_empty() {
                            assert!(
                                !original.run_ids.contains(actual)
                                    || allowed_run_ids.contains(actual),
                                "history retains an original run ID"
                            );
                        }
                    }
                    Some(DynamicKey::BuildId) => {
                        let actual =
                            assert_empty_or_canonical_string(value, SANITIZED_BUILD_ID, key);
                        assert_not_original(actual, &original.build_ids, SANITIZED_BUILD_ID);
                    }
                    Some(DynamicKey::BuildIds) => {
                        let values = value
                            .as_array()
                            .unwrap_or_else(|| panic!("history {key} must remain a string array"));
                        for value in values {
                            let actual = assert_empty_or_canonical_string(
                                value,
                                SANITIZED_BUILD_ID,
                                "build_ids",
                            );
                            assert_not_original(actual, &original.build_ids, SANITIZED_BUILD_ID);
                        }
                    }
                    Some(DynamicKey::WorkerDeploymentVersion) => {
                        let actual = assert_empty_or_canonical_string(
                            value,
                            SANITIZED_DEPLOYMENT_VERSION,
                            key,
                        );
                        assert_not_original(
                            actual,
                            &original.worker_deployment_versions,
                            SANITIZED_DEPLOYMENT_VERSION,
                        );
                    }
                    Some(DynamicKey::WorkerDeploymentName | DynamicKey::DeploymentName) => {
                        let actual =
                            assert_empty_or_canonical_string(value, SANITIZED_DEPLOYMENT_NAME, key);
                        assert_not_original(
                            actual,
                            &original.deployment_names,
                            SANITIZED_DEPLOYMENT_NAME,
                        );
                    }
                    Some(DynamicKey::SeriesName) => {
                        let actual =
                            assert_empty_or_canonical_string(value, SANITIZED_SERIES_NAME, key);
                        assert_not_original(actual, &original.series_names, SANITIZED_SERIES_NAME);
                    }
                    Some(DynamicKey::PinnedVersion) => {
                        let actual =
                            assert_empty_or_canonical_string(value, SANITIZED_PINNED_VERSION, key);
                        assert_not_original(
                            actual,
                            &original.pinned_versions,
                            SANITIZED_PINNED_VERSION,
                        );
                    }
                    None if is_metadata_container_key(key) => {
                        assert!(
                            value.is_object() || value.is_null(),
                            "history metadata container must remain an object or null"
                        );
                    }
                    None => {}
                }
                assert_sanitized_json(value, original, request_ids, allowed_run_ids);
            }
        }
        Value::String(value) => {
            assert_safe_text(value, allowed_run_ids);
            assert!(
                !contains_noncanonical_build_id(value),
                "history contains a source-derived Build ID"
            );
            if !value.is_empty() {
                for original in &original.all_nonempty_values {
                    if value.contains(original) {
                        assert!(
                            is_canonical_dynamic_value(value, request_ids, allowed_run_ids),
                            "history retains an original dynamic value"
                        );
                    }
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn canonical_request_id_bindings(value: &Value) -> BTreeMap<String, String> {
    let mut values = BTreeSet::new();
    collect_request_ids(value, &mut values);
    let mut expected = 1u32;
    values
        .into_iter()
        .map(|value| {
            assert!(
                is_canonical_request_id(&value),
                "checked corpus request_id is not canonical"
            );
            let suffix = value
                .strip_prefix("synthetic-replay-request-")
                .expect("canonical request ID prefix");
            let sequence = suffix
                .parse::<u32>()
                .expect("canonical request ID sequence");
            assert_eq!(
                sequence, expected,
                "checked corpus request IDs are not contiguous"
            );
            expected += 1;
            (value.clone(), value)
        })
        .collect()
}

fn is_canonical_request_id(value: &str) -> bool {
    value
        .strip_prefix("synthetic-replay-request-")
        .is_some_and(|suffix| {
            suffix.len() == 4
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
                && suffix.parse::<u32>().is_ok_and(|sequence| sequence > 0)
        })
}

fn assert_request_id_bindings(request_ids: &BTreeMap<String, String>) {
    let mut canonical_ids = request_ids.values().cloned().collect::<Vec<_>>();
    canonical_ids.sort_unstable();
    canonical_ids.dedup();
    for (index, request_id) in canonical_ids.iter().enumerate() {
        assert!(
            is_canonical_request_id(request_id),
            "request ID is not canonical"
        );
        assert_eq!(
            request_id,
            &format!("synthetic-replay-request-{:04}", index + 1),
            "request IDs are not contiguous"
        );
    }
    assert_eq!(
        canonical_ids.len(),
        request_ids.len(),
        "request IDs must map one-to-one"
    );
}

fn canonical_run_ids() -> BTreeSet<String> {
    [LEGACY_RUN_A, LEGACY_RUN_B, CURRENT_RUN_A, CURRENT_RUN_B]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// Validate a checked or newly-generated history after all machine-specific
/// values have been sanitized. This is deliberately public within the test
/// support module so checked-corpus loading cannot bypass the same safety
/// boundary used by live export.
pub(crate) fn assert_history_json_sanitized(value: &Value) {
    assert_history_json_sanitized_with_forbidden_values(value, &BTreeSet::new());
}

pub(crate) fn assert_history_json_sanitized_with_build_id(value: &Value, build_id: &str) {
    let forbidden_values = [build_id.to_owned()].into_iter().collect();
    assert_history_json_sanitized_with_forbidden_values(value, &forbidden_values);
}

fn assert_history_json_sanitized_with_forbidden_values(
    value: &Value,
    forbidden_values: &BTreeSet<String>,
) {
    assert_history_json_canonical(value);
    let request_ids = canonical_request_id_bindings(value);
    assert_request_id_bindings(&request_ids);
    let mut original = OriginalDynamicValues::default();
    original.all_nonempty_values.extend(
        forbidden_values
            .iter()
            .filter(|value| !value.is_empty())
            .cloned(),
    );
    let allowed_run_ids = canonical_run_ids();
    assert_sanitized_json(value, &original, &request_ids, &allowed_run_ids);
}

fn sanitize_json(
    value: &mut Value,
    run_ids: &BTreeMap<String, String>,
    request_ids: &mut BTreeMap<String, String>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                sanitize_json(value, run_ids, request_ids);
            }
        }
        Value::Object(object) => {
            let event_id = object.get("event_id").and_then(Value::as_i64);
            let marker_attributes = is_marker_attributes(object);
            let keys = object.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                let value = object
                    .get_mut(&key)
                    .expect("history object key disappeared during sanitization");
                if is_marker_payload_field(marker_attributes, &key) {
                    // Marker details contain the patch marker's encoded
                    // payload. Never decode, rewrite, or normalize those
                    // bytes while removing machine-specific history data.
                    continue;
                }
                match dynamic_key(&key) {
                    Some(DynamicKey::WorkflowId) => {
                        set_canonical_string(value, CORPUS_WORKFLOW_ID, "workflow_id")
                    }
                    Some(DynamicKey::Identity) => {
                        set_canonical_string(value, SANITIZED_IDENTITY, "identity")
                    }
                    Some(DynamicKey::RequestId) => {
                        let request_id = expect_string(value, "request_id");
                        let next_id = request_ids.len() + 1;
                        let canonical_id = request_ids
                            .entry(request_id.to_owned())
                            .or_insert_with(|| format!("synthetic-replay-request-{next_id:04}"))
                            .clone();
                        *value = Value::String(canonical_id);
                    }
                    Some(DynamicKey::RunId) => {
                        let run_id = expect_string(value, "run_id");
                        *value = Value::String(replace_run_id(run_id, run_ids));
                    }
                    Some(DynamicKey::BuildId) => {
                        set_canonical_build_value(value, "build_id");
                    }
                    Some(DynamicKey::BuildIds) => {
                        set_canonical_build_value(value, "build_ids");
                    }
                    Some(DynamicKey::WorkerDeploymentVersion) => set_canonical_metadata_string(
                        value,
                        SANITIZED_DEPLOYMENT_VERSION,
                        "worker_deployment_version",
                    ),
                    Some(DynamicKey::WorkerDeploymentName) => set_canonical_metadata_string(
                        value,
                        SANITIZED_DEPLOYMENT_NAME,
                        "worker_deployment_name",
                    ),
                    Some(DynamicKey::DeploymentName) => set_canonical_metadata_string(
                        value,
                        SANITIZED_DEPLOYMENT_NAME,
                        "deployment_name",
                    ),
                    Some(DynamicKey::SeriesName) => {
                        set_canonical_metadata_string(value, SANITIZED_SERIES_NAME, "series_name")
                    }
                    Some(DynamicKey::PinnedVersion) => set_canonical_metadata_string(
                        value,
                        SANITIZED_PINNED_VERSION,
                        "pinned_version",
                    ),
                    None if is_metadata_container_key(&key) => {
                        assert!(
                            value.is_object() || value.is_null(),
                            "history metadata container must remain an object or null"
                        );
                    }
                    None => match key.as_str() {
                        // WorkflowHistory's protobuf JSON serializer uses snake_case
                        // fields and RFC3339 strings. Keep this list deliberately
                        // string-only except for the explicit numeric canonicalization
                        // below. Numeric protobuf fields stay JSON numbers. The
                        // protobuf JSON implementation uses snake_case for the
                        // outer HistoryEvent and camelCase for nested messages.
                        "event_time" | "eventTime" if event_id.is_some() => {
                            set_canonical_string(value, "1970-01-01T00:00:00Z", "event_time");
                        }
                        "version" if event_id.is_some() => {
                            assert!(value.is_number(), "event version must remain numeric");
                            *value = Value::Number(0.into());
                        }
                        "task_id" if event_id.is_some() => {
                            assert!(value.is_number(), "event task_id must remain numeric");
                            *value =
                                Value::Number(event_id.expect("event_id was checked above").into());
                        }
                        _ => {}
                    },
                }
                sanitize_json(value, run_ids, request_ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn event_numeric_fields(value: &Value) -> Vec<(i64, i32, i64, i64)> {
    value
        .get("events")
        .and_then(Value::as_array)
        .expect("history JSON must contain events")
        .iter()
        .map(|event| {
            (
                event
                    .get("event_id")
                    .and_then(Value::as_i64)
                    .expect("event_id must remain numeric"),
                event
                    .get("event_type")
                    .and_then(Value::as_i64)
                    .expect("event_type must remain numeric") as i32,
                event
                    .get("version")
                    .and_then(Value::as_i64)
                    .expect("version must remain numeric"),
                event
                    .get("task_id")
                    .and_then(Value::as_i64)
                    .expect("task_id must remain numeric"),
            )
        })
        .collect()
}

pub(crate) fn assert_history_json_canonical(value: &Value) {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .expect("history JSON must contain events");
    assert!(!events.is_empty());
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.get("event_id").and_then(Value::as_i64),
            Some((index + 1) as i64)
        );
        let event_time = event
            .get("event_time")
            .and_then(Value::as_str)
            .expect("WorkflowHistory JSON must use RFC3339 event_time");
        DateTime::parse_from_rfc3339(event_time).expect("event_time must be RFC3339");
    }
}

pub(crate) fn sanitized_history(
    history: &WorkflowHistory,
    run_a: &str,
    run_b: &str,
    fixed_a: &str,
    fixed_b: &str,
) -> Vec<u8> {
    let mut run_ids = BTreeMap::new();
    run_ids.insert(run_a.to_owned(), fixed_a.to_owned());
    run_ids.insert(run_b.to_owned(), fixed_b.to_owned());
    let original_bytes = history.to_json().expect("encode sidecar history as JSON");
    let mut value: Value =
        serde_json::from_slice(&original_bytes).expect("decode sidecar history JSON");
    assert_history_json_canonical(&value);
    let original_numeric_fields = event_numeric_fields(&value);
    let mut original_dynamic_values = OriginalDynamicValues::default();
    collect_original_dynamic_values(&value, &mut original_dynamic_values);
    let mut request_id_values = BTreeSet::new();
    collect_request_ids(&value, &mut request_id_values);
    let mut request_ids = request_id_values
        .into_iter()
        .enumerate()
        .map(|(index, request_id)| {
            (
                request_id,
                format!("synthetic-replay-request-{:04}", index + 1),
            )
        })
        .collect::<BTreeMap<_, _>>();
    sanitize_json(&mut value, &run_ids, &mut request_ids);
    assert_history_json_canonical(&value);
    assert_request_id_bindings(&request_ids);
    let allowed_run_ids = canonical_run_ids();
    assert_sanitized_json(
        &value,
        &original_dynamic_values,
        &request_ids,
        &allowed_run_ids,
    );
    let sanitized_numeric_fields = event_numeric_fields(&value);
    assert_eq!(
        sanitized_numeric_fields.len(),
        original_numeric_fields.len()
    );
    let mut event_ids = BTreeSet::new();
    let mut task_ids = BTreeSet::new();
    for (
        (original_event_id, original_event_type, _, _),
        (event_id, event_type, version, task_id),
    ) in original_numeric_fields
        .iter()
        .zip(sanitized_numeric_fields.iter())
    {
        assert_eq!(event_id, original_event_id);
        assert_eq!(event_type, original_event_type);
        assert_eq!(*version, 0);
        assert_eq!(*task_id, *event_id);
        assert!(event_ids.insert(*event_id), "event IDs must be unique");
        assert!(task_ids.insert(*task_id), "task IDs must be unique");
    }
    let bytes = serde_json::to_vec(&value).expect("encode sanitized history JSON");
    let parsed = WorkflowHistory::from_json(&bytes).expect("sanitized history must decode");
    assert!(!parsed.events().is_empty());
    let reparsed_value: Value = serde_json::from_slice(
        &parsed
            .to_json()
            .expect("re-encode sanitized history JSON after parse"),
    )
    .expect("reparse sanitized history JSON");
    assert_eq!(reparsed_value, value);
    assert_sanitized_json(
        &reparsed_value,
        &original_dynamic_values,
        &request_ids,
        &allowed_run_ids,
    );
    bytes
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use serde_json::{Value, json};

    use super::*;

    fn dynamic_history() -> Value {
        json!({
            "events": [{
                "event_id": 1,
                "event_type": 1,
                "version": 37,
                "task_id": 99,
                "event_time": "2024-01-02T03:04:05+00:00",
                "workflow_id": "original-workflow-id",
                "identity": "original-worker-identity",
                "request_id": "request-z",
                "original_execution_run_id": "original-run-a",
                "firstExecutionRunId": "original-run-a",
                "continued_execution_run_id": "original-run-b",
                "newExecutionRunId": "original-run-b",
                "base_run_id": "original-run-a",
                "newRunId": "original-run-b",
                "build_id": "original-build-a",
                "buildId": "original-build-b",
                "binaryChecksum": "original-checksum",
                "inherited_build_id": "original-inherited-build",
                "build_ids": ["original-build-list", ""],
                "worker_deployment_version": "original-worker-version",
                "workerDeploymentName": "original-worker-name",
                "deployment_name": "original-deployment-name",
                "seriesName": "original-series-name",
                "pinned_version": "original-pinned-version",
                "source": {
                    "sourceBuildId": "original-source-build",
                    "requestId": "request-a"
                },
                "worker": {
                    "build_id": "original-worker-build",
                    "worker_deployment_version": ""
                },
                "deployment": {
                    "deploymentBuildId": "original-deployment-build",
                    "deploymentName": "original-nested-deployment"
                },
                "safe_echo": "safe text",
                "marker_name": "Version",
                "details": "AQID/wrapped-marker-payload"
            }]
        })
    }

    #[test]
    fn sanitizer_canonicalizes_dynamic_aliases_and_preserves_empty_metadata() {
        let mut value = dynamic_history();
        let marker_details = value["events"][0]["details"].clone();
        let mut run_ids = BTreeMap::new();
        run_ids.insert("original-run-a".to_owned(), LEGACY_RUN_A.to_owned());
        run_ids.insert("original-run-b".to_owned(), LEGACY_RUN_B.to_owned());
        let mut original = OriginalDynamicValues::default();
        collect_original_dynamic_values(&value, &mut original);
        let mut request_values = BTreeSet::new();
        collect_request_ids(&value, &mut request_values);
        let mut request_ids = request_values
            .into_iter()
            .enumerate()
            .map(|(index, request_id)| {
                (
                    request_id,
                    format!("synthetic-replay-request-{:04}", index + 1),
                )
            })
            .collect::<BTreeMap<_, _>>();

        sanitize_json(&mut value, &run_ids, &mut request_ids);
        assert_request_id_bindings(&request_ids);
        assert_history_json_canonical(&value);
        let allowed_run_ids = canonical_run_ids();
        assert_sanitized_json(&value, &original, &request_ids, &allowed_run_ids);

        let event = &value["events"][0];
        assert_eq!(event["workflow_id"], CORPUS_WORKFLOW_ID);
        assert_eq!(event["identity"], SANITIZED_IDENTITY);
        assert_eq!(event["request_id"], "synthetic-replay-request-0002");
        assert_eq!(
            event["source"]["requestId"],
            "synthetic-replay-request-0001"
        );
        assert_eq!(event["original_execution_run_id"], LEGACY_RUN_A);
        assert_eq!(event["newExecutionRunId"], LEGACY_RUN_B);
        assert_eq!(event["build_id"], SANITIZED_BUILD_ID);
        assert_eq!(event["buildId"], SANITIZED_BUILD_ID);
        assert_eq!(event["build_ids"][1], "");
        assert_eq!(
            event["worker_deployment_version"],
            SANITIZED_DEPLOYMENT_VERSION
        );
        assert_eq!(event["worker"]["worker_deployment_version"], "");
        assert_eq!(
            event["deployment"]["deploymentName"],
            SANITIZED_DEPLOYMENT_NAME
        );
        assert_eq!(event["details"], marker_details);
        assert_eq!(event["event_time"], "1970-01-01T00:00:00Z");
        assert_eq!(event["version"], 0);
        assert_eq!(event["task_id"], 1);

        // The public checked-corpus boundary uses the same deterministic
        // postcondition without relying on the original export's side data.
        assert_history_json_sanitized(&value);
    }

    #[test]
    fn postcondition_rejects_original_values_outside_allowlisted_fields() {
        let mut value = dynamic_history();
        let mut run_ids = BTreeMap::new();
        run_ids.insert("original-run-a".to_owned(), LEGACY_RUN_A.to_owned());
        run_ids.insert("original-run-b".to_owned(), LEGACY_RUN_B.to_owned());
        let mut original = OriginalDynamicValues::default();
        collect_original_dynamic_values(&value, &mut original);
        let mut request_ids = BTreeMap::new();
        request_ids.insert(
            "request-a".to_owned(),
            "synthetic-replay-request-0001".to_owned(),
        );
        request_ids.insert(
            "request-z".to_owned(),
            "synthetic-replay-request-0002".to_owned(),
        );
        sanitize_json(&mut value, &run_ids, &mut request_ids);
        value["events"][0]["safe_echo"] = Value::String("original-build-a".to_owned());
        let panic = catch_unwind(AssertUnwindSafe(|| {
            assert_sanitized_json(&value, &original, &request_ids, &canonical_run_ids());
        }));
        assert!(panic.is_err());
    }

    #[test]
    fn checked_postcondition_rejects_noncanonical_request_ids_and_build_ids() {
        let mut value = dynamic_history();
        let event = value["events"][0].as_object_mut().expect("event object");
        event.insert(
            "request_id".to_owned(),
            Value::String("request-id-from-source".to_owned()),
        );
        event.insert(
            "unknown".to_owned(),
            Value::String(
                "nagi/0.1.0/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            ),
        );
        let panic = catch_unwind(AssertUnwindSafe(|| {
            assert_history_json_sanitized(&value);
        }));
        assert!(panic.is_err());
    }
}

use std::time::Duration;

use sha2::{Digest, Sha256};
use temporalio_client::{
    Client, ClientOptions, Connection, ConnectionOptions, RetryOptions, RpcOptions, Url,
    WorkflowExecutionInfo, WorkflowFetchHistoryOptions, WorkflowGetResultOptions, WorkflowHandle,
    WorkflowHistory, WorkflowStartOptions, errors::WorkflowGetResultError,
};
use temporalio_common::{
    protos::temporal::api::history::v1::{
        WorkflowExecutionCompletedEventAttributes, WorkflowExecutionStartedEventAttributes,
        history_event::Attributes,
    },
    protos::{
        PATCHED_MARKER_DETAILS_KEY,
        constants::PATCH_MARKER_NAME,
        coresdk::common::decode_change_marker_details,
        temporal::api::{
            enums::v1::{
                ContinueAsNewInitiator, TaskQueueKind, TaskQueueType, WorkerVersioningMode,
            },
            taskqueue::v1::TaskQueue,
            workflowservice::v1::DescribeTaskQueueRequest,
        },
    },
    worker::WorkerDeploymentOptions,
};
use temporalio_sdk::{
    Runtime, Worker, WorkerOptions, WorkflowContext, WorkflowResult,
    workflows::{workflow, workflow_methods},
};

use super::*;

#[path = "replay.rs"]
pub(crate) mod replay;

pub(crate) fn workflow_build_id() -> String {
    let digest = Sha256::digest(include_bytes!("workflows.rs"));
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("write Build ID digest");
    }
    format!("{BUILD_ID_PREFIX}{encoded}")
}

/// The pre-patch source definition. It deliberately does not call
/// `WorkflowContext::patched`, so a marker-bearing history must not replay
/// against it.
#[workflow]
#[derive(Default)]
struct LegacyReplayWorkflow;

#[workflow_methods]
impl LegacyReplayWorkflow {
    #[run(name = "ReplayCompatibilityWorkflow")]
    async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: ReplayInput,
    ) -> WorkflowResult<ReplayResult> {
        if input.generation == 0 {
            ctx.continue_as_new(
                ReplayInput {
                    generation: 1,
                    carried_state: format!("{}:legacy", input.carried_state),
                    build_id: input.build_id.clone(),
                },
                temporalio_sdk::ContinueAsNewOptions::default(),
            )?;
        }
        Ok(ReplayResult {
            carried_state: input.carried_state,
            patch_active: false,
            build_id: input.build_id,
        })
    }
}

/// The current source definition records a stable patch marker on each run.
/// Continue-As-New carries the full state needed by the next run instead of
/// relying on process-local workflow state.
#[workflow]
#[derive(Default)]
struct ReplayCompatibilityWorkflow;

#[workflow_methods]
impl ReplayCompatibilityWorkflow {
    #[run(name = "ReplayCompatibilityWorkflow")]
    async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: ReplayInput,
    ) -> WorkflowResult<ReplayResult> {
        let patch_active = ctx.patched(PATCH_ID);
        if input.generation == 0 {
            ctx.continue_as_new(
                ReplayInput {
                    generation: 1,
                    carried_state: format!(
                        "{}:{}",
                        input.carried_state,
                        if patch_active { "current" } else { "legacy" }
                    ),
                    build_id: input.build_id.clone(),
                },
                temporalio_sdk::ContinueAsNewOptions::default(),
            )?;
        }
        Ok(ReplayResult {
            carried_state: input.carried_state,
            patch_active,
            build_id: input.build_id,
        })
    }
}

/// A deliberately incompatible current definition used only to prove that a
/// marker for `PATCH_ID` is not silently accepted under another patch ID.
#[workflow]
#[derive(Default)]
struct MismatchedPatchReplayWorkflow;

#[workflow_methods]
impl MismatchedPatchReplayWorkflow {
    #[run(name = "ReplayCompatibilityWorkflow")]
    async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: ReplayInput,
    ) -> WorkflowResult<ReplayResult> {
        let patch_active = ctx.patched(MISMATCHED_PATCH_ID);
        if input.generation == 0 {
            ctx.continue_as_new(
                ReplayInput {
                    generation: 1,
                    carried_state: format!(
                        "{}:{}",
                        input.carried_state,
                        if patch_active { "current" } else { "legacy" }
                    ),
                    build_id: input.build_id.clone(),
                },
                temporalio_sdk::ContinueAsNewOptions::default(),
            )?;
        }
        Ok(ReplayResult {
            carried_state: input.carried_state,
            patch_active,
            build_id: input.build_id,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RunChain {
    pub(crate) workflow_id: &'static str,
    pub(crate) run_a: String,
    pub(crate) run_b: String,
    pub(crate) history_a: WorkflowHistory,
    pub(crate) history_b: WorkflowHistory,
}

struct ChainExpectation {
    workflow_id: &'static str,
    carried_state: &'static str,
    patch_active: bool,
}

fn bounded_rpc_options() -> RpcOptions {
    RpcOptions::builder()
        .timeout(Duration::from_secs(5))
        .retry_options(RetryOptions::no_retries())
        .build()
}

pub(crate) fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be supplied by the contract task"))
}

pub(crate) fn parse_loopback_address(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    let port = url.port()?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || port == 0
    {
        return None;
    }
    Some(url)
}

pub(crate) fn valid_run_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) async fn connect_client() -> (Client, String) {
    let address = parse_loopback_address(&required_env(ADDRESS_ENV))
        .expect("contract task must supply the exact IPv4 loopback URL");
    let namespace = required_env(NAMESPACE_ENV);
    assert!(!namespace.is_empty());
    let connection = Connection::connect(
        ConnectionOptions::new(address)
            .identity(CLIENT_IDENTITY)
            .connect_timeout(Duration::from_secs(5))
            .build(),
    )
    .await
    .expect("connect to the local Temporal sidecar");
    let client = Client::new(connection, ClientOptions::new(namespace.clone()).build())
        .expect("create Temporal client");
    (client, namespace)
}

#[allow(deprecated)]
pub(crate) async fn assert_worker_pollers(
    client: &Client,
    namespace: &str,
    expected_build_id: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut workflow_service = client.connection().workflow_service();
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            workflow_service.describe_task_queue(temporalio_client::tonic::Request::new(
                DescribeTaskQueueRequest {
                    namespace: namespace.to_owned(),
                    task_queue: Some(TaskQueue {
                        name: TASK_QUEUE.to_owned(),
                        kind: TaskQueueKind::Normal as i32,
                        ..Default::default()
                    }),
                    task_queue_type: TaskQueueType::Workflow as i32,
                    report_pollers: true,
                    ..Default::default()
                },
            )),
        )
        .await;
        if let Ok(Ok(response)) = response {
            let pollers = response.into_inner().pollers;
            let matching = pollers
                .iter()
                .filter(|poller| poller.identity == WORKER_IDENTITY)
                .collect::<Vec<_>>();
            if matching.len() == 1 {
                let deployment_options = matching[0]
                    .deployment_options
                    .as_ref()
                    .expect("worker poller must advertise deployment options");
                assert_eq!(deployment_options.deployment_name, "");
                assert_eq!(deployment_options.build_id, expected_build_id);
                assert_eq!(
                    WorkerVersioningMode::try_from(deployment_options.worker_versioning_mode)
                        .expect("worker poller must advertise a known versioning mode"),
                    WorkerVersioningMode::Unversioned
                );
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "worker poller did not advertise its build ID before the deadline"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) async fn fetch_exact_history<W>(handle: &WorkflowHandle<Client, W>) -> WorkflowHistory
where
    W: temporalio_workflow::common::HasWorkflowDefinition,
{
    tokio::time::timeout(
        Duration::from_secs(10),
        handle.fetch_history(
            WorkflowFetchHistoryOptions::builder()
                .rpc_options(bounded_rpc_options())
                .build(),
        ),
    )
    .await
    .expect("exact history fetch deadline")
    .expect("fetch exact run history")
}

fn started_attributes(history: &WorkflowHistory) -> &WorkflowExecutionStartedEventAttributes {
    match history
        .events()
        .first()
        .and_then(|event| event.attributes.as_ref())
    {
        Some(Attributes::WorkflowExecutionStartedEventAttributes(attributes)) => attributes,
        _ => panic!("history must begin with WorkflowExecutionStarted"),
    }
}

pub(crate) fn validate_history_shape(
    history: &WorkflowHistory,
    workflow_id: &str,
    expected_run_id: &str,
    expected_first_run_id: &str,
    expected_continued_run_id: Option<&str>,
) {
    assert_eq!(history.workflow_id(), Some(workflow_id));
    assert!(valid_run_id(expected_run_id));
    assert!(valid_run_id(expected_first_run_id));
    let events = history.events();
    assert!(!events.is_empty());
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.event_id, (index + 1) as i64);
        assert!(
            event.attributes.is_some(),
            "history event must have attributes"
        );
    }
    let started = started_attributes(history);
    assert_eq!(started.workflow_id, workflow_id);
    assert_eq!(
        started
            .workflow_type
            .as_ref()
            .map(|workflow| workflow.name.as_str()),
        Some(WORKFLOW_TYPE)
    );
    assert_eq!(
        started.task_queue.as_ref().map(|queue| queue.name.as_str()),
        Some(TASK_QUEUE)
    );
    assert_eq!(started.first_execution_run_id, expected_first_run_id);
    assert_eq!(started.original_execution_run_id, expected_run_id);
    assert_eq!(
        started.continued_execution_run_id,
        expected_continued_run_id.unwrap_or("")
    );
}

pub(crate) fn continue_as_new_run_id(history: &WorkflowHistory) -> String {
    match history
        .events()
        .last()
        .and_then(|event| event.attributes.as_ref())
    {
        Some(Attributes::WorkflowExecutionContinuedAsNewEventAttributes(attributes)) => {
            assert!(valid_run_id(&attributes.new_execution_run_id));
            // temporalio-sdk 0.7.0 does not expose an initiator field on its
            // Continue-As-New command, so the server records the self-request
            // as `UNSPECIFIED`. Reject retry/cron continuations while accepting
            // both representations of an SDK-issued Continue-As-New.
            let initiator = ContinueAsNewInitiator::try_from(attributes.initiator)
                .expect("Continue-As-New event must advertise a known initiator");
            assert!(
                matches!(
                    initiator,
                    ContinueAsNewInitiator::Workflow | ContinueAsNewInitiator::Unspecified
                ),
                "run A must be continued by the workflow SDK"
            );
            attributes.new_execution_run_id.clone()
        }
        _ => panic!("run A must terminate with Continue-As-New"),
    }
}

pub(crate) fn assert_completed(history: &WorkflowHistory) {
    match history
        .events()
        .last()
        .and_then(|event| event.attributes.as_ref())
    {
        Some(Attributes::WorkflowExecutionCompletedEventAttributes(
            WorkflowExecutionCompletedEventAttributes { .. },
        )) => {}
        _ => panic!("run B must terminate with WorkflowExecutionCompleted"),
    }
}

pub(crate) fn patch_markers(history: &WorkflowHistory) -> Vec<(String, bool)> {
    history
        .events()
        .iter()
        .filter_map(|event| match event.attributes.as_ref() {
            Some(Attributes::MarkerRecordedEventAttributes(attributes))
                if attributes.marker_name == PATCH_MARKER_NAME =>
            {
                let details = attributes
                    .details
                    .get(PATCHED_MARKER_DETAILS_KEY)
                    .expect("patch marker must contain patch-data details");
                assert_eq!(attributes.details.len(), 1);
                let (patch_id, deprecated) = decode_change_marker_details(&attributes.details)
                    .expect("patch marker details must decode");
                assert_eq!(patch_id, PATCH_ID);
                assert_eq!(details.payloads.len(), 1);
                Some((patch_id, deprecated))
            }
            Some(Attributes::MarkerRecordedEventAttributes(attributes)) => {
                panic!("unexpected non-patch marker: {}", attributes.marker_name)
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn assert_chain(
    chain: &RunChain,
    expected_first_run_id: &str,
    expected_patch: bool,
    expected_carried_state: &str,
) {
    validate_history_shape(
        &chain.history_a,
        chain.workflow_id,
        &chain.run_a,
        expected_first_run_id,
        None,
    );
    validate_history_shape(
        &chain.history_b,
        chain.workflow_id,
        &chain.run_b,
        expected_first_run_id,
        Some(&chain.run_a),
    );
    assert_ne!(chain.run_a, chain.run_b);
    assert_eq!(continue_as_new_run_id(&chain.history_a), chain.run_b);
    assert_completed(&chain.history_b);
    assert!(chain.history_a.events().iter().any(|event| matches!(
        event.attributes,
        Some(Attributes::WorkflowExecutionContinuedAsNewEventAttributes(
            _
        ))
    )));
    assert!(!chain.history_b.events().iter().any(|event| matches!(
        event.attributes,
        Some(Attributes::WorkflowExecutionContinuedAsNewEventAttributes(
            _
        ))
    )));
    let expected_markers = if expected_patch {
        vec![(PATCH_ID.to_owned(), false)]
    } else {
        Vec::new()
    };
    assert_eq!(patch_markers(&chain.history_a), expected_markers);
    assert_eq!(patch_markers(&chain.history_b), expected_markers);
    assert!(
        expected_carried_state.contains("legacy") || expected_carried_state.contains("current")
    );
}

async fn complete_chain<W>(
    client: &Client,
    namespace: &str,
    handle: WorkflowHandle<Client, W>,
    shutdown: impl Fn(),
    worker_task: tokio::task::JoinHandle<Result<(), temporalio_sdk::WorkerRunError>>,
    expectation: ChainExpectation,
) -> RunChain
where
    W: temporalio_workflow::common::HasWorkflowDefinition<Output = ReplayResult>,
{
    let run_a = handle.run_id().expect("run A ID").to_owned();
    let result_a = tokio::time::timeout(
        Duration::from_secs(20),
        handle.get_result(
            WorkflowGetResultOptions::builder()
                .follow_runs(false)
                .rpc_options(bounded_rpc_options())
                .build(),
        ),
    )
    .await
    .expect("run A result deadline");
    assert!(matches!(
        result_a,
        Err(WorkflowGetResultError::ContinuedAsNew)
    ));
    let history_a = fetch_exact_history(&handle).await;
    let run_b = continue_as_new_run_id(&history_a);
    let continued: WorkflowHandle<Client, W> = WorkflowHandle::new(
        client.clone(),
        WorkflowExecutionInfo {
            namespace: namespace.to_owned(),
            workflow_id: expectation.workflow_id.to_owned(),
            run_id: Some(run_b.clone()),
            first_execution_run_id: Some(run_a.clone()),
        },
    );
    let result_b = tokio::time::timeout(
        Duration::from_secs(20),
        continued.get_result(
            WorkflowGetResultOptions::builder()
                .follow_runs(false)
                .rpc_options(bounded_rpc_options())
                .build(),
        ),
    )
    .await
    .expect("run B result deadline")
    .expect("run B should complete");
    assert_eq!(result_b.carried_state, expectation.carried_state);
    assert_eq!(result_b.patch_active, expectation.patch_active);
    assert_eq!(result_b.build_id, SANITIZED_BUILD_ID);
    let history_b = fetch_exact_history(&continued).await;
    shutdown();
    tokio::time::timeout(Duration::from_secs(10), worker_task)
        .await
        .expect("worker shutdown deadline")
        .expect("worker task join")
        .expect("worker shutdown");
    RunChain {
        workflow_id: expectation.workflow_id,
        run_a,
        run_b,
        history_a,
        history_b,
    }
}

pub(crate) async fn run_legacy_chain(
    client: &Client,
    namespace: &str,
    runtime: &Runtime,
) -> RunChain {
    // This source-derived Build ID is the actual poller identity witness; it
    // is asserted live and recorded in the export manifest. It is distinct
    // from the fixed synthetic Build ID carried by workflow payloads/results;
    // publication canonicalizes all server-recorded worker/deployment metadata.
    let build_id = workflow_build_id();
    let options = WorkerOptions::new(TASK_QUEUE)
        .client_identity_override(WORKER_IDENTITY.to_owned())
        .deployment_options(WorkerDeploymentOptions::from_build_id(build_id.clone()))
        .register_workflow::<LegacyReplayWorkflow>()
        .expect("register legacy replay workflow")
        .build();
    assert!(!options.deployment_options.use_worker_versioning);
    assert_eq!(options.deployment_options.version.build_id, build_id);
    let mut worker = Worker::new(runtime, client.clone(), options).expect("create legacy worker");
    let shutdown = worker.shutdown_handle();
    let worker_task = tokio::task::spawn_local(async move { worker.run().await });
    assert_worker_pollers(client, namespace, &build_id).await;
    let handle = client
        .start_workflow(
            LegacyReplayWorkflow::run,
            ReplayInput {
                generation: 0,
                carried_state: "synthetic".to_owned(),
                build_id: SANITIZED_BUILD_ID.to_owned(),
            },
            WorkflowStartOptions::new(TASK_QUEUE, LEGACY_WORKFLOW_ID)
                .rpc_options(bounded_rpc_options())
                .build(),
        )
        .await
        .expect("start legacy replay workflow");
    complete_chain(
        client,
        namespace,
        handle,
        shutdown,
        worker_task,
        ChainExpectation {
            workflow_id: LEGACY_WORKFLOW_ID,
            carried_state: "synthetic:legacy",
            patch_active: false,
        },
    )
    .await
}

pub(crate) async fn run_current_chain(
    client: &Client,
    namespace: &str,
    runtime: &Runtime,
) -> RunChain {
    // The worker poller must advertise the actual source-derived Build ID,
    // while workflow input/result use the fixed synthetic payload value. The
    // exported History sanitizer canonicalizes worker/deployment metadata.
    let build_id = workflow_build_id();
    let options = WorkerOptions::new(TASK_QUEUE)
        .client_identity_override(WORKER_IDENTITY.to_owned())
        .deployment_options(WorkerDeploymentOptions::from_build_id(build_id.clone()))
        .register_workflow::<ReplayCompatibilityWorkflow>()
        .expect("register current replay workflow")
        .build();
    assert!(!options.deployment_options.use_worker_versioning);
    assert_eq!(options.deployment_options.version.build_id, build_id);
    let mut worker = Worker::new(runtime, client.clone(), options).expect("create current worker");
    let shutdown = worker.shutdown_handle();
    let worker_task = tokio::task::spawn_local(async move { worker.run().await });
    assert_worker_pollers(client, namespace, &build_id).await;
    let handle = client
        .start_workflow(
            ReplayCompatibilityWorkflow::run,
            ReplayInput {
                generation: 0,
                carried_state: "synthetic".to_owned(),
                build_id: SANITIZED_BUILD_ID.to_owned(),
            },
            WorkflowStartOptions::new(TASK_QUEUE, CURRENT_WORKFLOW_ID)
                .rpc_options(bounded_rpc_options())
                .build(),
        )
        .await
        .expect("start current replay workflow");
    complete_chain(
        client,
        namespace,
        handle,
        shutdown,
        worker_task,
        ChainExpectation {
            workflow_id: CURRENT_WORKFLOW_ID,
            carried_state: "synthetic:current",
            patch_active: true,
        },
    )
    .await
}

#![cfg(target_os = "macos")]

//! Live, credential-free checks for Temporal Activity recovery.
//!
//! This test is intentionally behind `temporal-activity-contract` and the
//! macOS-only `contract:temporal-activities` task. The task supplies a
//! loopback address for a pinned local Temporal sidecar and selects one of
//! three private driver phases. A worker is started as a separate process so
//! the shell harness can terminate and reap it; no filesystem checkpoint is
//! used by a replacement worker.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use temporalio_client::{
    Client, ClientOptions, Connection, ConnectionOptions, RetryOptions, RpcOptions, Url,
    WorkflowCancelOptions, WorkflowExecutionInfo, WorkflowFetchHistoryOptions,
    WorkflowGetResultOptions, WorkflowHandle, WorkflowQueryOptions, WorkflowSignalOptions,
    WorkflowStartOptions,
};
use temporalio_common::{
    RetryPolicy,
    protos::temporal::api::history::v1::{
        WorkflowExecutionStartedEventAttributes, history_event::Attributes,
    },
};
use temporalio_sdk::{
    ActivityCancellationType, ActivityExecutionError, ActivityOptions, Runtime,
    SyncWorkflowContext, Worker, WorkerOptions, WorkflowCancellationToken, WorkflowContext,
    WorkflowContextView, WorkflowResult,
    activities::{ActivityContext, ActivityError, activities},
    error::ApplicationFailure,
    workflows::{workflow, workflow_methods},
};

const ADDRESS_ENV: &str = "NAGI_TEMPORAL_ACTIVITY_ADDRESS";
const NAMESPACE_ENV: &str = "NAGI_TEMPORAL_ACTIVITY_NAMESPACE";
const ROLE_ENV: &str = "NAGI_TEMPORAL_ACTIVITY_ROLE";
const PHASE_ENV: &str = "NAGI_TEMPORAL_ACTIVITY_PHASE";
const RUN_ID_ENV: &str = "NAGI_TEMPORAL_ACTIVITY_RUN_ID";

const CLIENT_IDENTITY: &str = "nagi-contract-activity-client-v1";
const WORKER_IDENTITY: &str = "nagi-contract-activity-worker-v1";
const WORKFLOW_ID: &str = "nagi-contract-activity-workflow-v1";
const TASK_QUEUE: &str = "nagi-contract-activity-v1";
const TOTAL_STEPS: u32 = 4;
const HEARTBEAT_QUIET_MARGIN: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct HeartbeatCheckpoint {
    next_step: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActivityProgress {
    attempt: u32,
    checkpoint: u32,
    resumed_from_heartbeat: u32,
    heartbeat_seen: bool,
    resume_marker: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CancellationWitness {
    marker: String,
    attempt: u32,
    checkpoint: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct ActivityState {
    latest_attempt: u32,
    latest_checkpoint: u32,
    heartbeat_count: u32,
    resumed_from_heartbeat: u32,
    heartbeat_resumed: bool,
    activity_cancellation_observed: bool,
    workflow_cancellation_acknowledged: bool,
    cleanup_completed: bool,
    completed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RecoveryResult {
    latest_attempt: u32,
    latest_checkpoint: u32,
    heartbeat_resumed: bool,
    activity_cancelled: bool,
    workflow_cancellation_acknowledged: bool,
    cleanup_completed: bool,
}

struct RecoveryActivities;

#[activities]
impl RecoveryActivities {
    /// Run until the workflow cancels this Activity, carrying progress only in
    /// Temporal heartbeats. Every retry starts from
    /// `ActivityContext::heartbeat_details`, never from a local file.
    #[activity]
    async fn long_running(ctx: ActivityContext, total_steps: u32) -> Result<(), ActivityError> {
        let checkpoint = ctx
            .heartbeat_details()
            .deserialize::<HeartbeatCheckpoint>()?
            .unwrap_or(HeartbeatCheckpoint { next_step: 0 });
        let mut next_step = checkpoint.next_step.min(total_steps);
        let attempt = ctx.info().attempt;
        let resumed_from_heartbeat = next_step;

        report_progress(
            &ctx,
            ActivityProgress {
                attempt,
                checkpoint: next_step,
                resumed_from_heartbeat,
                heartbeat_seen: resumed_from_heartbeat != 0,
                resume_marker: true,
            },
        )
        .await;

        loop {
            if ctx.is_cancelled() {
                return Err(ActivityError::cancelled_with_details(CancellationWitness {
                    marker: "nagi-activity-cancelled-v1".to_owned(),
                    attempt,
                    checkpoint: next_step,
                }));
            }

            if next_step < total_steps {
                next_step += 1;
            }
            ctx.record_heartbeat(HeartbeatCheckpoint { next_step })
                .await?;
            report_progress(
                &ctx,
                ActivityProgress {
                    attempt,
                    checkpoint: next_step,
                    resumed_from_heartbeat,
                    heartbeat_seen: true,
                    resume_marker: false,
                },
            )
            .await;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// A cancellation-independent cleanup Activity proves that cancellation
    /// was acknowledged by the SDK and that the workflow can still commit a
    /// deterministic terminal result.
    #[activity]
    async fn cleanup(_ctx: ActivityContext, _input: ()) -> Result<(), ActivityError> {
        Ok(())
    }
}

#[workflow]
#[derive(Default)]
struct ActivityRecoveryWorkflow {
    latest_attempt: u32,
    latest_checkpoint: u32,
    heartbeat_count: u32,
    resumed_from_heartbeat: u32,
    heartbeat_resumed: bool,
    activity_cancellation_observed: bool,
    workflow_cancellation_acknowledged: bool,
    cleanup_completed: bool,
    completed: bool,
}

#[workflow_methods]
impl ActivityRecoveryWorkflow {
    #[run]
    async fn run(
        ctx: &mut WorkflowContext<Self>,
        total_steps: u32,
    ) -> WorkflowResult<RecoveryResult> {
        let activity = ctx
            .execute_activity(
                RecoveryActivities::long_running,
                total_steps,
                ActivityOptions::with_schedule_to_close_timeout(Duration::from_secs(60))
                    .heartbeat_timeout(Duration::from_secs(2))
                    .retry_policy(retry_policy())
                    .do_not_eagerly_execute(true)
                    .cancellation_type(ActivityCancellationType::WaitCancellationCompleted)
                    .build(),
            )
            .await;

        match activity {
            Err(ActivityExecutionError::Cancelled(cancelled)) => {
                // This branch is reached only after Temporal has delivered the
                // cancellation to the Activity and the Activity has returned a
                // cancellation witness. Decode that witness from SDK/History,
                // rather than inferring cancellation from a process exit or a
                // best-effort progress signal.
                let cancellation =
                    cancelled.details::<CancellationWitness>()?.ok_or_else(|| {
                        ApplicationFailure::new("missing Activity cancellation witness")
                    })?;
                if cancellation.marker != "nagi-activity-cancelled-v1"
                    || cancellation.attempt == 0
                    || cancellation.checkpoint == 0
                    || cancellation.checkpoint > TOTAL_STEPS
                {
                    return Err(
                        ApplicationFailure::new("invalid Activity cancellation witness").into(),
                    );
                }
                ctx.state_mut(|state| {
                    state.activity_cancellation_observed = true;
                    state.workflow_cancellation_acknowledged = true;
                });
                // Use an independent token for cleanup so root workflow
                // cancellation does not skip it.
                ctx.execute_activity(
                    RecoveryActivities::cleanup,
                    (),
                    ActivityOptions::with_schedule_to_close_timeout(Duration::from_secs(10))
                        .retry_policy(RetryPolicy::builder().maximum_attempts(1).build())
                        .cancellation_token(WorkflowCancellationToken::new())
                        .build(),
                )
                .await?;
                ctx.state_mut(|state| {
                    state.cleanup_completed = true;
                    state.completed = true;
                });
                let state = ctx.state(|state| ActivityState {
                    latest_attempt: state.latest_attempt,
                    latest_checkpoint: state.latest_checkpoint,
                    heartbeat_count: state.heartbeat_count,
                    resumed_from_heartbeat: state.resumed_from_heartbeat,
                    heartbeat_resumed: state.heartbeat_resumed,
                    activity_cancellation_observed: state.activity_cancellation_observed,
                    workflow_cancellation_acknowledged: state.workflow_cancellation_acknowledged,
                    cleanup_completed: state.cleanup_completed,
                    completed: state.completed,
                });
                Ok(RecoveryResult {
                    latest_attempt: state.latest_attempt,
                    latest_checkpoint: state.latest_checkpoint,
                    heartbeat_resumed: state.heartbeat_resumed,
                    activity_cancelled: state.activity_cancellation_observed,
                    workflow_cancellation_acknowledged: state.workflow_cancellation_acknowledged,
                    cleanup_completed: state.cleanup_completed,
                })
            }
            Ok(()) => {
                Err(ApplicationFailure::new("long-running Activity unexpectedly completed").into())
            }
            Err(error) => Err(error.into()),
        }
    }

    #[signal]
    fn progress(&mut self, _ctx: &mut SyncWorkflowContext<Self>, progress: ActivityProgress) {
        self.latest_attempt = self.latest_attempt.max(progress.attempt);
        self.latest_checkpoint = self.latest_checkpoint.max(progress.checkpoint);
        self.heartbeat_count = self.heartbeat_count.saturating_add(1);
        self.resumed_from_heartbeat = self
            .resumed_from_heartbeat
            .max(progress.resumed_from_heartbeat);
        self.heartbeat_resumed |= progress.resume_marker
            && progress.heartbeat_seen
            && progress.resumed_from_heartbeat != 0
            && progress.attempt > 1;
    }

    #[query]
    fn state(&self, _ctx: &WorkflowContextView) -> ActivityState {
        ActivityState {
            latest_attempt: self.latest_attempt,
            latest_checkpoint: self.latest_checkpoint,
            heartbeat_count: self.heartbeat_count,
            resumed_from_heartbeat: self.resumed_from_heartbeat,
            heartbeat_resumed: self.heartbeat_resumed,
            activity_cancellation_observed: self.activity_cancellation_observed,
            workflow_cancellation_acknowledged: self.workflow_cancellation_acknowledged,
            cleanup_completed: self.cleanup_completed,
            completed: self.completed,
        }
    }
}

fn retry_policy() -> RetryPolicy {
    RetryPolicy::builder()
        .initial_interval(Duration::from_secs(1))
        .maximum_interval(Duration::from_secs(2))
        .maximum_attempts(4)
        .build()
}

fn bounded_rpc_options() -> RpcOptions {
    RpcOptions::builder()
        .timeout(Duration::from_millis(500))
        .retry_options(RetryOptions::no_retries())
        .build()
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be supplied by the contract task"))
}

fn valid_run_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn required_run_id() -> String {
    let run_id = required_env(RUN_ID_ENV);
    assert!(valid_run_id(&run_id), "contract supplied an invalid run ID");
    run_id
}

fn parse_loopback_address(value: &str) -> Option<Url> {
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

async fn report_progress(ctx: &ActivityContext, progress: ActivityProgress) {
    // Progress is only a bounded witness for the driver. Once the synthetic
    // checkpoint reaches its fixed total, avoid sending duplicate signals in
    // the long-running tail; heartbeat delivery remains the durability gate.
    if progress.checkpoint >= TOTAL_STEPS && !progress.resume_marker {
        return;
    }
    let Some(handle) = ctx.workflow_handle::<ActivityRecoveryWorkflow>() else {
        return;
    };
    let request_id = format!(
        "activity-progress-a{}-c{}-x{}",
        progress.attempt, progress.checkpoint, progress.resume_marker as u8
    );
    let options = WorkflowSignalOptions::builder()
        .request_id(request_id)
        .rpc_options(bounded_rpc_options())
        .build();
    // A progress signal is only an observable witness. Heartbeats remain the
    // recovery source of truth, so a transient signal failure never changes
    // Activity completion or retry behavior.
    let _ = tokio::time::timeout(
        Duration::from_millis(200),
        handle.signal(ActivityRecoveryWorkflow::progress, progress, options),
    )
    .await;
}

async fn connect_client() -> (Client, String) {
    let address = parse_loopback_address(&required_env(ADDRESS_ENV))
        .expect("contract task must supply the exact IPv4 loopback URL");
    let namespace = required_env(NAMESPACE_ENV);
    assert!(!namespace.is_empty());
    let connection_options = ConnectionOptions::new(address)
        .identity(CLIENT_IDENTITY)
        .connect_timeout(Duration::from_secs(5))
        .build();
    let connection = Connection::connect(connection_options)
        .await
        .expect("connect to the local Temporal sidecar");
    let client = Client::new(connection, ClientOptions::new(namespace.clone()).build())
        .expect("create Temporal client");
    (client, namespace)
}

fn exact_workflow_handle(
    client: &Client,
    namespace: &str,
    run_id: String,
) -> WorkflowHandle<Client, activity_recovery_workflow::Run> {
    WorkflowHandle::new(
        client.clone(),
        WorkflowExecutionInfo {
            namespace: namespace.to_owned(),
            workflow_id: WORKFLOW_ID.to_owned(),
            run_id: Some(run_id.clone()),
            first_execution_run_id: Some(run_id),
        },
    )
}

async fn run_worker() {
    let (client, _) = connect_client().await;
    let runtime = Runtime::new_assume_tokio(Default::default()).expect("create SDK runtime");
    let worker_options = WorkerOptions::new(TASK_QUEUE)
        .client_identity_override(WORKER_IDENTITY.to_owned())
        .max_cached_workflows(0)
        .max_heartbeat_throttle_interval(Duration::from_millis(250))
        .default_heartbeat_throttle_interval(Duration::from_millis(250))
        .register_workflow::<ActivityRecoveryWorkflow>()
        .expect("register synthetic workflow")
        .register_activities(RecoveryActivities)
        .build();
    let mut worker = Worker::new(&runtime, client, worker_options).expect("create activity worker");
    worker.run().await.expect("activity worker should run");
}

async fn assert_exact_history(
    handle: &WorkflowHandle<Client, activity_recovery_workflow::Run>,
    expected_run_id: &str,
    require_cancellation: bool,
) {
    let history = tokio::time::timeout(
        Duration::from_secs(5),
        handle.fetch_history(
            WorkflowFetchHistoryOptions::builder()
                .rpc_options(bounded_rpc_options())
                .build(),
        ),
    )
    .await
    .expect("history fetch deadline")
    .expect("fetch exact workflow history");
    assert_eq!(history.workflow_id(), Some(WORKFLOW_ID));
    let events = history.events();
    assert!(!events.is_empty());
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.event_id, (index + 1) as i64);
        assert!(!matches!(
            event.attributes,
            Some(Attributes::WorkflowExecutionContinuedAsNewEventAttributes(
                _
            ))
        ));
    }
    match events.first().and_then(|event| event.attributes.as_ref()) {
        Some(Attributes::WorkflowExecutionStartedEventAttributes(
            WorkflowExecutionStartedEventAttributes {
                workflow_id,
                first_execution_run_id,
                original_execution_run_id,
                ..
            },
        )) => {
            assert_eq!(workflow_id, WORKFLOW_ID);
            assert_eq!(first_execution_run_id, expected_run_id);
            assert_eq!(original_execution_run_id, expected_run_id);
        }
        _ => panic!("exact history must begin with WorkflowExecutionStarted"),
    }
    if require_cancellation {
        assert!(events.iter().any(|event| matches!(
            event.attributes,
            Some(Attributes::ActivityTaskCanceledEventAttributes(_))
        )));
    }
    assert_eq!(handle.run_id(), Some(expected_run_id));
}

async fn query_state(
    handle: &temporalio_client::WorkflowHandle<Client, activity_recovery_workflow::Run>,
) -> Result<ActivityState, temporalio_client::errors::WorkflowQueryError> {
    handle
        .query(
            ActivityRecoveryWorkflow::state,
            (),
            WorkflowQueryOptions::builder()
                .rpc_options(bounded_rpc_options())
                .build(),
        )
        .await
}

async fn wait_for_state(
    handle: &temporalio_client::WorkflowHandle<Client, activity_recovery_workflow::Run>,
    predicate: impl Fn(&ActivityState) -> bool,
) -> ActivityState {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Ok(Ok(actual)) =
            tokio::time::timeout(Duration::from_millis(750), query_state(handle)).await
            && predicate(&actual)
        {
            return actual;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("synthetic Activity workflow did not reach the expected state");
}

async fn run_driver(phase: &str) {
    let (client, namespace) = connect_client().await;
    let (handle, expected_run_id) = match phase {
        "start" => {
            let handle = client
                .start_workflow(
                    ActivityRecoveryWorkflow::run,
                    TOTAL_STEPS,
                    WorkflowStartOptions::new(TASK_QUEUE, WORKFLOW_ID)
                        .rpc_options(bounded_rpc_options())
                        .build(),
                )
                .await
                .expect("start the synthetic Activity workflow");
            let run_id = handle
                .run_id()
                .expect("Temporal must return the started run ID")
                .to_owned();
            assert!(valid_run_id(&run_id));
            // This marker is captured only by the private harness output and
            // becomes the exact run binding for every later process.
            println!("activity-run-id={run_id}");
            (handle, run_id)
        }
        "after-worker" | "after-server" => {
            let run_id = required_run_id();
            (
                exact_workflow_handle(&client, &namespace, run_id.clone()),
                run_id,
            )
        }
        _ => panic!("unsupported private Activity contract phase"),
    };
    assert_eq!(handle.run_id(), Some(expected_run_id.as_str()));

    match phase {
        "start" => {
            wait_for_state(&handle, |state| {
                state.latest_attempt == 1
                    && state.latest_checkpoint >= 1
                    && state.heartbeat_count > 0
            })
            .await;
            assert_exact_history(&handle, &expected_run_id, false).await;
            // Core queues heartbeats before sending them to the server. Leave
            // a quiet interval longer than the throttle before the harness
            // force-kills Worker A, so retry details are server-acknowledged.
            tokio::time::sleep(HEARTBEAT_QUIET_MARGIN).await;
        }
        "after-worker" => {
            wait_for_state(&handle, |state| {
                state.latest_attempt >= 2
                    && state.heartbeat_resumed
                    && state.resumed_from_heartbeat >= 1
            })
            .await;
            assert_exact_history(&handle, &expected_run_id, false).await;
            tokio::time::sleep(HEARTBEAT_QUIET_MARGIN).await;
        }
        "after-server" => {
            wait_for_state(&handle, |state| {
                state.latest_attempt >= 3
                    && state.heartbeat_resumed
                    && state.resumed_from_heartbeat >= 1
            })
            .await;
            assert_exact_history(&handle, &expected_run_id, false).await;

            handle
                .cancel(
                    WorkflowCancelOptions::builder()
                        .rpc_options(bounded_rpc_options())
                        .build(),
                )
                .await
                .expect("request workflow cancellation");
            let state = wait_for_state(&handle, |state| {
                state.activity_cancellation_observed
                    && state.workflow_cancellation_acknowledged
                    && state.cleanup_completed
                    && state.completed
            })
            .await;
            assert!(state.activity_cancellation_observed);
            assert!(state.workflow_cancellation_acknowledged);
            assert!(state.cleanup_completed);

            let result = tokio::time::timeout(
                Duration::from_secs(15),
                handle.get_result(
                    WorkflowGetResultOptions::builder()
                        .follow_runs(false)
                        .rpc_options(bounded_rpc_options())
                        .build(),
                ),
            )
            .await
            .expect("workflow result deadline")
            .expect("completed Activity recovery workflow result");
            assert_eq!(result.latest_attempt, state.latest_attempt);
            assert!(result.heartbeat_resumed);
            assert!(result.activity_cancelled);
            assert!(result.workflow_cancellation_acknowledged);
            assert!(result.cleanup_completed);
            assert_exact_history(&handle, &expected_run_id, true).await;
        }
        _ => unreachable!(),
    }

    // Keep the namespace binding live for the whole driver operation and avoid
    // accidentally accepting a caller-selected or empty namespace.
    assert!(!namespace.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn temporal_activity_contract_exercises_recovery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            if required_env(ROLE_ENV) == "worker" {
                run_worker().await;
            } else {
                let phase = required_env(PHASE_ENV);
                run_driver(&phase).await;
            }
        })
        .await;
}

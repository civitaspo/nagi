//! Live, credential-free checks for the Temporal message contract.
//!
//! This test is deliberately behind `temporal-message-contract` and the
//! macOS-only `contract:temporal-messages` task. The task supplies a loopback
//! address for the pinned local Temporal CLI sidecar; no provider or
//! application credential is involved.

#![cfg(target_os = "macos")]

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use temporalio_client::{
    Client, ClientInterceptor, ClientOptions, Connection, ConnectionOptions, Next,
    PollWorkflowUpdateInput, PollWorkflowUpdateOutput, RetryOptions, RpcOptions, Url,
    WorkflowExecuteUpdateOptions, WorkflowGetResultOptions, WorkflowQueryOptions,
    WorkflowSignalOptions, WorkflowStartOptions, WorkflowStartSignal,
    errors::{WorkflowStartError, WorkflowUpdateError},
};
use temporalio_common::{
    data_converters::{DataConverter, SerializationContextData},
    protos::temporal::api::{
        common::v1::Payloads,
        enums::v1::{WorkflowIdConflictPolicy, WorkflowIdReusePolicy},
    },
};
use temporalio_sdk::{
    Runtime, SyncWorkflowContext, Worker, WorkerOptions, WorkflowContext, WorkflowContextView,
    WorkflowResult,
    workflows::{workflow, workflow_methods},
};

const ADDRESS_ENV: &str = "NAGI_TEMPORAL_MESSAGE_ADDRESS";
const NAMESPACE_ENV: &str = "NAGI_TEMPORAL_MESSAGE_NAMESPACE";
const CLIENT_IDENTITY: &str = "nagi-contract-message-client-v1";
const WORKER_IDENTITY: &str = "nagi-contract-message-worker-v1";
const WORKFLOW_ID: &str = "nagi-contract-message-workflow-v1";
const TASK_QUEUE: &str = "nagi-contract-message-v1";
const START_SIGNAL_ID: &str = "logical-signal-v1";
const START_SIGNAL_DIGEST: &str = "digest-a";
const CONFLICTING_SIGNAL_DELTA: i32 = 99;
const UPDATE_ID: &str = "logical-update-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LogicalSignal {
    logical_id: String,
    digest: String,
    delta: i32,
}

type MessageState = (i32, usize, usize, u32, u32, bool);

#[workflow]
#[derive(Default)]
struct MessageWorkflow {
    applied_signals: Vec<LogicalSignal>,
    rejected_signal_conflicts: usize,
    signal_deliveries: u32,
    total: i32,
    update_invocations: u32,
    finished: bool,
}

#[workflow_methods]
impl MessageWorkflow {
    #[run]
    async fn run(ctx: &mut WorkflowContext<Self>, _: ()) -> WorkflowResult<i32> {
        ctx.wait_condition(|state| state.finished).await?;
        Ok(ctx.state(|state| state.total))
    }

    /// Signals are application-idempotent: a logical signal ID and its full
    /// canonical payload are applied once even when separate transport
    /// requests carry the same logical message. A conflicting payload is
    /// ignored (fail closed).
    #[signal]
    fn apply_signal(&mut self, _ctx: &mut SyncWorkflowContext<Self>, signal: LogicalSignal) {
        self.signal_deliveries += 1;
        let Some(applied) = self
            .applied_signals
            .iter()
            .find(|applied| applied.logical_id == signal.logical_id)
        else {
            self.total += signal.delta;
            self.applied_signals.push(signal);
            return;
        };
        if applied != &signal {
            // A conflicting full payload is an observable application-level
            // rejection, but it never changes the business total or replaces
            // the first accepted payload.
            self.rejected_signal_conflicts += 1;
        }
    }

    #[query]
    fn state(&self, _ctx: &WorkflowContextView) -> MessageState {
        (
            self.total,
            self.applied_signals.len(),
            self.rejected_signal_conflicts,
            self.signal_deliveries,
            self.update_invocations,
            self.finished,
        )
    }

    #[update_validator(complete)]
    fn validate_complete(
        &self,
        _ctx: &WorkflowContextView,
        requested_total: &i32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if *requested_total < 0 {
            return Err("requested total must be non-negative".into());
        }
        if self.finished {
            return Err("workflow is already finished".into());
        }
        Ok(())
    }

    #[update]
    fn complete(&mut self, _ctx: &mut SyncWorkflowContext<Self>, requested_total: i32) -> i32 {
        let previous_total = self.total;
        self.total = requested_total;
        self.update_invocations += 1;
        previous_total
    }

    #[signal]
    fn finish(&mut self, _ctx: &mut SyncWorkflowContext<Self>) {
        self.finished = true;
    }
}

/// Inject one error after the server has completed an Update. This models a
/// lost client response without changing the request sent to Temporal.
struct ResponseLossOnce {
    fail_once: AtomicBool,
    observed_update_ids: Mutex<Vec<String>>,
}

impl ResponseLossOnce {
    fn new() -> Self {
        Self {
            fail_once: AtomicBool::new(true),
            observed_update_ids: Mutex::new(Vec::new()),
        }
    }

    fn update_ids(&self) -> Vec<String> {
        self.observed_update_ids
            .lock()
            .expect("response-loss observer lock")
            .clone()
    }
}

impl ClientInterceptor for ResponseLossOnce {
    fn poll_workflow_update<'a>(
        &'a self,
        input: PollWorkflowUpdateInput,
        next: Next<
            'a,
            PollWorkflowUpdateInput,
            BoxFuture<'a, Result<PollWorkflowUpdateOutput, WorkflowUpdateError>>,
        >,
    ) -> BoxFuture<'a, Result<PollWorkflowUpdateOutput, WorkflowUpdateError>> {
        let update_id = input.update_id.clone();
        Box::pin(async move {
            let output = next.run(input).await?;
            let observer = self;
            observer
                .observed_update_ids
                .lock()
                .expect("response-loss observer lock")
                .push(update_id);
            if observer.fail_once.swap(false, Ordering::AcqRel) {
                return Err(WorkflowUpdateError::Other(
                    std::io::Error::other("synthetic response loss after completed update").into(),
                ));
            }
            Ok(output)
        })
    }
}

fn bounded_rpc_options() -> RpcOptions {
    RpcOptions::builder()
        .timeout(Duration::from_secs(5))
        .retry_options(RetryOptions::no_retries())
        .build()
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be supplied by the contract task"))
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

async fn make_start_signal(data_converter: &DataConverter) -> WorkflowStartSignal {
    let signal = LogicalSignal {
        logical_id: START_SIGNAL_ID.to_owned(),
        digest: START_SIGNAL_DIGEST.to_owned(),
        delta: 1,
    };
    let payloads = data_converter
        .to_payloads(&SerializationContextData::Workflow, &signal)
        .await
        .expect("synthetic signal payload encoding");
    WorkflowStartSignal::new("apply_signal")
        .input(Payloads { payloads })
        .build()
}

async fn query_state(
    handle: &temporalio_client::WorkflowHandle<Client, message_workflow::Run>,
) -> Result<MessageState, temporalio_client::errors::WorkflowQueryError> {
    handle
        .query(
            MessageWorkflow::state,
            (),
            WorkflowQueryOptions::builder()
                .rpc_options(bounded_rpc_options())
                .build(),
        )
        .await
}

async fn wait_for_state(
    handle: &temporalio_client::WorkflowHandle<Client, message_workflow::Run>,
    expected: MessageState,
) {
    for _ in 0..80 {
        if let Ok(actual) = query_state(handle).await
            && actual == expected
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("synthetic workflow did not reach the expected queried state")
}

fn update_options(update_id: &str) -> WorkflowExecuteUpdateOptions {
    WorkflowExecuteUpdateOptions::builder()
        .update_id(update_id.to_owned())
        .rpc_options(bounded_rpc_options())
        .build()
}

#[tokio::test(flavor = "current_thread")]
async fn temporal_message_contract_exercises_messages() {
    tokio::task::LocalSet::new()
        .run_until(async {
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

            let base_options = ClientOptions::new(namespace.clone()).build();
            let start_signal = make_start_signal(&base_options.data_converter).await;
            let client =
                Client::new(connection.clone(), base_options).expect("create Temporal client");

            let runtime =
                Runtime::new_assume_tokio(Default::default()).expect("create SDK runtime");
            let worker_options = WorkerOptions::new(TASK_QUEUE)
                .client_identity_override(WORKER_IDENTITY.to_owned())
                .register_workflow::<MessageWorkflow>()
                .expect("register synthetic workflow")
                .build();
            let mut worker = Worker::new(&runtime, client.clone(), worker_options)
                .expect("create synthetic workflow worker");
            let shutdown_worker = worker.shutdown_handle();
            let worker_task = tokio::task::spawn_local(async move { worker.run().await });

            let start_options = WorkflowStartOptions::new(TASK_QUEUE, WORKFLOW_ID)
                .id_conflict_policy(WorkflowIdConflictPolicy::UseExisting)
                .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
                .start_signal(start_signal.clone())
                .rpc_options(bounded_rpc_options())
                .build();
            let handle = client
                .start_workflow(MessageWorkflow::run, (), start_options)
                .await
                .expect("Signal-With-Start should start the synthetic workflow");

            // The start signal is delivered before the first workflow task. A query
            // proves both delivery and the workflow-level logical-ID idempotency key.
            wait_for_state(&handle, (1, 1, 0, 1, 0, false)).await;

            // temporalio-client 0.7.0 generates the Signal-With-Start transport
            // request ID internally, so this resend deliberately uses the same
            // application logical ID and full payload instead. UseExisting makes
            // the running execution explicit, and the delivery counter proves
            // the second Signal-With-Start was observed before application
            // deduplication.
            let resend_handle = client
                .start_workflow(
                    MessageWorkflow::run,
                    (),
                    WorkflowStartOptions::new(TASK_QUEUE, WORKFLOW_ID)
                        .id_conflict_policy(WorkflowIdConflictPolicy::UseExisting)
                        .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
                        .start_signal(start_signal.clone())
                        .rpc_options(bounded_rpc_options())
                        .build(),
                )
                .await
                .expect("Signal-With-Start resend should use the running workflow");
            assert_eq!(resend_handle.run_id(), handle.run_id());
            wait_for_state(&handle, (1, 1, 0, 2, 0, false)).await;

            // Use distinct transport request IDs while retaining one logical ID. The
            // application, rather than transport retry behavior, owns deduplication.
            for request_id in ["synthetic-signal-request-a", "synthetic-signal-request-b"] {
                let signal_options = WorkflowSignalOptions::builder()
                    .request_id(request_id.to_owned())
                    .rpc_options(bounded_rpc_options())
                    .build();
                handle
                    .signal(
                        MessageWorkflow::apply_signal,
                        LogicalSignal {
                            logical_id: START_SIGNAL_ID.to_owned(),
                            digest: START_SIGNAL_DIGEST.to_owned(),
                            delta: 1,
                        },
                        signal_options,
                    )
                    .await
                    .expect("duplicate logical signal should be accepted");
            }
            wait_for_state(&handle, (1, 1, 0, 4, 0, false)).await;

            let mismatched_signal_options = WorkflowSignalOptions::builder()
                .request_id("synthetic-signal-request-mismatch".to_owned())
                .rpc_options(bounded_rpc_options())
                .build();
            handle
                .signal(
                    MessageWorkflow::apply_signal,
                    LogicalSignal {
                        logical_id: START_SIGNAL_ID.to_owned(),
                        digest: START_SIGNAL_DIGEST.to_owned(),
                        delta: CONFLICTING_SIGNAL_DELTA,
                    },
                    mismatched_signal_options,
                )
                .await
                .expect("conflicting logical signal should be accepted then ignored");
            // A changed delta with the same declared digest cannot mutate the
            // already-applied logical signal.
            wait_for_state(&handle, (1, 1, 1, 5, 0, false)).await;

            // The validator rejects before the handler mutates state.
            let invalid_update = handle
                .execute_update(
                    MessageWorkflow::complete,
                    -1,
                    update_options("invalid-update-v1"),
                )
                .await;
            assert!(
                matches!(invalid_update, Err(WorkflowUpdateError::Failed(_))),
                "negative Update must be rejected by its validator"
            );
            wait_for_state(&handle, (1, 1, 1, 5, 0, false)).await;

            let response_loss = Arc::new(ResponseLossOnce::new());
            let response_options = ClientOptions::new(namespace)
                .client_interceptors(vec![response_loss.clone()])
                .build();
            let response_client =
                Client::new(connection, response_options).expect("create response-loss client");
            let response_handle =
                response_client.get_workflow_handle::<MessageWorkflow>(WORKFLOW_ID);

            // The interceptor waits for the server to complete the Update and then
            // hides the response. Query state first to resolve the ambiguity; no new
            // mutation ID is minted and no blind mutation retry is attempted.
            let lost_response = response_handle
                .execute_update(MessageWorkflow::complete, 7, update_options(UPDATE_ID))
                .await;
            assert!(
                matches!(lost_response, Err(WorkflowUpdateError::Other(_))),
                "the first Update result must model a lost client response"
            );
            wait_for_state(&handle, (7, 1, 1, 5, 1, false)).await;

            // A retry is allowed only after state inspection and carries the exact same
            // stable Update ID. Temporal returns the original handler result. The
            // interceptor records the ID on both completed-outcome polls below.
            let recovered_result = response_handle
                .execute_update(MessageWorkflow::complete, 7, update_options(UPDATE_ID))
                .await
                .expect("same Update ID should recover the committed result");
            assert_eq!(recovered_result, 1);
            assert_eq!(
                query_state(&handle).await.expect("stable post-retry query"),
                (7, 1, 1, 5, 1, false)
            );
            assert_eq!(
                response_loss.update_ids(),
                vec![UPDATE_ID.to_owned(), UPDATE_ID.to_owned()]
            );

            handle
                .signal(
                    MessageWorkflow::finish,
                    (),
                    WorkflowSignalOptions::builder()
                        .request_id("synthetic-finish-request-v1".to_owned())
                        .rpc_options(bounded_rpc_options())
                        .build(),
                )
                .await
                .expect("finish signal should close the synthetic workflow");
            wait_for_state(&handle, (7, 1, 1, 5, 1, true)).await;

            let result = handle
                .get_result(
                    WorkflowGetResultOptions::builder()
                        .follow_runs(true)
                        .rpc_options(bounded_rpc_options())
                        .build(),
                )
                .await
                .expect("completed workflow result");
            assert_eq!(result, 7);

            // A closed workflow ID cannot be reused, even when a Signal-With-Start
            // request asks to use an existing running execution. In temporalio-client
            // 0.7.0, the ordinary StartWorkflowExecution path maps AlreadyExists to
            // WorkflowStartError::AlreadyStarted, but the Signal-With-Start path
            // preserves the gRPC status as WorkflowStartError::Rpc. Match that
            // specific status to prove that no new run or start-signal mutation was
            // created after completion.
            let closed_retry = client
                .start_workflow(
                    MessageWorkflow::run,
                    (),
                    WorkflowStartOptions::new(TASK_QUEUE, WORKFLOW_ID)
                        .id_conflict_policy(WorkflowIdConflictPolicy::UseExisting)
                        .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
                        .start_signal(start_signal)
                        .rpc_options(bounded_rpc_options())
                        .build(),
                )
                .await;
            assert!(
                matches!(
                    closed_retry,
                    Err(WorkflowStartError::Rpc(status))
                        if status.code() == temporalio_client::tonic::Code::AlreadyExists
                ),
                "closed Signal-With-Start retry must return Rpc(AlreadyExists)"
            );
            assert_eq!(
                query_state(&handle)
                    .await
                    .expect("closed retry must leave state queryable"),
                (7, 1, 1, 5, 1, true)
            );

            shutdown_worker();
            let worker_result = tokio::time::timeout(Duration::from_secs(10), worker_task)
                .await
                .expect("worker shutdown deadline")
                .expect("worker task join");
            worker_result.expect("worker should shut down cleanly");
        })
        .await;
}

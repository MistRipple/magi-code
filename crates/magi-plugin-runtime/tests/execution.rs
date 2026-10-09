use magi_plugin_runtime::{
    CapabilityHandler, CapabilityRequest, ExecutionError, ExecutionErrorCode, Invocation,
    InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct RecordingHandler {
    identities: Mutex<Vec<InvocationIdentity>>,
    cancellations: Mutex<Vec<RunCancellation>>,
}

impl CapabilityHandler for RecordingHandler {
    fn call(
        &self,
        request: CapabilityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ExecutionError>> + Send + '_>> {
        Box::pin(async move {
            self.cancellations
                .lock()
                .unwrap()
                .push(request.cancellation.clone());
            self.identities.lock().unwrap().push(request.identity);
            match request.operation.as_str() {
                "test.echo" => Ok(request.arguments),
                "test.wait" => {
                    request.cancellation.cancelled().await;
                    Err(ExecutionError::new(
                        ExecutionErrorCode::Cancelled,
                        "cancelled",
                    ))
                }
                _ => Err(ExecutionError::new(
                    ExecutionErrorCode::CapabilityRejected,
                    "not granted",
                )),
            }
        })
    }
}

fn host() -> PluginHost {
    PluginHost::new(
        env!("CARGO_BIN_EXE_magi-plugin-worker").into(),
        magi_process::ManagedProcessGroup::new(),
    )
}

fn invocation(source: &str) -> Invocation {
    Invocation {
        identity: InvocationIdentity {
            plugin_id: "test.plugin".into(),
            package_digest: "test-digest".into(),
            instance_id: "host-instance".into(),
            invocation_id: "host-invocation".into(),
            workspace_id: Some("host-workspace".into()),
            run_id: Some("host-run".into()),
            attempt_id: Some("host-attempt".into()),
        },
        source: source.into(),
        input: json!({"count": 12}),
        limits: RuntimeLimits::default(),
    }
}

#[test]
fn distribution_probe_runs_copied_worker_without_developer_environment() {
    let directory = tempfile::Builder::new()
        .prefix("Magi plugin resources ")
        .tempdir()
        .unwrap();
    let name = if cfg!(windows) {
        "magi-plugin-worker.exe"
    } else {
        "magi-plugin-worker"
    };
    let worker = directory.path().join(name);
    std::fs::copy(env!("CARGO_BIN_EXE_magi-plugin-worker"), &worker).unwrap();
    let output = std::process::Command::new(&worker)
        .arg("--preflight")
        .env_clear()
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"ok": true, "workerVersion": env!("CARGO_PKG_VERSION")})
    );
    assert!(
        !std::process::Command::new(&worker)
            .arg("--unknown")
            .env_clear()
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn esm_and_async_results_execute_without_machine_developer_tools() {
    let result = host()
        .invoke(
            invocation(
                "export default async (input) => ({value: await Promise.resolve(input.count * 2)})",
            ),
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(result, json!({"value": 24}));
}

#[tokio::test]
async fn worker_has_no_environment_files_network_process_or_module_loader() {
    let result = host()
        .invoke(
            invocation(
                r#"export default () => ({
        process: typeof process, require: typeof require, fetch: typeof fetch,
        fs: typeof fs, std: typeof std, os: typeof os, exit: typeof exit,
        sdkGlobal: typeof magi, console: typeof console
    })"#,
            ),
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap();
    assert!(
        result
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == "undefined"),
        "{result}"
    );
    let error = host()
        .invoke(
            invocation(
                "import fs from 'node:fs'; export default () => fs.readFileSync('/etc/passwd')",
            ),
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::ScriptFailed);
}

#[tokio::test]
async fn capabilities_use_host_identity_and_cannot_be_self_authorized() {
    let handler = RecordingHandler::default();
    let result = host().invoke(invocation(r#"export default (input, sdk) => {
        let rejected = false;
        try { sdk.call('file.read', {workspace_id: 'forged-workspace'}); } catch (_) { rejected = true; }
        return {rejected, value: sdk.call('test.echo', input)};
    }"#), &handler, &RunCancellation::default()).await.unwrap();
    assert_eq!(result, json!({"rejected": true, "value": {"count": 12}}));
    let identities = handler.identities.lock().unwrap();
    assert_eq!(identities.len(), 2);
    assert!(
        identities
            .iter()
            .all(|i| i.workspace_id.as_deref() == Some("host-workspace")
                && i.invocation_id == "host-invocation")
    );
}

#[tokio::test]
async fn runaway_script_times_out_and_does_not_block_next_invocation() {
    let host = host();
    let mut request = invocation("export default () => { while (true) {} }");
    request.limits.timeout_ms = 100;
    let began = std::time::Instant::now();
    let error = host
        .invoke(
            request,
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::TimedOut);
    assert!(began.elapsed() < Duration::from_secs(4));
    let result = host
        .invoke(
            invocation("export default () => 'still available'"),
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(result, "still available");
}

#[tokio::test]
async fn javascript_memory_and_output_are_bounded() {
    let mut request = invocation(
        "export default () => { const arrays = []; while (true) arrays.push(new Uint8Array(1024 * 1024)); }",
    );
    request.limits.memory_bytes = 4 * 1024 * 1024;
    let error = host()
        .invoke(
            request,
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::ResourceLimit);
    let mut request = invocation("export default () => 'x'.repeat(4000)");
    request.limits.max_frame_bytes = 2048;
    let error = host()
        .invoke(
            request,
            &RecordingHandler::default(),
            &RunCancellation::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::ResourceLimit);
}

#[tokio::test]
async fn cancellation_covers_script_and_pending_capability() {
    for source in [
        "export default () => { while (true) {} }",
        "export default (_, sdk) => sdk.call('test.wait', {})",
    ] {
        let token = RunCancellation::default();
        let host = host();
        let handler = RecordingHandler::default();
        let execute = host.invoke(invocation(source), &handler, &token);
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        };
        let (result, ()) = tokio::join!(execute, cancel);
        assert_eq!(result.unwrap_err().code, ExecutionErrorCode::Cancelled);
    }
}

#[tokio::test]
async fn timeout_covers_pending_host_capability_and_pre_cancel_does_not_execute() {
    let handler = RecordingHandler::default();
    let mut request = invocation("export default (_, sdk) => sdk.call('test.wait', {})");
    request.limits.timeout_ms = 200;
    let error = host()
        .invoke(request.clone(), &handler, &RunCancellation::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::TimedOut);
    let token = RunCancellation::default();
    token.cancel();
    let before = handler.identities.lock().unwrap().len();
    let error = host().invoke(request, &handler, &token).await.unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::Cancelled);
    assert_eq!(handler.identities.lock().unwrap().len(), before);
}

#[tokio::test]
async fn concurrent_invocations_do_not_share_javascript_state_or_cancellation() {
    let host = Arc::new(host());
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let host = host.clone();
        tasks.push(tokio::spawn(async move {
            host.invoke(invocation("globalThis.counter = (globalThis.counter ?? 0) + 1; export default () => counter"), &RecordingHandler::default(), &RunCancellation::default()).await.unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), 1);
    }
}

#[tokio::test]
async fn dropped_invocation_revokes_pending_capability_context() {
    let host = Arc::new(host());
    let handler = Arc::new(RecordingHandler::default());
    let task = {
        let host = host.clone();
        let handler = handler.clone();
        tokio::spawn(async move {
            host.invoke(
                invocation("export default (_, sdk) => sdk.call('test.wait', {})"),
                handler.as_ref(),
                &RunCancellation::default(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !handler.cancellations.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let token = handler.cancellations.lock().unwrap()[0].clone();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(token.is_cancelled());
    assert_eq!(
        host.invoke(
            invocation("export default () => 1"),
            &RecordingHandler::default(),
            &RunCancellation::default()
        )
        .await
        .unwrap(),
        1
    );
}

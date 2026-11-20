//! Shared Cordis process control. No package installation or enabled-state ownership;
//! production cutover is owned by daemon's runtime coordinator.

use crate::{
    frame::{read_frame, write_frame},
    peer::{BoxFuture, Incoming, Peer, PeerLimits},
    protocol::{ErrorKind, Message, RpcError, response_ok},
    transport,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use magi_process::ManagedProcessGroup;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Notify, watch};

const DEADLINE: Duration = Duration::from_secs(15);

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub struct SharedLaunchConfig {
    pub node: PathBuf,
    /// The staged native runtime/dsh/entry.mjs, including its locked dependencies.
    pub runtime_entry: PathBuf,
    pub endpoint_dir: PathBuf,
    pub host_generation: u64,
    pub incoming: Arc<dyn Incoming>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activation {
    pub plugin_id: String,
    pub instance_id: String,
    pub activation_generation: u64,
    pub version: String,
    /// Immutable original npm entry resolved by the package owner.
    pub entry: PathBuf,
    pub config: Value,
}

/// Admission of an extracted original package in the dependency-locked loading project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageInspection {
    pub project_directory: PathBuf,
    pub directory: PathBuf,
    pub name: String,
    pub version: String,
    pub manifest_sha256: String,
}

struct ProcessOwner {
    stop: Arc<Notify>,
    exited: watch::Receiver<Option<ProcessExit>>,
}

#[derive(Clone, Copy, Debug)]
enum ProcessExit {
    Child(std::process::ExitStatus),
    Owner,
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

struct Handler {
    delegate: Arc<dyn Incoming>,
    generation: u64,
    ready: watch::Sender<bool>,
}

impl Incoming for Handler {
    fn request(&self, method: String, params: Value) -> BoxFuture<Result<Value, RpcError>> {
        self.delegate.request(method, params)
    }

    fn notification(&self, method: String, params: Value) {
        if params["hostGeneration"].as_u64() != Some(self.generation) {
            return;
        }
        if method == "host.ready" {
            self.ready.send_replace(true);
        } else {
            self.delegate.notification(method, params);
        }
    }
}

pub struct SharedHost {
    peer: Peer,
    owner: ProcessOwner,
    generation: u64,
    web_route_proof: String,
}

/// Loopback target for an admitted daemon gateway request. Never expose the
/// address or authorization headers in a renderer response or redirect.
pub struct PrivateWebRouteEndpoint {
    address: SocketAddrV4,
    host_generation: u64,
    proof: String,
    peer: Peer,
}

impl PrivateWebRouteEndpoint {
    pub fn address(&self) -> SocketAddrV4 {
        self.address
    }

    pub fn is_closed(&self) -> bool {
        self.peer.is_closed()
    }

    pub fn headers(
        &self,
        host_generation: u64,
        instance_id: &str,
        activation_generation: u64,
        call_epoch: u64,
    ) -> Result<[(&'static str, String); 4], RpcError> {
        if host_generation != self.host_generation {
            return Err(RpcError::new(
                ErrorKind::StaleGeneration,
                "Plugin route belongs to another host",
            ));
        }
        if self.peer.is_closed() {
            return Err(RpcError::new(
                ErrorKind::HostGone,
                "Plugin route host has stopped",
            ));
        }
        if instance_id.is_empty()
            || instance_id.len() > 4096
            || activation_generation == 0
            || activation_generation > 9_007_199_254_740_991
            || call_epoch == 0
            || call_epoch > 9_007_199_254_740_991
        {
            return Err(RpcError::new(
                ErrorKind::InvalidParams,
                "Invalid plugin route activation identity",
            ));
        }
        Ok([
            ("x-magi-route-proof", self.proof.clone()),
            ("x-magi-route-instance", URL_SAFE_NO_PAD.encode(instance_id)),
            ("x-magi-route-generation", activation_generation.to_string()),
            ("x-magi-route-epoch", call_epoch.to_string()),
        ])
    }

    pub async fn closed(&self) {
        self.peer.closed().await;
    }
}

/// An abandoned mutating control call cannot leave a late activation in the shared tree.
struct ControlGuard {
    peer: Peer,
    stop: Arc<Notify>,
    settled: bool,
}

impl Drop for ControlGuard {
    fn drop(&mut self) {
        if !self.settled {
            self.peer.close();
            self.stop.notify_one();
        }
    }
}

fn internal(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(ErrorKind::Internal, error.to_string())
}

async fn exited(mut state: watch::Receiver<Option<ProcessExit>>) -> Option<ProcessExit> {
    loop {
        if let Some(exit) = *state.borrow_and_update() {
            return Some(exit);
        }
        state.changed().await.ok()?;
    }
}

impl SharedHost {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn notify_service(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.peer.notify(method, params)
    }

    /// Observe transport loss without retaining ownership of the process. A supervisor
    /// can invalidate runtime facts even when no control request is in flight.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let peer = self.peer.clone();
        async move { peer.closed().await }
    }

    /// The socket can close just before the OS reports the child's exit.
    /// Only a natural nonzero exit code is evidence of a process-side failure;
    /// a signal, owner termination or lost transport has no plugin attribution.
    pub async fn reported_exit_code(&self, deadline: Duration) -> Option<i32> {
        match tokio::time::timeout(deadline, exited(self.owner.exited.clone())).await {
            Ok(Some(ProcessExit::Child(status))) => status.code().filter(|code| *code != 0),
            _ => None,
        }
    }

    /// An unconfirmed mutating result cannot leave this process accepting more business.
    pub fn invalidate(&self) {
        self.peer.close();
        self.owner.stop.notify_one();
    }

    pub async fn launch(
        group: &ManagedProcessGroup,
        config: SharedLaunchConfig,
    ) -> Result<Self, RpcError> {
        if config.host_generation == 0 || config.host_generation > 9_007_199_254_740_991 {
            return Err(RpcError::new(
                ErrorKind::InvalidParams,
                "Invalid host generation",
            ));
        }
        let endpoint = transport::new_endpoint(&config.endpoint_dir, "shared").map_err(internal)?;
        let mut listener = transport::bind(&endpoint).map_err(internal)?;
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(internal)?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut web_route_proof = Sha256::new();
        web_route_proof.update(b"magi:private-web-routes:v1\0");
        web_route_proof.update(token.as_bytes());
        let mut command = magi_process::tokio_command(&config.node);
        command
            .arg("--experimental-import-meta-resolve")
            .arg(&config.runtime_entry)
            .env("MAGI_EXT_ENDPOINT", &endpoint)
            .env("MAGI_EXT_TOKEN", &token)
            .env_remove("MAGI_EXT_ENTRY")
            .env_remove("MAGI_EXT_COMPAT_ROOT")
            .env_remove("MAGI_EXT_COMPAT_HOME")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = group.spawn_tokio(&mut command).map_err(internal)?;
        let stop = Arc::new(Notify::new());
        let (finished, finished_rx) = watch::channel(None);
        let owner = ProcessOwner {
            stop: Arc::clone(&stop),
            exited: finished_rx,
        };
        tokio::spawn(async move {
            let exit = tokio::select! {
                result = child.wait() => result.map_or(ProcessExit::Owner, ProcessExit::Child),
                _ = stop.notified() => {
                    match tokio::time::timeout(Duration::from_millis(100), child.wait()).await {
                        Ok(Ok(status)) => ProcessExit::Child(status),
                        _ => ProcessExit::Owner,
                    }
                }
            };
            let _ = child.terminate().await;
            finished.send_replace(Some(exit));
        });

        let limits = PeerLimits::default();
        let connect = async {
            let mut stream = listener.accept().await.map_err(internal)?;
            let hello = read_frame(&mut stream, limits.frame_bytes)
                .await
                .map_err(internal)?
                .ok_or_else(|| {
                    RpcError::new(ErrorKind::HostGone, "Shared host disconnected before hello")
                })?;
            let Message::Request { id, method, params } = Message::parse(hello)? else {
                return Err(RpcError::new(
                    ErrorKind::ProtocolError,
                    "Expected host.hello",
                ));
            };
            if method != "host.hello"
                || !constant_time_eq(params["token"].as_str().unwrap_or_default(), &token)
            {
                return Err(RpcError::new(
                    ErrorKind::PermissionDenied,
                    "Invalid shared-host hello",
                ));
            }
            if params["protocol"]["major"] != 1
                || !params["features"]
                    .as_array()
                    .is_some_and(|features| features.iter().any(|value| value == "runtime.shared"))
            {
                return Err(RpcError::new(
                    ErrorKind::VersionMismatch,
                    "Shared runtime contract required",
                ));
            }
            write_frame(
                &mut stream,
                &response_ok(
                    &id,
                    json!({
                        "protocol": {"major": 1, "minor": 0}, "features": ["runtime.shared"],
                        "hostGeneration": config.host_generation,
                        "limits": {"frameBytes": limits.frame_bytes, "pending": limits.pending},
                    }),
                ),
                limits.frame_bytes,
            )
            .await
            .map_err(internal)?;
            Ok(stream)
        };
        let stream = tokio::select! {
            result = tokio::time::timeout(DEADLINE, connect) => result.map_err(|_| RpcError::new(ErrorKind::DeadlineExceeded, "Shared-host handshake timeout"))??,
            _ = exited(owner.exited.clone()) => return Err(RpcError::new(ErrorKind::HostGone, "Shared host exited during handshake")),
        };
        drop(listener);
        let (ready, mut ready_rx) = watch::channel(false);
        let peer = Peer::spawn(
            stream,
            Arc::new(Handler {
                delegate: config.incoming,
                generation: config.host_generation,
                ready,
            }),
            limits,
        );
        // Owning the peer before awaiting readiness closes both transport and process on cancellation.
        let host = Self {
            peer,
            owner,
            generation: config.host_generation,
            web_route_proof: web_route_proof
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        let ready = async {
            while !*ready_rx.borrow_and_update() {
                ready_rx.changed().await.map_err(internal)?;
            }
            Ok::<(), RpcError>(())
        };
        tokio::select! {
            result = tokio::time::timeout(DEADLINE, ready) => result.map_err(|_| RpcError::new(ErrorKind::DeadlineExceeded, "Shared-host ready timeout"))??,
            _ = host.peer.closed() => return Err(RpcError::new(ErrorKind::HostGone, "Shared host disconnected before ready")),
        }
        Ok(host)
    }

    /// Bind once the daemon gateway has a real public origin. The child reports
    /// only a loopback port over the authenticated control channel.
    pub async fn open_web_routes(
        &self,
        public_origin: &str,
    ) -> Result<PrivateWebRouteEndpoint, RpcError> {
        let reply = self
            .control("host.webRoutes.open", json!({"publicOrigin":public_origin}))
            .await?;
        let Some(port) = reply["port"]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0)
        else {
            self.invalidate();
            return Err(RpcError::new(
                ErrorKind::ProtocolError,
                "Invalid private web route endpoint",
            ));
        };
        Ok(PrivateWebRouteEndpoint {
            address: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port),
            host_generation: self.generation,
            proof: self.web_route_proof.clone(),
            peer: self.peer.clone(),
        })
    }

    pub async fn inspect_package(&self, package: PackageInspection) -> Result<Value, RpcError> {
        self.peer
            .request(
                "package.inspect",
                serde_json::to_value(package).map_err(internal)?,
                DEADLINE,
            )
            .await
    }

    /// Parse immutable source bytes with the locked upstream dialect, without applying a graph.
    pub async fn parse_configuration(&self, name: &str, content: &str) -> Result<Value, RpcError> {
        self.peer
            .request(
                "configuration.parse",
                json!({ "name": name, "content": content }),
                DEADLINE,
            )
            .await
    }

    /// Produce an inert graph from captured sources; daemon must commit it before application.
    pub async fn prepare_configuration(&self, input: Value) -> Result<Value, RpcError> {
        self.peer
            .request("configuration.prepare", input, DEADLINE)
            .await
    }

    /// Source write-back is compiled without changing any file or running entry.
    pub async fn prepare_configuration_edit(&self, input: Value) -> Result<Value, RpcError> {
        self.peer
            .request("configuration.edit.prepare", input, DEADLINE)
            .await
    }

    /// Compile bundle-layer source edits (`bundle.patch.yml` / `cordis.patch.yml`) and compatibility checks without side effects.
    pub async fn prepare_bundle_mutation(&self, input: Value) -> Result<Value, RpcError> {
        self.peer
            .request("configuration.bundle.prepare", input, DEADLINE)
            .await
    }

    /// Expose `ctx.pluginManager` on the shared root context once daemon's bundle manager is bound.
    pub async fn open_plugin_manager(&self) -> Result<(), RpcError> {
        self.control("host.pluginManager.open", json!({})).await?;
        Ok(())
    }

    /// Mount the DSH host service closure on the shared root context.
    pub async fn open_services(
        &self,
        dsh_home: &std::path::Path,
        initial_host_config: Value,
    ) -> Result<(), RpcError> {
        self.control(
            "host.services.open",
            json!({
                "dshHome": dsh_home,
                "initialHostConfig": initial_host_config,
            }),
        )
        .await?;
        Ok(())
    }

    /// Invoke a non-lifecycle business service method (`host.tools.*`, `host.web.*`,
    /// `host.commands.*`, `host.skills.*`, `host.llm.*`, `host.settings.*`).
    pub async fn call_service(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, RpcError> {
        self.peer.request(method, params, deadline).await
    }

    pub async fn call_cancellable_service(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, RpcError> {
        self.peer
            .request_cancellable(method, params, deadline)
            .await
    }

    pub async fn apply_configuration(&self, input: Value) -> Result<Value, RpcError> {
        if input["token"]["hostGeneration"].as_u64() != Some(self.generation) {
            return Err(RpcError::new(
                ErrorKind::PermissionDenied,
                "Loading application belongs to another host",
            ));
        }
        self.control("configuration.apply", input).await
    }

    pub async fn activate(&self, activation: Activation) -> Result<Value, RpcError> {
        let mut params = serde_json::to_value(activation).map_err(internal)?;
        params["hostGeneration"] = json!(self.generation);
        self.control("plugin.activate", params).await
    }

    async fn control(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let mut guard = ControlGuard {
            peer: self.peer.clone(),
            stop: Arc::clone(&self.owner.stop),
            settled: false,
        };
        let result = self.peer.request(method, params, DEADLINE).await;
        guard.settled = !matches!(&result, Err(error) if matches!(error.kind(), ErrorKind::DeadlineExceeded | ErrorKind::HostGone));
        result
    }

    pub async fn snapshot(
        &self,
        plugin_id: &str,
        instance_id: &str,
        activation_generation: u64,
    ) -> Result<Value, RpcError> {
        self.peer.request("plugin.snapshot", json!({
            "hostGeneration": self.generation, "pluginId": plugin_id, "instanceId": instance_id, "activationGeneration": activation_generation,
        }), DEADLINE).await
    }

    pub async fn deactivate(
        &self,
        plugin_id: &str,
        instance_id: &str,
        activation_generation: u64,
    ) -> Result<(), RpcError> {
        self.control("plugin.deactivate", json!({
            "hostGeneration": self.generation, "pluginId": plugin_id, "instanceId": instance_id, "activationGeneration": activation_generation,
        })).await?;
        Ok(())
    }

    pub async fn ping(&self, deadline: Duration) -> Result<u64, RpcError> {
        let reply = self.peer.request("host.ping", json!({}), deadline).await?;
        Ok(reply["rssBytes"].as_u64().unwrap_or(0))
    }

    pub async fn stop(&self) -> Result<(), RpcError> {
        let result = self
            .peer
            .request("host.shutdown", json!({}), Duration::from_secs(5))
            .await;
        self.peer.close();
        self.owner.stop.notify_one();
        exited(self.owner.exited.clone()).await;
        result.map(|_| ())
    }
}

impl Drop for SharedHost {
    fn drop(&mut self) {
        self.peer.close();
    }
}

//! Shared-host application of PluginManager's committed configuration.
//! No state is read from UI/Node preferences, and no old runtime is called on failure.
use crate::lifecycle::{LoadingInstanceAction, LoadingInstanceChangeSpec, RequiredLoadingSource};
use crate::{
    LoadingActivation, LoadingApplication, LoadingApplicationToken, LoadingCandidate,
    LoadingEditProposal, LoadingPlan, LoadingSourceEdit, PluginError, PluginManager,
};
use magi_extension_host::{protocol::RpcError, shared::SharedHost};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

mod host_services;
mod source_monitor;
mod web_routes;
pub use host_services::{LoadingHostRestartTrigger, LoadingHostServices};
pub use source_monitor::{LoadingSourceMonitor, LoadingSourceObservation};
pub use web_routes::{LoadingWebRoutes, PluginRouteLease, PluginRoutePage};

pub use crate::lifecycle::LoadingPreparation;

enum CandidateCompilation<'a> {
    Snapshot,
    SourceEdit(&'a Value),
    Installation(&'a [LoadingSourceEdit]),
}

#[derive(Debug)]
pub struct LoadingReconciliation {
    pub revision: u64,
    /// None means the same source was already admitted and applied: no graph
    /// operation was issued. It is not a claim about current instance readiness.
    pub application: Option<Value>,
}

#[derive(Debug)]
pub struct LoadingEditResult {
    pub result: Value,
    /// The revision accepted for this edit; convergence may already include a
    /// later accepted edit by the time it finishes.
    pub accepted_revision: u64,
    pub reconciliation: LoadingReconciliation,
}

/// Durable source acceptance, not a running-graph or plugin-readiness receipt.
/// Startup callbacks may wait for this receipt; they must not wait for their own
/// lifecycle to finish as part of saving configuration.
#[derive(Debug)]
pub struct LoadingEditSubmission {
    pub result: Value,
    pub revision: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadingRuntimeError {
    #[error(transparent)]
    Owner(#[from] PluginError),
    #[error(transparent)]
    Host(#[from] RpcError),
    #[error("加载回执无效：{0}")]
    InvalidReceipt(String),
    #[error("加载准备结果无效：{0}")]
    InvalidPlan(#[from] serde_json::Error),
    #[error("加载来源监视失败：{0}")]
    Monitor(#[from] notify::Error),
    #[error("加载来源监视任务失败：{0}")]
    MonitorTask(#[from] tokio::task::JoinError),
}

#[derive(Clone)]
pub struct LoadingRuntime {
    owner: Arc<Mutex<PluginManager>>,
    host: Arc<SharedHost>,
    admission: Arc<tokio::sync::Mutex<()>>,
    application: Arc<tokio::sync::Mutex<()>>,
    admitted: tokio::sync::watch::Sender<u64>,
    prepare_application: Option<Arc<PrepareApplication>>,
}

type PrepareApplication =
    dyn Fn(&crate::LoadingConfiguration) -> Result<(), PluginError> + Send + Sync;

fn lock(owner: &Mutex<PluginManager>) -> Result<MutexGuard<'_, PluginManager>, PluginError> {
    owner
        .lock()
        .map_err(|_| PluginError::CorruptState("插件管理器锁已损坏".into()))
}

fn capture_edit(
    owner: &PluginManager,
    expected_revision: u64,
) -> Result<(LoadingCandidate, Value, LoadingPreparation), PluginError> {
    if owner.pending_loading_edit().is_some() {
        return Err(PluginError::Conflict("加载来源有未完成的编辑事务".into()));
    }
    let current = owner
        .loading_configuration()
        .ok_or_else(|| PluginError::Conflict("加载编辑需要已接纳的配置".into()))?;
    let names = current
        .sources
        .iter()
        .map(|source| source.name().to_owned())
        .collect::<Vec<_>>();
    let candidate = owner.prepare_loading_configuration(expected_revision, &names)?;
    if candidate.sources_sha256() != current.sources_sha256 {
        return Err(PluginError::Conflict(
            "配置文件已有未接纳变更，请先处理来源冲突".into(),
        ));
    }
    Ok((
        candidate,
        current.graph.clone(),
        current.preparation.clone(),
    ))
}

fn commit_edit(
    owner: &mut PluginManager,
    preparation: &LoadingPreparation,
    candidate: LoadingCandidate,
    proposal: LoadingEditProposal,
    replacing_installation: bool,
) -> Result<u64, PluginError> {
    if proposal.base_sources_sha256 != candidate.sources_sha256()
        || &proposal.plan.preparation != preparation
    {
        return Err(PluginError::Conflict("加载编辑来源或安装项目已变化".into()));
    }
    if proposal.edits.is_empty() {
        return Ok(owner
            .accept_loading_configuration(candidate, proposal.plan)?
            .revision);
    }
    if replacing_installation {
        owner.begin_installation_edit(candidate, proposal)?;
    } else {
        owner.begin_loading_edit(candidate, proposal)?;
    }
    Ok(owner.finish_loading_edit()?.revision)
}

struct UnconfirmedApplication {
    owner: Arc<Mutex<PluginManager>>,
    host: Arc<SharedHost>,
    token: LoadingApplicationToken,
    confirmed: bool,
}

impl Drop for UnconfirmedApplication {
    fn drop(&mut self) {
        if self.confirmed {
            return;
        }
        self.host.invalidate();
        if let Ok(mut owner) = lock(&self.owner) {
            owner.loading_host_ended(
                self.token.host_generation,
                "加载操作未确认；宿主已失效，已保存的配置没有回滚".into(),
            );
        }
    }
}

impl LoadingRuntime {
    pub fn new(owner: Arc<Mutex<PluginManager>>, host: Arc<SharedHost>) -> Self {
        let closed = host.closed();
        let generation = host.generation();
        let weak_owner = Arc::downgrade(&owner);
        tokio::spawn(async move {
            closed.await;
            if let Some(owner) = weak_owner.upgrade()
                && let Ok(mut owner) = lock(&owner)
            {
                owner.loading_host_ended(
                    generation,
                    "共享插件宿主已断开；已保存的配置将在重新激活后生效".into(),
                );
            }
        });
        Self {
            owner,
            host,
            admission: Arc::default(),
            application: Arc::default(),
            admitted: tokio::sync::watch::channel(0).0,
            prepare_application: None,
        }
    }

    pub fn with_prepare_application(
        mut self,
        prepare: impl Fn(&crate::LoadingConfiguration) -> Result<(), PluginError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.prepare_application = Some(Arc::new(prepare));
        self
    }

    pub fn host_generation(&self) -> u64 {
        self.host.generation()
    }

    fn validate_host_restart(
        &self,
        caller: &host_services::LoadingCaller,
        minimum_revision: u64,
    ) -> Result<u64, LoadingRuntimeError> {
        let owner = lock(&self.owner)?;
        caller.validate(self, &owner)?;
        let current = owner
            .loading_configuration()
            .ok_or_else(|| PluginError::Conflict("尚无已接纳的加载配置".into()))?;
        if minimum_revision == 0 || minimum_revision > current.revision {
            return Err(PluginError::Conflict("请求的宿主重启修订尚未接纳".into()).into());
        }
        if owner.pending_loading_edit().is_some() {
            return Err(PluginError::Conflict("加载来源有未完成的编辑事务".into()).into());
        }
        Ok(self.host.generation())
    }

    /// Serialize graph application, releasing the owner lock before any Node wait.
    pub async fn apply_current(&self) -> Result<Value, LoadingRuntimeError> {
        let _application = self.application.lock().await;
        self.drain_applications_locked().await
    }

    /// Reconcile the files of the accepted installation, or initialize an
    /// empty runtime with its first preparation. Replacement uses the
    /// installation submission path.
    pub async fn reconcile_sources(
        &self,
        expected_revision: u64,
        preparation: &LoadingPreparation,
    ) -> Result<LoadingReconciliation, LoadingRuntimeError> {
        {
            let _admission = self.admission.lock().await;
            self.admit_sources_locked(expected_revision, preparation, None, None)
                .await?;
        }
        self.apply_latest().await
    }

    async fn reconcile_notified_sources(
        &self,
        initial: &LoadingPreparation,
        watched: &source_monitor::WatchedSources,
        starting: bool,
    ) -> Result<LoadingReconciliation, LoadingRuntimeError> {
        let admission = async {
            let _admission = self.admission.lock().await;
            let (revision, preparation) = {
                let mut owner = lock(&self.owner)?;
                // A new daemon can finish a journal only from known before/after
                // bytes. Do this before considering a fresh source candidate.
                if starting && owner.pending_loading_edit().is_some() {
                    owner.finish_loading_edit()?;
                }
                owner.loading_configuration().map_or_else(
                    || (0, initial.clone()),
                    |current| (current.revision, current.preparation.clone()),
                )
            };
            self.admit_sources_locked(revision, &preparation, None, Some(watched))
                .await
        }
        .await;
        if let Err(error) = admission {
            let restore_accepted = starting && {
                let owner = lock(&self.owner)?;
                owner.loading_configuration().is_some() && owner.loading_application().is_none()
            };
            if restore_accepted {
                // On restart an invalid draft cannot activate, or prevent
                // recovery of the last configuration the daemon accepted.
                self.apply_latest().await?;
            }
            return Err(error);
        }
        self.apply_latest().await
    }

    async fn admit_sources_locked(
        &self,
        expected_revision: u64,
        preparation: &LoadingPreparation,
        caller: Option<&host_services::LoadingCaller>,
        watched: Option<&source_monitor::WatchedSources>,
    ) -> Result<(), LoadingRuntimeError> {
        let (mut candidate, previous) = {
            let owner = lock(&self.owner)?;
            if let Some(caller) = caller {
                caller.validate(self, &owner)?;
            }
            let current = owner.loading_configuration();
            if current.is_some_and(|config| &config.preparation != preparation) {
                return Err(PluginError::Conflict("安装项目已变化，请提交安装修订".into()).into());
            }
            if let Some(watched) = watched {
                watched.set(
                    preparation.root_sources().map(String::as_str).chain(
                        current
                            .into_iter()
                            .flat_map(|config| config.sources.iter().map(|source| source.name())),
                    ),
                );
            }
            (
                owner.prepare_loading_configuration(
                    expected_revision,
                    &preparation.root_sources().cloned().collect::<Vec<_>>(),
                )?,
                current.map(|configuration| configuration.graph.clone()),
            )
        };
        let (plan, creations) = self
            .compile_candidate(
                preparation,
                &mut candidate,
                previous.as_ref(),
                CandidateCompilation::Snapshot,
                watched,
            )
            .await?;
        let plan: LoadingPlan = serde_json::from_value(plan)?;
        let revision = {
            let mut owner = lock(&self.owner)?;
            if let Some(caller) = caller {
                caller.validate(self, &owner)?;
            }
            if &plan.preparation != preparation {
                return Err(LoadingRuntimeError::InvalidReceipt(
                    "编译回执的安装项目不匹配".into(),
                ));
            }
            let configuration = if creations.is_empty() {
                owner.accept_loading_configuration(candidate, plan)?
            } else {
                let proposal = LoadingEditProposal {
                    base_sources_sha256: candidate.sources_sha256().into(),
                    edits: creations,
                    plan,
                    result: Value::Null,
                };
                owner.begin_source_initialization(candidate, proposal)?;
                owner.finish_loading_edit()?
            };
            if let Some(watched) = watched {
                watched.set(configuration.sources.iter().map(|source| source.name()));
            }
            configuration.revision
        };
        self.notify_admission(revision);
        Ok(())
    }

    async fn compile_candidate(
        &self,
        preparation: &LoadingPreparation,
        candidate: &mut LoadingCandidate,
        previous: Option<&Value>,
        mode: CandidateCompilation<'_>,
        watched: Option<&source_monitor::WatchedSources>,
    ) -> Result<(Value, Vec<LoadingSourceEdit>), LoadingRuntimeError> {
        // A request for additional inputs is a normal planning step, never an
        // execution retry. The owner captures only reachable configuration files.
        let mut creations = Vec::new();
        loop {
            let proposed = match mode {
                CandidateCompilation::Installation(edits) => {
                    let mut proposed = edits.to_vec();
                    proposed.extend(creations.iter().cloned());
                    Some(candidate.sources_after_edits(&proposed)?)
                }
                _ if !creations.is_empty() => Some(candidate.sources_after_edits(&creations)?),
                _ => None,
            };
            let mut input = json!({
                "projectDirectory": preparation.project_directory,
                "sourceDirectory": candidate.source_root(),
                "sources": proposed.as_deref().unwrap_or_else(|| candidate.sources()),
                "base": preparation.base, "overlays": preparation.overlays,
                "packages": preparation.packages, "previous": previous,
            });
            let result = if let CandidateCompilation::SourceEdit(operation) = mode {
                input["operation"] = operation.clone();
                self.host.prepare_configuration_edit(input).await
            } else {
                self.host.prepare_configuration(input).await
            };
            match result {
                Ok(result) => return Ok((result, creations)),
                Err(error) => {
                    let Some(required) = error.data.detail.get("requiredSources") else {
                        return Err(error.into());
                    };
                    let required: Vec<RequiredLoadingSource> =
                        serde_json::from_value(required.clone())?;
                    lock(&self.owner)?.expand_loading_candidate(
                        candidate,
                        required,
                        &mut creations,
                        |name| {
                            if let Some(watched) = watched {
                                watched.add(name);
                            }
                        },
                    )?;
                }
            }
        }
    }

    /// Include synchronization joins accepted configuration and graph application,
    /// not every plugin's startup. Refresh explicitly recaptures the real sources.
    async fn synchronize_for_caller(
        &self,
        minimum_revision: u64,
        caller: &host_services::LoadingCaller,
        refresh: bool,
    ) -> Result<LoadingReconciliation, LoadingRuntimeError> {
        {
            let _admission = self.admission.lock().await;
            let (revision, preparation) = {
                let owner = lock(&self.owner)?;
                caller.validate(self, &owner)?;
                let current = owner
                    .loading_configuration()
                    .ok_or_else(|| PluginError::Conflict("尚无已接纳的加载配置".into()))?;
                let revision = current.revision;
                if minimum_revision == 0 || minimum_revision > revision {
                    return Err(PluginError::Conflict("请求的加载配置修订尚未接纳".into()).into());
                }
                if owner.pending_loading_edit().is_some() {
                    return Err(PluginError::Conflict(
                        "加载来源有未完成的编辑事务，请先恢复该事务".into(),
                    )
                    .into());
                }
                (revision, refresh.then(|| current.preparation.clone()))
            };
            if let Some(preparation) = preparation {
                self.admit_sources_locked(revision, &preparation, Some(caller), None)
                    .await?;
            }
        }
        self.apply_latest().await
    }

    /// Compile a source edit, journal candidate writes, and submit through the
    /// same source CAS before any running entry sees the new configuration.
    pub async fn edit_sources(
        &self,
        expected_revision: u64,
        operation: Value,
    ) -> Result<LoadingEditResult, LoadingRuntimeError> {
        let submission = self
            .submit_source_edit(expected_revision, operation)
            .await?;
        Ok(LoadingEditResult {
            result: submission.result,
            accepted_revision: submission.revision,
            reconciliation: self.apply_latest().await?,
        })
    }

    /// Accept an edit without waiting for plugin execution. The source monitor
    /// owns subsequent convergence; an in-flight application also drains newer
    /// accepted revisions before returning. This path can be called reentrantly
    /// while the host waits for a startup/update/disposal callback.
    pub async fn submit_source_edit(
        &self,
        expected_revision: u64,
        operation: Value,
    ) -> Result<LoadingEditSubmission, LoadingRuntimeError> {
        self.submit_source_edit_inner(expected_revision, operation, None)
            .await
    }

    /// Commit a `profilePatchConfig` edit from `ctx.configEditor` / `ctx.settings`
    /// and apply immediately unless a graph application is already in flight
    /// (in which case `drain_applications_locked` drains the admitted revision).
    pub(crate) async fn edit_profile_patch(
        &self,
        expected_revision: u64,
        id: String,
        name: String,
        next_config: Value,
        clear_override: bool,
    ) -> Result<u64, LoadingRuntimeError> {
        let submission = self
            .submit_source_edit_inner(
                expected_revision,
                json!({
                    "kind": "profilePatchConfig",
                    "id": id,
                    "name": name,
                    "nextConfig": next_config,
                    "clearOverride": clear_override,
                }),
                None,
            )
            .await?;
        if let Ok(_guard) = self.application.try_lock() {
            let _ = self.drain_applications_locked().await?;
        }
        Ok(submission.revision)
    }

    /// Submit a fully prepared immutable package project and edits to existing
    /// sources as one installation revision. Cancelling before admission writes
    /// neither sources nor owner state. After submission the project must be
    /// kept while either a pending journal or the accepted configuration
    /// references it, including when finishing the journal returns an error.
    pub async fn submit_installation(
        &self,
        expected_revision: u64,
        preparation: &LoadingPreparation,
        mut edits: Vec<LoadingSourceEdit>,
    ) -> Result<u64, LoadingRuntimeError> {
        let _admission = self.admission.lock().await;
        let (mut candidate, previous, _) = {
            let owner = lock(&self.owner)?;
            capture_edit(&owner, expected_revision)?
        };
        let (plan, creations) = self
            .compile_candidate(
                preparation,
                &mut candidate,
                Some(&previous),
                CandidateCompilation::Installation(&edits),
                None,
            )
            .await?;
        edits.extend(creations);
        let plan = serde_json::from_value(plan)?;
        let proposal = LoadingEditProposal {
            base_sources_sha256: candidate.sources_sha256().into(),
            edits,
            plan,
            result: Value::Null,
        };
        let revision = {
            let mut owner = lock(&self.owner)?;
            commit_edit(&mut owner, preparation, candidate, proposal, true)?
        };
        self.notify_admission(revision);
        Ok(revision)
    }

    async fn submit_source_edit_inner(
        &self,
        expected_revision: u64,
        operation: Value,
        caller: Option<&host_services::LoadingCaller>,
    ) -> Result<LoadingEditSubmission, LoadingRuntimeError> {
        let _admission = self.admission.lock().await;
        let (mut candidate, previous, preparation) = {
            let owner = lock(&self.owner)?;
            if let Some(caller) = caller {
                caller.validate(self, &owner)?;
            }
            capture_edit(&owner, expected_revision)?
        };
        let (proposal, creations) = self
            .compile_candidate(
                &preparation,
                &mut candidate,
                Some(&previous),
                CandidateCompilation::SourceEdit(&operation),
                None,
            )
            .await?;
        let mut proposal: LoadingEditProposal = serde_json::from_value(proposal)?;
        if !creations.is_empty() {
            if proposal.base_sources_sha256 != candidate.digest_after_edits(&creations)? {
                return Err(LoadingRuntimeError::InvalidReceipt(
                    "加载初始化回执未匹配候选来源".into(),
                ));
            }
            proposal.base_sources_sha256 = candidate.sources_sha256().into();
            proposal.edits.extend(creations);
        }
        let result = proposal.result.clone();
        let revision = {
            let mut owner = lock(&self.owner)?;
            if let Some(caller) = caller {
                caller.validate(self, &owner)?;
            }
            commit_edit(&mut owner, &preparation, candidate, proposal, false)?
        };
        self.notify_admission(revision);
        Ok(LoadingEditSubmission { result, revision })
    }

    /// Explicit recovery of a journaled source save; never replays plugin calls.
    pub async fn resume_source_edit(&self) -> Result<LoadingReconciliation, LoadingRuntimeError> {
        {
            let _admission = self.admission.lock().await;
            let revision = lock(&self.owner)?.finish_loading_edit()?.revision;
            self.notify_admission(revision);
        }
        self.apply_latest().await
    }

    fn notify_admission(&self, revision: u64) {
        self.admitted.send_if_modified(|current| {
            if *current == revision {
                return false;
            }
            *current = revision;
            true
        });
    }

    async fn apply_latest(&self) -> Result<LoadingReconciliation, LoadingRuntimeError> {
        let _application = self.application.lock().await;
        let revision = lock(&self.owner)?
            .loading_configuration()
            .ok_or_else(|| PluginError::Conflict("尚无已接纳的加载配置".into()))?
            .revision;
        let already_applied = {
            let owner = lock(&self.owner)?;
            let already_applied = matches!(owner.loading_application(),
                Some(LoadingApplication::Applied(token)) if token.revision == revision
                    && token.host_generation == self.host.generation());
            already_applied
        };
        if already_applied {
            return Ok(LoadingReconciliation {
                revision,
                application: None,
            });
        }
        let receipt = self.drain_applications_locked().await?;
        Ok(LoadingReconciliation {
            revision: receipt["token"]["revision"]
                .as_u64()
                .expect("validated application receipt"),
            application: Some(receipt),
        })
    }

    async fn drain_applications_locked(&self) -> Result<Value, LoadingRuntimeError> {
        loop {
            let receipt = self.apply_locked(None).await?;
            let owner = lock(&self.owner)?;
            if matches!(owner.loading_application(), Some(LoadingApplication::Applied(token))
                if receipt["token"] == json!(token))
            {
                return Ok(receipt);
            }
            // A valid old receipt is not a receipt for newer saved intent.
            // Continue from the latest committed graph, never replay source edits.
        }
    }

    async fn change_loading_instances(
        &self,
        request: &host_services::InstanceChange,
    ) -> Result<Value, LoadingRuntimeError> {
        let _application = self.application.lock().await;
        self.apply_locked(Some(request)).await
    }

    async fn apply_locked(
        &self,
        change: Option<&host_services::InstanceChange>,
    ) -> Result<Value, LoadingRuntimeError> {
        let (token, previous_bindings, configuration, activations) = {
            let mut owner = lock(&self.owner)?;
            if let Some(change) = change {
                change.caller.validate(self, &owner)?;
                if !matches!(owner.loading_application(), Some(LoadingApplication::Applied(token))
                    if token.host_generation == self.host.generation() && token.revision == change.expected_revision)
                {
                    return Err(PluginError::Conflict("加载条目所见修订已失效".into()).into());
                }
            }
            let (token, previous_bindings) = owner.begin_loading_application_with_action(
                self.host.generation(),
                change.map(|change| LoadingInstanceChangeSpec {
                    action: change.action,
                    entry_id: change.entry_id.as_deref(),
                    targets: change.targets.as_slice(),
                    activate_targets: change.activate_targets.as_slice(),
                }),
            )?;
            let activations = owner.loading_activations(&token)?;
            let configuration = owner
                .loading_configuration()
                .expect("begun configuration")
                .clone();
            if let Some(prepare) = &self.prepare_application
                && let Err(error) = prepare(&configuration)
            {
                owner.finish_loading_application(&token, Err(error.to_string()))?;
                return Err(error.into());
            }
            (token, previous_bindings, configuration, activations)
        };
        let mut pending = UnconfirmedApplication {
            owner: self.owner.clone(),
            host: self.host.clone(),
            token: token.clone(),
            confirmed: false,
        };
        let action = change.filter(|change| change.action != LoadingInstanceAction::Activate);
        let receipt = self
            .host
            .apply_configuration(json!({
                "token": token, "configuration": configuration, "activations": activations,
                "instanceAction": action.map(|change| json!({
                    "kind": change.action,
                    "entryId": change.entry_id,
                    "targets": change.targets,
                    "activateTargets": change.activate_targets,
                })),
            }))
            .await?;
        let instances = receipt["instances"]
            .as_array()
            .ok_or_else(|| LoadingRuntimeError::InvalidReceipt("缺少实例状态".into()))?;
        let refresh_conflict = action
            .is_some_and(|change| change.action == LoadingInstanceAction::Refresh)
            && receipt["refreshConflict"] == true;
        if refresh_conflict {
            if receipt["token"] != json!(token) {
                return Err(LoadingRuntimeError::InvalidReceipt("修订不匹配".into()));
            }
            let mut owner = lock(&self.owner)?;
            if owner
                .loading_configuration()
                .map(|configuration| configuration.revision)
                != Some(token.revision)
            {
                pending.confirmed = true;
                return Err(PluginError::Conflict("刷新前加载配置已变化".into()).into());
            }
            let restored = owner.abort_loading_refresh(&token, previous_bindings)?;
            validate_receipt_instances(instances, &restored)?;
            pending.confirmed = true;
            for instance in instances {
                if matches!(instance["state"].as_str(), Some("failed" | "stopped"))
                    && let Some(activation) = restored.iter().find(|activation| {
                        instance["instanceId"] == activation.executable.instance_id
                    })
                {
                    owner.observe_loading_scope(activation, instance)?;
                }
            }
            return Err(PluginError::Conflict("刷新前加载实例已结束".into()).into());
        }
        if receipt["token"] != json!(token) {
            return Err(LoadingRuntimeError::InvalidReceipt("修订不匹配".into()));
        }
        validate_receipt_instances(instances, &activations)?;
        if let Some(action) = action.filter(|change| change.action == LoadingInstanceAction::Stop)
            && action.targets.iter().any(|target| {
                !instances.iter().any(|instance| {
                    instance["instanceId"] == target.instance_id
                        && matches!(instance["state"].as_str(), Some("stopped" | "failed"))
                })
            })
        {
            return Err(LoadingRuntimeError::InvalidReceipt(
                "终止后仍有实例在运行".into(),
            ));
        }
        // A valid old receipt may be superseded by a new desired revision. Do not kill a healthy host.
        pending.confirmed = true;
        let mut owner = lock(&self.owner)?;
        if owner
            .loading_configuration()
            .map(|configuration| configuration.revision)
            == Some(token.revision)
        {
            owner.finish_loading_application(&token, Ok(()))?;
        }
        for instance in instances {
            if matches!(instance["state"].as_str(), Some("failed" | "stopped"))
                && let Some(activation) = activations
                    .iter()
                    .find(|activation| instance["instanceId"] == activation.executable.instance_id)
            {
                owner.observe_loading_scope(activation, instance)?;
            }
        }
        Ok(receipt)
    }

    /// Late notifications cannot revoke a replacement instance.
    pub fn observe_instance_status(&self, status: &Value) -> Result<(), PluginError> {
        let mut owner = lock(&self.owner)?;
        let Some(binding) = status["instanceId"]
            .as_str()
            .and_then(|id| owner.loading_instance_status(id))
            .map(|(activation, _)| activation.clone())
        else {
            return Ok(());
        };
        if status["hostGeneration"] == binding.host_generation
            && status["activationGeneration"] == binding.activation_generation
            && status["pluginId"] == binding.executable.plugin_id
            && status["version"] == binding.executable.version
        {
            owner.observe_loading_scope(&binding, status)?;
        }
        Ok(())
    }
}

fn validate_receipt_instances(
    instances: &[Value],
    activations: &[LoadingActivation],
) -> Result<(), LoadingRuntimeError> {
    if instances.len() != activations.len() {
        return Err(LoadingRuntimeError::InvalidReceipt(
            "修订或实例数量不匹配".into(),
        ));
    }
    let mut seen = BTreeSet::new();
    for instance in instances {
        let activation = activations
            .iter()
            .find(|activation| instance["instanceId"] == activation.executable.instance_id)
            .ok_or_else(|| LoadingRuntimeError::InvalidReceipt("未知实例".into()))?;
        if !seen.insert(&activation.executable.instance_id)
            || instance["hostGeneration"] != activation.host_generation
            || instance["pluginId"] != activation.executable.plugin_id
            || instance["activationGeneration"] != activation.activation_generation
            || instance["version"] != activation.executable.version
            || !matches!(
                instance["state"].as_str(),
                Some("starting" | "waiting" | "running" | "failed" | "stopped")
            )
        {
            return Err(LoadingRuntimeError::InvalidReceipt(
                "实例身份或状态不匹配".into(),
            ));
        }
    }
    Ok(())
}

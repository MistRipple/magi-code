//! Loading sources, accepted configuration revisions and fenced application receipts.
//! The shared host interprets source syntax; only PluginManager commits these facts.
use super::*;
use magi_extension_host::shared::PackageInspection;
use sha2::{Digest, Sha256};
use std::io::Read;

mod edit;
pub use edit::{LoadingEditProposal, LoadingSourceEdit};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_CONFIGURATION_BYTES: usize = 4 * MAX_SOURCE_BYTES;
const MAX_SOURCES: usize = 64;
const MAX_GENERATION: u64 = 9_007_199_254_740_991;

/// Immutable installation inputs submitted by the owner. Source notifications
/// and edits use these inputs from the current accepted configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingPreparation {
    pub project_directory: PathBuf,
    pub base: String,
    pub overlays: Vec<String>,
    pub packages: Vec<PackageInspection>,
}

impl LoadingPreparation {
    pub(crate) fn root_sources(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.base).chain(&self.overlays)
    }

    fn validate(&self, sources: &[LoadingSource]) -> Result<(), PluginError> {
        let mut names = BTreeSet::new();
        if !self.project_directory.is_absolute()
            || self.root_sources().any(|name| {
                !names.insert(name) || !sources.iter().any(|source| source.name == *name)
            })
            || self.packages.iter().any(|package| {
                package.project_directory != self.project_directory
                    || !package.directory.starts_with(&self.project_directory)
            })
        {
            return Err(conflict("加载计划的安装项目或根来源不一致"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingSource {
    name: String,
    content: String,
    sha256: String,
}

impl LoadingSource {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Private captured inputs can be inspected by the host but not edited by a caller.
#[derive(Debug)]
pub struct LoadingCandidate {
    expected_revision: u64,
    source_root: PathBuf,
    sources: Vec<LoadingSource>,
    sources_sha256: String,
}

impl LoadingCandidate {
    pub fn sources(&self) -> &[LoadingSource] {
        &self.sources
    }
    pub fn sources_sha256(&self) -> &str {
        &self.sources_sha256
    }
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    pub(crate) fn sources_after_edits(
        &self,
        edits: &[LoadingSourceEdit],
    ) -> Result<Vec<LoadingSource>, PluginError> {
        edit::apply_source_edits(&self.sources, edits)
    }

    pub(crate) fn digest_after_edits(
        &self,
        edits: &[LoadingSourceEdit],
    ) -> Result<String, PluginError> {
        sources_digest(&self.sources_after_edits(edits)?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequiredLoadingSource {
    name: String,
    initial: Vec<String>,
}

/// Result of the host's complete source/dependency validation, before any graph execution.
/// Graph syntax is internal to the shared host, not a second Rust plugin API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingPlan {
    pub source_root: PathBuf,
    pub source_names: Vec<String>,
    pub sources_sha256: String,
    pub preparation: LoadingPreparation,
    pub graph: Value,
    pub executables: Vec<LoadingExecutable>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingExecutable {
    pub instance_id: String,
    pub plugin_id: String,
    pub version: String,
    pub entry: PathBuf,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadingActivation {
    pub executable: LoadingExecutable,
    pub host_generation: u64,
    pub activation_generation: u64,
    pub activation_order: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadingHostCrashSummary {
    pub host_generation: u64,
    pub reason: String,
    pub activating_instances: Vec<LoadingActivation>,
    pub active_instances: Vec<LoadingActivation>,
}

#[derive(Clone)]
pub(super) struct LoadingBinding {
    activation: LoadingActivation,
    scope: Option<LoadingScope>,
    ended: bool,
    status: LoadingInstanceStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadingInstanceStatus {
    Starting,
    Waiting(Vec<String>),
    Running,
    Stopping,
    Stopped,
    Failed(Option<String>),
}

/// The latest observed Cordis scope of an issued activation. Dropping its
/// running sender fences every admitted call without persisting a second fact.
#[derive(Clone)]
struct LoadingScope {
    configuration_epoch: u64,
    configuration_active: bool,
    activating: bool,
    call_epoch: u64,
    running: Option<Arc<tokio::sync::watch::Sender<()>>>,
    scope_revoked: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingConfiguration {
    pub revision: u64,
    pub sources: Vec<LoadingSource>,
    pub sources_sha256: String,
    pub source_root: PathBuf,
    pub preparation: LoadingPreparation,
    pub graph: Value,
    pub executables: Vec<LoadingExecutable>,
}

fn loading_entry_config<'a>(nodes: &'a [Value], instance: &str) -> Option<&'a Value> {
    for node in nodes {
        if node["instanceId"] == instance {
            return node["options"].get("config");
        }
        if let Some(config) = node["children"]
            .as_array()
            .and_then(|children| loading_entry_config(children, instance))
        {
            return Some(config);
        }
    }
    None
}

fn accepted_entry_config<'a>(graph: &'a Value, instance: &str) -> Option<&'a Value> {
    graph["root"]["entries"]
        .as_array()
        .and_then(|nodes| loading_entry_config(nodes, instance))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LoadingState {
    host_generation: u64,
    application_sequence: u64,
    configuration: Option<LoadingConfiguration>,
    instance_generations: BTreeMap<String, u64>,
    plugin_order: BTreeMap<String, u64>,
    order_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_edit: Option<edit::PendingLoadingEdit>,
}

impl LoadingState {
    pub(super) fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub(super) fn validate(&self) -> Result<(), PluginError> {
        if self.host_generation > MAX_GENERATION || self.application_sequence > MAX_GENERATION {
            return Err(PluginError::CorruptState("加载代次超限".into()));
        }
        if self.order_sequence > MAX_GENERATION
            || self
                .instance_generations
                .values()
                .chain(self.plugin_order.values())
                .any(|value| *value == 0 || *value > MAX_GENERATION)
        {
            return Err(PluginError::CorruptState("加载实例代次或顺序无效".into()));
        }
        if let Some(configuration) = &self.configuration {
            validate_sources(&configuration.sources)?;
            configuration.preparation.validate(&configuration.sources)?;
            validate_graph(&configuration.graph)?;
            validate_executables(&configuration.executables)?;
            if configuration.revision == 0
                || configuration.revision > MAX_GENERATION
                || !configuration.source_root.is_absolute()
                || configuration.sources_sha256 != sources_digest(&configuration.sources)?
            {
                return Err(PluginError::CorruptState("加载配置修订或摘要无效".into()));
            }
        }
        if let Some(pending) = &self.pending_edit {
            pending.validate(self.configuration.as_ref())?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadingApplicationToken {
    pub revision: u64,
    pub host_generation: u64,
    pub sequence: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LoadingInstanceAction {
    Activate,
    Stop,
    Refresh,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LoadingInstanceTarget {
    pub(crate) instance_id: String,
    pub(crate) activation_generation: u64,
}

pub(crate) struct LoadingInstanceChangeSpec<'a> {
    pub(crate) action: LoadingInstanceAction,
    pub(crate) entry_id: Option<&'a str>,
    pub(crate) targets: &'a [LoadingInstanceTarget],
    pub(crate) activate_targets: &'a [LoadingInstanceTarget],
}

pub(crate) struct LoadingBindingsSnapshot(BTreeMap<String, LoadingBinding>);

/// An in-memory observation. Restart never restores an old process's "applied" state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadingApplication {
    Applying(LoadingApplicationToken),
    Applied(LoadingApplicationToken),
    Failed(LoadingApplicationToken, String),
}

impl PluginManager {
    /// The actual editable loading project; no projection of an old profile.
    pub fn loading_source_root(&self) -> PathBuf {
        self.root.join("loading-source")
    }

    pub fn loading_configuration(&self) -> Option<&LoadingConfiguration> {
        self.state.loading.configuration.as_ref()
    }

    pub fn loading_application(&self) -> Option<&LoadingApplication> {
        self.loading_application.as_ref()
    }

    pub fn prepare_loading_configuration(
        &self,
        expected_revision: u64,
        names: &[String],
    ) -> Result<LoadingCandidate, PluginError> {
        self.check_loading_revision(expected_revision)?;
        if names.is_empty() || names.len() > MAX_SOURCES {
            return Err(PluginError::InvalidInput("加载来源数量无效".into()));
        }
        let source_root = fs::canonicalize(self.loading_source_root())?;
        let sources: Vec<_> = names
            .iter()
            .map(|name| capture_source(&source_root, name))
            .collect::<Result<_, _>>()?;
        let mut sources = sources;
        sources.sort_by(|a, b| a.name.cmp(&b.name));
        validate_sources(&sources)?;
        let sources_sha256 = sources_digest(&sources)?;
        Ok(LoadingCandidate {
            expected_revision,
            source_root,
            sources,
            sources_sha256,
        })
    }

    /// Expand a candidate using the compiler's unfulfilled Include references.
    /// Previously captured bytes stay fixed until the ordinary commit recheck.
    pub(crate) fn expand_loading_candidate(
        &self,
        candidate: &mut LoadingCandidate,
        required: Vec<RequiredLoadingSource>,
        proposed: &mut Vec<LoadingSourceEdit>,
        track: impl Fn(&str),
    ) -> Result<(), PluginError> {
        self.check_loading_revision(candidate.expected_revision)?;
        if fs::canonicalize(self.loading_source_root())? != candidate.source_root {
            return Err(conflict("加载来源目录已经变化"));
        }
        if required.is_empty()
            || required.len() + candidate.sources.len() + proposed.len() > MAX_SOURCES
        {
            return Err(PluginError::InvalidInput("加载来源数量无效".into()));
        }
        let mut seen = BTreeSet::new();
        for RequiredLoadingSource { name, .. } in &required {
            let path = Path::new(name);
            if !valid_source_name(name)
                || !seen.insert(name)
                || !matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("yaml" | "yml" | "json")
                )
                || candidate.sources.iter().any(|source| source.name == *name)
                || proposed.iter().any(|creation| creation.name == *name)
            {
                return Err(PluginError::InvalidInput(format!(
                    "加载来源无效或重复：{name}"
                )));
            }
            track(name);
        }
        for RequiredLoadingSource { name, initial } in required {
            match capture_source(&candidate.source_root, &name) {
                Ok(source) => candidate.sources.push(source),
                Err(PluginError::Storage(error))
                    if error.kind() == std::io::ErrorKind::NotFound && !initial.is_empty() =>
                {
                    let [content]: [String; 1] = initial
                        .try_into()
                        .map_err(|_| conflict(format!("加载来源 {name} 有多个不同的初始配置")))?;
                    edit::new_source_path(&candidate.source_root, &name)?;
                    proposed.push(LoadingSourceEdit {
                        name,
                        expected_sha256: None,
                        content,
                    });
                }
                Err(error) => {
                    return Err(PluginError::InvalidInput(format!(
                        "无法读取加载来源 {name}：{error}"
                    )));
                }
            }
        }
        candidate.sources.sort_by(|a, b| a.name.cmp(&b.name));
        validate_sources(&candidate.sources)?;
        candidate.sources_sha256 = sources_digest(&candidate.sources)?;
        Ok(())
    }

    /// The coordinator must supply the complete validated plan, never just parsed YAML.
    /// This commits intent, not a claim that plugins have started or completed business.
    pub fn accept_loading_configuration(
        &mut self,
        candidate: LoadingCandidate,
        plan: LoadingPlan,
    ) -> Result<LoadingConfiguration, PluginError> {
        self.check_loading_revision(candidate.expected_revision)?;
        if fs::canonicalize(self.loading_source_root())? != candidate.source_root
            || plan.source_root != candidate.source_root
        {
            return Err(conflict("加载来源或编译输入已变化"));
        }
        validate_graph(&plan.graph)?;
        validate_executables(&plan.executables)?;
        let sources = select_plan_sources(&candidate.sources, &plan)?;
        plan.preparation.validate(&sources)?;
        if let Some(pending) = &self.state.loading.pending_edit
            && (pending.expected_revision != candidate.expected_revision
                || pending.proposal.plan != plan
                || candidate.sources != pending.expected_sources()?)
        {
            return Err(conflict("加载来源有未完成的编辑事务，请先恢复该事务"));
        }
        for source in &candidate.sources {
            if capture_source(&candidate.source_root, &source.name)? != *source {
                return Err(conflict(format!(
                    "加载来源 {} 已变化，请重新校验",
                    source.name
                )));
            }
        }
        if let Some(current) = self.loading_configuration()
            && current.sources == sources
            && current.preparation == plan.preparation
            && current.graph == plan.graph
            && current.executables == plan.executables
        {
            let current = current.clone();
            if self.state.loading.pending_edit.is_some() {
                let mut next = self.state.clone();
                next.loading.pending_edit = None;
                self.commit(next)?;
            }
            return Ok(current);
        }
        let revision = increment(candidate.expected_revision, "加载配置修订")?;
        let configuration = LoadingConfiguration {
            revision,
            sources,
            sources_sha256: plan.sources_sha256,
            source_root: candidate.source_root,
            preparation: plan.preparation,
            graph: plan.graph,
            executables: plan.executables,
        };
        let mut next = self.state.clone();
        next.loading.configuration = Some(configuration.clone());
        next.loading.pending_edit = None;
        let previous = self.loading_configuration();
        let mut bindings = self.loading_bindings.clone();
        bindings.retain(|id, binding| {
            configuration
                .executables
                .contains(&binding.activation.executable)
                && (!binding.ended
                    || previous.is_some_and(|previous| {
                        accepted_entry_config(&previous.graph, id)
                            == accepted_entry_config(&configuration.graph, id)
                    }))
        });
        self.commit(next)?;
        self.loading_bindings = bindings;
        // A late receipt for the previous desired revision cannot settle this one.
        self.loading_application = None;
        Ok(configuration)
    }

    pub fn allocate_loading_host_generation(&mut self) -> Result<u64, PluginError> {
        let mut next = self.state.clone();
        next.loading.host_generation = increment(next.loading.host_generation, "加载宿主代次")?;
        let generation = next.loading.host_generation;
        self.commit(next)?;
        self.loading_application = None;
        self.loading_bindings.clear();
        Ok(generation)
    }

    pub fn begin_loading_application(
        &mut self,
        host_generation: u64,
    ) -> Result<LoadingApplicationToken, PluginError> {
        Ok(self
            .begin_loading_application_with_action(host_generation, None)?
            .0)
    }

    pub(crate) fn begin_loading_application_with_action(
        &mut self,
        host_generation: u64,
        action: Option<LoadingInstanceChangeSpec<'_>>,
    ) -> Result<(LoadingApplicationToken, LoadingBindingsSnapshot), PluginError> {
        let revision = self
            .loading_configuration()
            .ok_or_else(|| conflict("尚无已接纳的加载配置"))?
            .revision;
        if host_generation == 0 || host_generation != self.state.loading.host_generation {
            return Err(conflict("加载宿主代次已失效"));
        }
        if matches!(
            self.loading_application,
            Some(LoadingApplication::Applying(_))
        ) {
            return Err(conflict("加载配置正在收敛"));
        }
        let previous = LoadingBindingsSnapshot(self.loading_bindings.clone());
        let mut next = self.state.clone();
        let mut bindings = self.loading_bindings.clone();
        let configuration = self.loading_configuration().expect("checked above");
        let executables = &configuration.executables;
        let mut selected = BTreeSet::new();
        let mut reactivated = BTreeSet::new();
        if let Some(spec) = &action {
            if let Some(carrier_id) = spec.entry_id {
                if spec.action != LoadingInstanceAction::Refresh {
                    return Err(conflict("仅强制更新操作可指定载体条目"));
                }
                let expected = carrier_forced_update_executables(&configuration.graph, carrier_id)
                    .ok_or_else(|| conflict("强制更新的结构载体不存在或已停用"))?;
                for target in spec.targets {
                    if !selected.insert(target.instance_id.as_str())
                        || !expected.contains(&target.instance_id)
                        || !bindings.get(&target.instance_id).is_some_and(|binding| {
                            !binding.ended
                                && binding.activation.activation_generation
                                    == target.activation_generation
                                && binding.activation.host_generation == host_generation
                                && executables.contains(&binding.activation.executable)
                        })
                    {
                        return Err(conflict("加载操作的实例、代次或生命周期已变化"));
                    }
                }
                for target in spec.activate_targets {
                    if selected.contains(target.instance_id.as_str())
                        || !reactivated.insert(target.instance_id.as_str())
                        || !expected.contains(&target.instance_id)
                        || !bindings.get(&target.instance_id).is_some_and(|binding| {
                            binding.ended
                                && binding.activation.activation_generation
                                    == target.activation_generation
                                && binding.activation.host_generation == host_generation
                                && executables.contains(&binding.activation.executable)
                        })
                    {
                        return Err(conflict("加载操作的实例、代次或生命周期已变化"));
                    }
                }
                if selected.len() + reactivated.len() != expected.len() {
                    return Err(conflict("结构载体强制更新缺少完整的目标实例集合"));
                }
            } else {
                if !spec.activate_targets.is_empty() || spec.targets.is_empty() {
                    return Err(conflict("加载操作缺少目标实例"));
                }
                for target in spec.targets {
                    if !selected.insert(target.instance_id.as_str())
                        || !bindings.get(&target.instance_id).is_some_and(|binding| {
                            (match spec.action {
                                LoadingInstanceAction::Activate => binding.ended,
                                LoadingInstanceAction::Stop => true,
                                LoadingInstanceAction::Refresh => !binding.ended,
                            }) && binding.activation.activation_generation
                                == target.activation_generation
                                && binding.activation.host_generation == host_generation
                                && executables.contains(&binding.activation.executable)
                        })
                    {
                        return Err(conflict("加载操作的实例、代次或生命周期已变化"));
                    }
                }
            }
        }
        for executable in executables {
            let restart = reactivated.contains(executable.instance_id.as_str());
            let targeted = selected.contains(executable.instance_id.as_str());
            if !restart
                && targeted
                && let Some(spec) = &action
            {
                match spec.action {
                    LoadingInstanceAction::Stop => {
                        let binding = bindings
                            .get_mut(&executable.instance_id)
                            .expect("validated target");
                        binding.scope = None;
                        binding.ended = true;
                        binding.status = LoadingInstanceStatus::Stopped;
                        continue;
                    }
                    LoadingInstanceAction::Refresh => continue,
                    LoadingInstanceAction::Activate => {}
                }
            }
            if !restart
                && !targeted
                && bindings
                    .get(&executable.instance_id)
                    .is_some_and(|binding| {
                        binding.activation.executable == *executable
                            && binding.activation.host_generation == host_generation
                    })
            {
                continue;
            }
            let generation = increment(
                *next
                    .loading
                    .instance_generations
                    .get(&executable.instance_id)
                    .unwrap_or(&0),
                "插件实例代次",
            )?;
            next.loading
                .instance_generations
                .insert(executable.instance_id.clone(), generation);
            let order = if let Some(order) = next.loading.plugin_order.get(&executable.plugin_id) {
                *order
            } else {
                next.loading.order_sequence =
                    increment(next.loading.order_sequence, "插件激活顺序")?;
                next.loading
                    .plugin_order
                    .insert(executable.plugin_id.clone(), next.loading.order_sequence);
                next.loading.order_sequence
            };
            bindings.insert(
                executable.instance_id.clone(),
                LoadingBinding {
                    activation: LoadingActivation {
                        executable: executable.clone(),
                        host_generation,
                        activation_generation: generation,
                        activation_order: order,
                    },
                    scope: None,
                    ended: false,
                    status: LoadingInstanceStatus::Starting,
                },
            );
        }
        next.loading.application_sequence =
            increment(next.loading.application_sequence, "加载操作序号")?;
        let token = LoadingApplicationToken {
            revision,
            host_generation,
            sequence: next.loading.application_sequence,
        };
        self.commit(next)?;
        self.loading_bindings = bindings;
        self.loading_application = Some(LoadingApplication::Applying(token.clone()));
        Ok((token, previous))
    }

    pub(crate) fn abort_loading_refresh(
        &mut self,
        token: &LoadingApplicationToken,
        previous: LoadingBindingsSnapshot,
    ) -> Result<Vec<LoadingActivation>, PluginError> {
        if self.loading_application.as_ref() != Some(&LoadingApplication::Applying(token.clone()))
            || self.state.loading.host_generation != token.host_generation
            || self
                .loading_configuration()
                .map(|configuration| configuration.revision)
                != Some(token.revision)
        {
            return Err(conflict("加载回执已失效"));
        }
        self.loading_bindings = previous.0;
        self.loading_application = Some(LoadingApplication::Applied(token.clone()));
        self.loading_configuration()
            .expect("checked above")
            .executables
            .iter()
            .map(|executable| {
                self.loading_bindings
                    .get(&executable.instance_id)
                    .map(|binding| binding.activation.clone())
                    .ok_or_else(|| conflict("加载实例已结束，请重新收敛"))
            })
            .collect()
    }

    pub fn loading_activations(
        &self,
        token: &LoadingApplicationToken,
    ) -> Result<Vec<LoadingActivation>, PluginError> {
        if self.loading_application.as_ref() != Some(&LoadingApplication::Applying(token.clone())) {
            return Err(conflict("加载操作已失效"));
        }
        self.loading_configuration()
            .expect("applying configuration")
            .executables
            .iter()
            .map(|executable| {
                self.loading_bindings
                    .get(&executable.instance_id)
                    .map(|binding| binding.activation.clone())
                    .ok_or_else(|| conflict("加载实例已结束，请重新收敛"))
            })
            .collect::<Result<Vec<_>, _>>()
    }

    /// Allocation is not readiness; runtime contributions still require their own live registration.
    pub fn loading_instance_current(&self, activation: &LoadingActivation) -> bool {
        self.loading_instance_binding(&activation.executable.instance_id) == Some(activation)
    }

    pub fn loading_instance_ended(&mut self, activation: &LoadingActivation) {
        if let Some(binding) = self
            .loading_bindings
            .get_mut(&activation.executable.instance_id)
            && binding.activation == *activation
        {
            // Retain the issued identity for graph convergence. An unrelated
            // revision must not authorize another startup of this entry.
            binding.scope = None;
            binding.ended = true;
            if !matches!(binding.status, LoadingInstanceStatus::Failed(_)) {
                binding.status = LoadingInstanceStatus::Stopped;
            }
        }
    }

    pub fn loading_instance_binding(&self, instance_id: &str) -> Option<&LoadingActivation> {
        self.loading_bindings
            .get(instance_id)
            .filter(|binding| !binding.ended)
            .map(|binding| &binding.activation)
    }

    /// Current daemon observation for an issued instance, including its terminal state.
    pub fn loading_instance_status(
        &self,
        instance_id: &str,
    ) -> Option<(&LoadingActivation, &LoadingInstanceStatus)> {
        self.loading_bindings
            .get(instance_id)
            .map(|binding| (&binding.activation, &binding.status))
    }

    pub(crate) fn observe_loading_scope(
        &mut self,
        activation: &LoadingActivation,
        status: &Value,
    ) -> Result<(), PluginError> {
        let Some(binding) = self
            .loading_bindings
            .get_mut(&activation.executable.instance_id)
            .filter(|binding| &binding.activation == activation)
        else {
            return Ok(());
        };
        let bad_scope = || PluginError::InvalidInput("加载实例状态缺少有效的执行作用域".into());
        let state = status["state"].as_str().ok_or_else(bad_scope)?;
        if !matches!(
            state,
            "starting" | "running" | "waiting" | "stopping" | "stopped" | "failed"
        ) {
            return Err(bad_scope());
        }
        let epoch = |key: &str| {
            status[key]
                .as_u64()
                .filter(|epoch| (1..=MAX_GENERATION).contains(epoch))
                .ok_or_else(bad_scope)
        };
        let configuration_epoch = epoch("configurationEpoch")?;
        let call_epoch = epoch("callEpoch")?;
        let error = || status["error"].as_str().map(str::to_owned);
        if binding.ended {
            // Cordis may report failure before resolving the exception. Only
            // enrich that terminal fact; later events cannot reopen the scope.
            if state == "failed"
                && let LoadingInstanceStatus::Failed(message @ None) = &mut binding.status
            {
                *message = error();
            }
            return Ok(());
        }
        let configuration_active = matches!(state, "starting" | "running");
        let activating = state == "starting" && status["activating"].as_bool().unwrap_or(false);
        let running = state == "running";
        let previous = binding.scope.as_ref();
        if previous.is_some_and(|previous| {
            configuration_epoch < previous.configuration_epoch
                || call_epoch < previous.call_epoch
                || (configuration_epoch == previous.configuration_epoch
                    && configuration_active
                    && !previous.configuration_active)
                || (call_epoch == previous.call_epoch && running && previous.scope_revoked)
        }) {
            return Ok(());
        }
        binding.status = match state {
            "waiting" => LoadingInstanceStatus::Waiting(
                status["missingServices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            "running" => LoadingInstanceStatus::Running,
            "stopping" => LoadingInstanceStatus::Stopping,
            "stopped" => LoadingInstanceStatus::Stopped,
            "failed" => LoadingInstanceStatus::Failed(error()),
            _ => LoadingInstanceStatus::Starting,
        };
        if matches!(state, "failed" | "stopped") {
            self.loading_instance_ended(activation);
            return Ok(());
        }
        let same_call = previous.filter(|previous| previous.call_epoch == call_epoch);
        binding.scope = Some(LoadingScope {
            configuration_epoch,
            configuration_active,
            activating,
            call_epoch,
            running: running.then(|| {
                same_call
                    .and_then(|scope| scope.running.clone())
                    .unwrap_or_else(|| Arc::new(tokio::sync::watch::channel(()).0))
            }),
            scope_revoked: !running
                && same_call.is_some_and(|scope| scope.running.is_some() || scope.scope_revoked),
        });
        Ok(())
    }

    pub fn loading_configuration_scope_current(&self, instance_id: &str, epoch: u64) -> bool {
        self.loading_bindings
            .get(instance_id)
            .and_then(|binding| binding.scope.as_ref())
            .is_some_and(|scope| scope.configuration_active && scope.configuration_epoch == epoch)
    }

    pub(crate) fn loading_call_scope(
        &self,
        activation: &LoadingActivation,
    ) -> Option<(u64, tokio::sync::watch::Receiver<()>)> {
        let binding = self
            .loading_bindings
            .get(&activation.executable.instance_id)
            .filter(|binding| &binding.activation == activation)?;
        let scope = binding.scope.as_ref()?;
        Some((scope.call_epoch, scope.running.as_ref()?.subscribe()))
    }

    /// Host loss invalidates issued instances, without changing the saved desired configuration.
    pub fn loading_host_ended(&mut self, host_generation: u64, reason: String) {
        if self.state.loading.host_generation != host_generation {
            return;
        }
        if self
            .last_loading_host_crash
            .as_ref()
            .is_none_or(|crash| crash.host_generation != host_generation)
        {
            let mut activating_instances = Vec::new();
            let mut active_instances = Vec::new();
            for binding in self.loading_bindings.values().filter(|b| !b.ended) {
                if let Some(scope) = &binding.scope {
                    if scope.activating {
                        activating_instances.push(binding.activation.clone());
                    } else if scope.running.is_some() {
                        active_instances.push(binding.activation.clone());
                    }
                }
            }
            self.last_loading_host_crash = Some(LoadingHostCrashSummary {
                host_generation,
                reason: reason.clone(),
                activating_instances,
                active_instances,
            });
        }
        self.loading_bindings.clear();
        if let Some(LoadingApplication::Applying(token) | LoadingApplication::Applied(token)) =
            &self.loading_application
        {
            self.loading_application = Some(LoadingApplication::Failed(token.clone(), reason));
        }
    }

    pub fn loading_host_crash(&self, host_generation: u64) -> Option<&LoadingHostCrashSummary> {
        self.last_loading_host_crash
            .as_ref()
            .filter(|summary| summary.host_generation == host_generation)
    }

    pub fn loading_executable_location(&self, instance_id: &str) -> Option<(String, String)> {
        let nodes = self.loading_configuration()?.graph["root"]["entries"].as_array()?;
        find_executable_location(nodes, "", instance_id)
    }

    pub fn finish_loading_application(
        &mut self,
        token: &LoadingApplicationToken,
        outcome: Result<(), String>,
    ) -> Result<(), PluginError> {
        if self.loading_application.as_ref() != Some(&LoadingApplication::Applying(token.clone()))
            || self.state.loading.host_generation != token.host_generation
            || self
                .loading_configuration()
                .map(|configuration| configuration.revision)
                != Some(token.revision)
        {
            return Err(conflict("加载回执已失效"));
        }
        if let Err(message) = &outcome
            && (message.is_empty() || message.len() > 4096)
        {
            return Err(PluginError::InvalidInput("加载失败诊断无效".into()));
        }
        self.loading_application = Some(match outcome {
            Ok(()) => LoadingApplication::Applied(token.clone()),
            Err(message) => LoadingApplication::Failed(token.clone(), message),
        });
        Ok(())
    }

    fn check_loading_revision(&self, expected: u64) -> Result<(), PluginError> {
        if self
            .loading_configuration()
            .map_or(0, |configuration| configuration.revision)
            != expected
        {
            return Err(conflict("加载配置已变化，请重新读取修订"));
        }
        Ok(())
    }
}

fn collect_group_executables(nodes: &[Value], out: &mut BTreeSet<String>) {
    for node in nodes {
        if let Some(instance_id) = node["instanceId"].as_str() {
            out.insert(instance_id.to_owned());
        } else if node.get("source").is_none()
            && node["options"]["group"].as_bool() == Some(true)
            && let Some(children) = node["children"].as_array()
        {
            collect_group_executables(children, out);
        }
    }
}

fn find_carrier_executables(
    nodes: &[Value],
    scope: &str,
    entry_id: &str,
) -> Option<BTreeSet<String>> {
    for node in nodes {
        let local_id = node["options"]["id"].as_str()?;
        let id = if scope.is_empty() {
            local_id.to_owned()
        } else {
            format!("{scope}:{local_id}")
        };
        if id == entry_id {
            if node.get("artifact").is_some() {
                return None;
            }
            let children = node["children"].as_array()?;
            let mut out = BTreeSet::new();
            if node.get("source").is_none() && node["options"]["group"].as_bool() == Some(true) {
                collect_group_executables(children, &mut out);
            }
            return Some(out);
        }
        if let Some(children) = node["children"].as_array() {
            let child_scope = if node.get("source").is_some() {
                id.as_str()
            } else {
                scope
            };
            if let Some(found) = find_carrier_executables(children, child_scope, entry_id) {
                return Some(found);
            }
        }
    }
    None
}

fn find_executable_location(
    nodes: &[Value],
    scope: &str,
    target_instance_id: &str,
) -> Option<(String, String)> {
    for node in nodes {
        let local_id = node["options"]["id"].as_str()?;
        if node["instanceId"].as_str() == Some(target_instance_id) {
            return Some((scope.to_owned(), local_id.to_owned()));
        }
        if let Some(children) = node["children"].as_array() {
            let id = if scope.is_empty() {
                local_id.to_owned()
            } else {
                format!("{scope}:{local_id}")
            };
            let child_scope = if node.get("source").is_some() {
                id.as_str()
            } else {
                scope
            };
            if let Some(found) = find_executable_location(children, child_scope, target_instance_id)
            {
                return Some(found);
            }
        }
    }
    None
}

fn carrier_forced_update_executables(graph: &Value, entry_id: &str) -> Option<BTreeSet<String>> {
    if entry_id.is_empty() || entry_id.len() > 4096 {
        return None;
    }
    find_carrier_executables(graph["root"]["entries"].as_array()?, "", entry_id)
}

fn increment(value: u64, kind: &str) -> Result<u64, PluginError> {
    value
        .checked_add(1)
        .filter(|value| *value <= MAX_GENERATION)
        .ok_or_else(|| conflict(format!("{kind}耗尽")))
}

fn sha256(content: &[u8]) -> String {
    magi_core::hex_lower(Sha256::digest(content))
}

fn valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', '\0'])
        && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && Path::new(name)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn capture_source(root: &Path, name: &str) -> Result<LoadingSource, PluginError> {
    if !valid_source_name(name) {
        return Err(PluginError::InvalidInput(
            "加载来源必须是项目内相对路径".into(),
        ));
    }
    let path = fs::canonicalize(root.join(name))?;
    if !path.starts_with(root) {
        return Err(PluginError::InvalidInput("加载来源越过项目目录".into()));
    }
    if !fs::metadata(&path)?.is_file() {
        return Err(PluginError::InvalidInput("加载来源必须是普通文件".into()));
    }
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(PluginError::InvalidInput("加载来源必须是普通文件".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(PluginError::InvalidInput("加载来源超过大小上限".into()));
    }
    let digest = sha256(&bytes);
    let content = String::from_utf8(bytes)
        .map_err(|_| PluginError::InvalidInput("加载来源必须是 UTF-8".into()))?;
    Ok(LoadingSource {
        name: name.into(),
        content,
        sha256: digest,
    })
}

fn validate_sources(sources: &[LoadingSource]) -> Result<(), PluginError> {
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    if sources.is_empty() || sources.len() > MAX_SOURCES {
        return Err(PluginError::InvalidInput("加载来源数量无效".into()));
    }
    for source in sources {
        total += source.content.len();
        if !valid_source_name(&source.name)
            || !names.insert(&source.name)
            || source.content.len() > MAX_SOURCE_BYTES
            || total > MAX_CONFIGURATION_BYTES
            || source.sha256 != sha256(source.content.as_bytes())
        {
            return Err(PluginError::InvalidInput(
                "加载来源重复、超限或摘要不一致".into(),
            ));
        }
    }
    Ok(())
}

fn sources_digest(sources: &[LoadingSource]) -> Result<String, PluginError> {
    serde_json::to_vec(sources)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| PluginError::CorruptState(error.to_string()))
}

fn select_plan_sources(
    captured: &[LoadingSource],
    plan: &LoadingPlan,
) -> Result<Vec<LoadingSource>, PluginError> {
    if plan.source_names.is_empty() || plan.source_names.len() > captured.len() {
        return Err(conflict("加载计划的来源数量无效"));
    }
    let sources = plan
        .source_names
        .iter()
        .map(|name| {
            captured
                .iter()
                .find(|source| source.name == *name)
                .cloned()
                .ok_or_else(|| conflict("加载计划引用了未捕获的来源"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_sources(&sources)?;
    if !sources.windows(2).all(|pair| pair[0].name < pair[1].name)
        || sources_digest(&sources)? != plan.sources_sha256
    {
        return Err(conflict("加载计划来源顺序或摘要不符"));
    }
    Ok(sources)
}

fn validate_graph(graph: &Value) -> Result<(), PluginError> {
    if !graph.is_object()
        || serde_json::to_vec(graph).map_or(true, |bytes| bytes.len() > MAX_CONFIGURATION_BYTES)
    {
        return Err(PluginError::InvalidInput("加载图必须是有界对象".into()));
    }
    Ok(())
}

fn validate_executables(executables: &[LoadingExecutable]) -> Result<(), PluginError> {
    let mut instances = BTreeSet::new();
    if executables.len() > 4096 {
        return Err(PluginError::InvalidInput("加载实例数量超限".into()));
    }
    for executable in executables {
        if executable.instance_id.is_empty()
            || executable.instance_id.len() > 4096
            || !instances.insert(&executable.instance_id)
            || executable.plugin_id.is_empty()
            || executable.plugin_id.len() > 214
            || semver::Version::parse(&executable.version).is_err()
            || !executable.entry.is_absolute()
            || executable.fingerprint.len() != 64
            || !executable
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(PluginError::InvalidInput(
                "加载实例身份、版本或指纹无效".into(),
            ));
        }
    }
    Ok(())
}

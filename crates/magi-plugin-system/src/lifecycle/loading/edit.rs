//! Candidate source writes are journaled by the same owner before any file is
//! replaced. A partially saved multi-source edit cannot be admitted as another graph.
use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingSourceEdit {
    pub name: String,
    pub expected_sha256: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoadingEditProposal {
    pub base_sources_sha256: String,
    pub edits: Vec<LoadingSourceEdit>,
    pub plan: LoadingPlan,
    pub result: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PendingLoadingEdit {
    pub(super) expected_revision: u64,
    pub(super) proposal: LoadingEditProposal,
    captured_sources: Vec<LoadingSource>,
}

impl PendingLoadingEdit {
    pub(super) fn validate(
        &self,
        configuration: Option<&LoadingConfiguration>,
    ) -> Result<(), PluginError> {
        if self.expected_revision != configuration.map_or(0, |current| current.revision)
            || configuration
                .is_some_and(|current| current.source_root != self.proposal.plan.source_root)
        {
            return Err(conflict("待恢复的加载编辑修订与原配置不符"));
        }
        self.expected_sources().map(|_| ())
    }

    pub(super) fn expected_sources(&self) -> Result<Vec<LoadingSource>, PluginError> {
        validate_proposal(&self.captured_sources, &self.proposal)
    }
}

fn validate_proposal(
    before: &[LoadingSource],
    proposal: &LoadingEditProposal,
) -> Result<Vec<LoadingSource>, PluginError> {
    validate_sources(before)?;
    if proposal.base_sources_sha256 != sources_digest(before)?
        || proposal.edits.is_empty()
        || proposal.edits.len() > MAX_SOURCES
        || serde_json::to_vec(&proposal.result).map_or(true, |bytes| bytes.len() > MAX_SOURCE_BYTES)
    {
        return Err(PluginError::InvalidInput(
            "加载编辑的来源、修订或结果无效".into(),
        ));
    }
    validate_graph(&proposal.plan.graph)?;
    validate_executables(&proposal.plan.executables)?;
    let sources = apply_source_edits(before, &proposal.edits)?;
    let selected = select_plan_sources(&sources, &proposal.plan)?;
    proposal.plan.preparation.validate(&selected)?;
    if proposal.edits.iter().any(|edit| {
        edit.expected_sha256.is_none() && !proposal.plan.source_names.contains(&edit.name)
    }) {
        return Err(conflict("初始文件不属于待接纳的加载图"));
    }
    Ok(sources)
}

pub(super) fn apply_source_edits(
    before: &[LoadingSource],
    edits: &[LoadingSourceEdit],
) -> Result<Vec<LoadingSource>, PluginError> {
    let mut sources = before.to_vec();
    let mut seen = BTreeSet::new();
    for edit in edits {
        if !seen.insert(&edit.name) {
            return Err(conflict("加载编辑重复或已失效"));
        }
        let existing = sources.iter().position(|source| source.name == edit.name);
        let replacement = LoadingSource {
            name: edit.name.clone(),
            content: edit.content.clone(),
            sha256: sha256(edit.content.as_bytes()),
        };
        match (existing, &edit.expected_sha256) {
            (Some(index), Some(expected)) if sources[index].sha256 == *expected => {
                sources[index] = replacement
            }
            (None, None) => sources.push(replacement),
            _ => return Err(conflict("加载编辑引用了未捕获或已变化的来源")),
        }
    }
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    validate_sources(&sources)?;
    Ok(sources)
}

pub(super) fn new_source_path(root: &Path, name: &str) -> Result<PathBuf, PluginError> {
    if !valid_source_name(name) {
        return Err(conflict("初始文件路径无效"));
    }
    let path = root.join(name);
    let parent = fs::canonicalize(path.parent().expect("source has parent"))?;
    if !parent.starts_with(root) {
        return Err(conflict("初始文件越过加载来源目录"));
    }
    Ok(parent.join(path.file_name().expect("source has filename")))
}

fn capture_optional_source(root: &Path, name: &str) -> Result<Option<LoadingSource>, PluginError> {
    match fs::symlink_metadata(root.join(name)) {
        Ok(_) => capture_source(root, name).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl PluginManager {
    pub fn pending_loading_edit(&self) -> Option<&LoadingEditProposal> {
        self.state
            .loading
            .pending_edit
            .as_ref()
            .map(|pending| &pending.proposal)
    }

    /// Record a validated edit intent before replacing any candidate source.
    /// No running configuration or live entry is changed by this operation.
    pub fn begin_loading_edit(
        &mut self,
        candidate: LoadingCandidate,
        proposal: LoadingEditProposal,
    ) -> Result<(), PluginError> {
        self.begin_edit(candidate, proposal, false)
    }

    pub(crate) fn begin_installation_edit(
        &mut self,
        candidate: LoadingCandidate,
        proposal: LoadingEditProposal,
    ) -> Result<(), PluginError> {
        self.begin_edit(candidate, proposal, true)
    }

    fn begin_edit(
        &mut self,
        candidate: LoadingCandidate,
        proposal: LoadingEditProposal,
        replacing_installation: bool,
    ) -> Result<(), PluginError> {
        let current = self
            .loading_configuration()
            .ok_or_else(|| conflict("加载编辑需要已接纳的配置"))?;
        if !replacing_installation && proposal.plan.preparation != current.preparation {
            return Err(conflict("配置编辑不能替换已接纳的安装项目"));
        }
        if !current
            .sources
            .iter()
            .all(|source| candidate.sources.contains(source))
            || proposal.edits.iter().any(|edit| {
                edit.expected_sha256.is_some()
                    && !current
                        .sources
                        .iter()
                        .any(|source| source.name == edit.name)
            })
        {
            return Err(conflict("配置文件已有未接纳变更，请先处理来源冲突"));
        }
        self.journal_edit(candidate, proposal)
    }

    pub(crate) fn begin_source_initialization(
        &mut self,
        candidate: LoadingCandidate,
        proposal: LoadingEditProposal,
    ) -> Result<(), PluginError> {
        if proposal
            .edits
            .iter()
            .any(|edit| edit.expected_sha256.is_some())
            || self
                .loading_configuration()
                .is_some_and(|current| current.preparation != proposal.plan.preparation)
        {
            return Err(conflict("初始化只能创建来源文件，不能修改安装项目"));
        }
        self.journal_edit(candidate, proposal)
    }

    fn journal_edit(
        &mut self,
        candidate: LoadingCandidate,
        proposal: LoadingEditProposal,
    ) -> Result<(), PluginError> {
        self.check_loading_revision(candidate.expected_revision)?;
        if self.state.loading.pending_edit.is_some() {
            return Err(conflict("加载来源有未完成的编辑事务"));
        }
        if fs::canonicalize(self.loading_source_root())? != candidate.source_root
            || proposal.plan.source_root != candidate.source_root
            || self
                .loading_configuration()
                .is_some_and(|current| current.source_root != candidate.source_root)
        {
            return Err(conflict("加载来源目录已经变化"));
        }
        validate_proposal(&candidate.sources, &proposal)?;
        for source in &candidate.sources {
            if capture_source(&candidate.source_root, &source.name)? != *source {
                return Err(conflict("配置文件在准备编辑时已变化"));
            }
        }
        for edit in proposal
            .edits
            .iter()
            .filter(|edit| edit.expected_sha256.is_none())
        {
            let target = new_source_path(&candidate.source_root, &edit.name)?;
            match fs::symlink_metadata(target) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => return Err(conflict("初始文件已由其他操作创建")),
            }
        }
        let mut next = self.state.clone();
        next.loading.pending_edit = Some(PendingLoadingEdit {
            expected_revision: candidate.expected_revision,
            proposal,
            captured_sources: candidate.sources,
        });
        self.commit(next)
    }

    /// Resume only known before/after source states. Unexpected external edits
    /// remain untouched and keep this transaction pending for explicit resolution.
    pub fn finish_loading_edit(&mut self) -> Result<LoadingConfiguration, PluginError> {
        let pending = self
            .state
            .loading
            .pending_edit
            .clone()
            .ok_or_else(|| conflict("没有待恢复的加载编辑"))?;
        self.check_loading_revision(pending.expected_revision)?;
        let desired = pending.expected_sources()?;
        let root = fs::canonicalize(self.loading_source_root())?;
        if root != pending.proposal.plan.source_root {
            return Err(conflict("加载来源目录已经变化"));
        }
        // Check the entire source set before the first write, including files
        // that the edit did not modify. Partial file saves are never execution facts.
        for after in &desired {
            let actual = capture_optional_source(&root, &after.name)?;
            let before = pending
                .captured_sources
                .iter()
                .find(|source| source.name == after.name);
            if actual.as_ref() != before && actual.as_ref() != Some(after) {
                return Err(conflict(format!(
                    "配置文件 {} 与编辑事务冲突；原文已保留",
                    after.name
                )));
            }
        }
        for edit in &pending.proposal.edits {
            let actual = capture_optional_source(&root, &edit.name)?;
            let after = desired
                .iter()
                .find(|source| source.name == edit.name)
                .expect("validated edit");
            if actual.as_ref() == Some(after) {
                continue;
            }
            if actual.as_ref().map(|source| source.sha256.as_str())
                != edit.expected_sha256.as_deref()
            {
                return Err(conflict("配置文件在保存编辑时已变化"));
            }
            if edit.expected_sha256.is_none() {
                let target = new_source_path(&root, &edit.name)?;
                magi_core::fs_atomic::write_atomic_create_new(&target, edit.content.as_bytes())?;
            } else {
                let target = fs::canonicalize(root.join(&edit.name))?;
                if !target.starts_with(&root) {
                    return Err(conflict("配置文件越过加载来源目录"));
                }
                magi_core::fs_atomic::write_atomic_preserving_target(
                    &target,
                    edit.content.as_bytes(),
                )?;
            }
        }
        let names = desired
            .into_iter()
            .map(|source| source.name)
            .collect::<Vec<_>>();
        let candidate = self.prepare_loading_configuration(pending.expected_revision, &names)?;
        // The ordinary CAS and file recheck are the only configuration commit.
        self.accept_loading_configuration(candidate, pending.proposal.plan)
    }
}

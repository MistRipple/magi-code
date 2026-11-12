use super::*;

#[derive(Clone, Default)]
pub struct GitService {
    pub(super) repository_mutexes: Arc<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>>,
}

#[derive(Debug)]
pub(super) struct CommandOutput {
    pub(super) success: bool,
    pub(super) code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

#[derive(Debug)]
pub(super) struct BinaryCommandOutput {
    pub(super) success: bool,
    pub(super) code: Option<i32>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: String,
}

impl GitService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn observe(&self, path: &Path) -> Result<GitObservation, GitError> {
        observe_unlocked(path).await
    }

    /// 读取当前 worktree 的 branch/HEAD 指纹。
    ///
    /// 该查询只执行一次 porcelain Git 命令，不扫描未跟踪文件，供变更面板高频判断
    /// repository 基线是否变化；只有指纹变化后才需要执行完整 `observe()`。
    pub async fn observe_ref(&self, path: &Path) -> Result<GitRefObservation, GitError> {
        let output = run_git(
            path,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=no",
            ],
        )
        .await?;
        let status = ensure_success("git_ref_observation", output)?;
        let mut branch = None;
        let mut head = None;
        for line in status.lines() {
            if let Some(value) = line.strip_prefix("# branch.head ") {
                branch = match value.trim() {
                    "(detached)" | "(unknown)" | "" => None,
                    value => Some(value.to_string()),
                };
            } else if let Some(value) = line.strip_prefix("# branch.oid ") {
                head = match value.trim() {
                    "(initial)" | "" => None,
                    value => Some(value.to_string()),
                };
            }
        }
        Ok(GitRefObservation { branch, head })
    }

    /// 读取两个不可变 Git tree 之间的 baseline 增量。
    ///
    /// rename 按删除旧路径 + 写入新路径展开，调用方可以直接把结果应用到独立的
    /// workspace snapshot baseline，而不会把当前 worktree 中仍未提交的内容误吞进去。
    pub async fn tree_baseline_patch(
        &self,
        worktree_path: &Path,
        previous_head: &str,
        current_head: &str,
    ) -> Result<Vec<GitTreeBaselineEntry>, GitError> {
        if previous_head.trim().is_empty() || current_head.trim().is_empty() {
            return Err(GitError::InvalidInput {
                message: "Git tree baseline patch requires non-empty revisions".to_string(),
            });
        }
        let output = run_git(
            worktree_path,
            &[
                "diff",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-renames",
                previous_head,
                current_head,
            ],
        )
        .await?;
        let raw = ensure_success("git_tree_baseline_diff", output)?;
        let fields = raw.split('\0').collect::<Vec<_>>();
        let mut entries = Vec::new();
        let mut index = 0usize;
        while index < fields.len() {
            let header = fields[index];
            index += 1;
            if header.is_empty() {
                continue;
            }
            let entry_path = fields.get(index).copied().unwrap_or_default();
            index += 1;
            if entry_path.is_empty() {
                return Err(GitError::Io(
                    "Git tree baseline diff returned an empty path".to_string(),
                ));
            }
            let mut parts = header.split_whitespace();
            let _previous_mode = parts.next();
            let current_mode = parts
                .next()
                .ok_or_else(|| GitError::Io(format!("invalid Git raw diff header: {header}")))?;
            let _previous_object = parts.next();
            let current_object = parts
                .next()
                .ok_or_else(|| GitError::Io(format!("invalid Git raw diff header: {header}")))?;
            let _status = parts.next();

            match current_mode {
                "000000" => entries.push(GitTreeBaselineEntry::Deleted {
                    path: entry_path.to_string(),
                }),
                "120000" => {
                    let target = read_git_object(worktree_path, current_object, entry_path).await?;
                    entries.push(GitTreeBaselineEntry::Symlink {
                        path: entry_path.to_string(),
                        target,
                    });
                }
                "160000" => entries.push(GitTreeBaselineEntry::Gitlink {
                    path: entry_path.to_string(),
                    object_id: current_object.to_string(),
                }),
                _ => {
                    let content =
                        read_git_object(worktree_path, current_object, entry_path).await?;
                    entries.push(GitTreeBaselineEntry::RegularFile {
                        path: entry_path.to_string(),
                        content,
                    });
                }
            }
        }
        Ok(entries)
    }

    /// 从指定 Git revision 读取一组路径的 baseline 内容。
    ///
    /// 用于 session 首次建立 Git context 或恢复丢失 context 时，把已经进入 HEAD 的
    /// pending 路径校准到提交树，同时保留 worktree 中仍不同于 HEAD 的内容。
    pub async fn tree_baseline_entries_for_paths(
        &self,
        worktree_path: &Path,
        revision: &str,
        paths: &[String],
    ) -> Result<Vec<GitTreeBaselineEntry>, GitError> {
        if revision.trim().is_empty() {
            return Err(GitError::InvalidInput {
                message: "Git tree baseline revision cannot be empty".to_string(),
            });
        }
        let mut entries = Vec::with_capacity(paths.len());
        for entry_path in paths {
            let output = run_git(
                worktree_path,
                &[
                    "ls-tree",
                    "-z",
                    "--full-tree",
                    revision,
                    "--",
                    entry_path.as_str(),
                ],
            )
            .await?;
            let tree_entry = ensure_success("git_tree_baseline_entry", output)?;
            if tree_entry.is_empty() {
                entries.push(GitTreeBaselineEntry::Deleted {
                    path: entry_path.clone(),
                });
                continue;
            }
            let header = tree_entry
                .split_once('\t')
                .map(|(header, _)| header)
                .ok_or_else(|| {
                    GitError::Io(format!("invalid Git ls-tree entry for {entry_path}"))
                })?;
            let mut parts = header.split_whitespace();
            let mode = parts
                .next()
                .ok_or_else(|| GitError::Io(format!("missing Git tree mode for {entry_path}")))?;
            let _object_type = parts.next();
            let object_id = parts
                .next()
                .ok_or_else(|| GitError::Io(format!("missing Git tree object for {entry_path}")))?;
            entries.push(
                self.read_tree_baseline_entry(worktree_path, entry_path, mode, object_id)
                    .await?,
            );
        }
        Ok(entries)
    }

    pub(super) async fn read_tree_baseline_entry(
        &self,
        worktree_path: &Path,
        entry_path: &str,
        mode: &str,
        object_id: &str,
    ) -> Result<GitTreeBaselineEntry, GitError> {
        match mode {
            "000000" => Ok(GitTreeBaselineEntry::Deleted {
                path: entry_path.to_string(),
            }),
            "120000" => Ok(GitTreeBaselineEntry::Symlink {
                path: entry_path.to_string(),
                target: read_git_object(worktree_path, object_id, entry_path).await?,
            }),
            "160000" => Ok(GitTreeBaselineEntry::Gitlink {
                path: entry_path.to_string(),
                object_id: object_id.to_string(),
            }),
            _ => Ok(GitTreeBaselineEntry::RegularFile {
                path: entry_path.to_string(),
                content: read_git_object(worktree_path, object_id, entry_path).await?,
            }),
        }
    }

    /// 判断当前观测是否只是 session 原 Git 基线在同一工作树、同一分支上的线性快进。
    /// 分支切换、工作树变化、HEAD 回退和历史改写均返回 `false`。
    pub async fn is_session_context_fast_forward(
        &self,
        context: &SessionCodeContext,
        observation: &GitObservation,
    ) -> Result<bool, GitError> {
        if !same_path(&context.git.repository_root, &observation.repository_root)
            || !same_path(&context.git.git_common_dir, &observation.git_common_dir)
            || !same_path(&context.git.worktree_path, &observation.worktree_path)
            || !same_path(&context.execution_root, &observation.worktree_path)
            || context.git.desired_ref.is_none()
            || context.git.desired_ref != observation.branch
        {
            return Ok(false);
        }
        let (Some(previous_head), Some(current_head)) = (
            context.git.base_head.as_deref(),
            observation.head.as_deref(),
        ) else {
            return Ok(false);
        };
        if previous_head == current_head {
            return Ok(false);
        }
        is_ancestor(&observation.worktree_path, previous_head, current_head).await
    }

    pub async fn branch_list(
        &self,
        path: &Path,
        include_remote: bool,
    ) -> Result<GitBranchList, GitError> {
        let observation = observe_unlocked(path).await?;
        let worktrees = worktree_list_unlocked(&observation.repository_root).await?;
        let mut args = vec![
            "for-each-ref",
            "--format=%(refname)%00%(refname:short)%00%(objectname)%00%(upstream:short)",
            "refs/heads",
        ];
        if include_remote {
            args.push("refs/remotes");
        }
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("branch_list", output).map(|stdout| {
            let mut branches = stdout
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split('\0');
                    let full_ref = fields.next()?.trim().to_string();
                    let name = fields.next()?.trim().to_string();
                    if name.is_empty() || name.ends_with("/HEAD") {
                        return None;
                    }
                    let head = non_empty(fields.next().unwrap_or_default());
                    let upstream = non_empty(fields.next().unwrap_or_default());
                    let is_remote = full_ref.starts_with("refs/remotes/");
                    let worktree_path = worktrees.iter().find_map(|worktree| {
                        (worktree.branch.as_deref() == Some(name.as_str()))
                            .then(|| worktree.path.clone())
                    });
                    Some(GitBranch {
                        is_current: !is_remote
                            && observation.branch.as_deref() == Some(name.as_str()),
                        name,
                        full_ref,
                        is_remote,
                        head,
                        upstream,
                        worktree_path,
                    })
                })
                .collect::<Vec<_>>();
            branches.sort_by(|left, right| {
                left.is_remote
                    .cmp(&right.is_remote)
                    .then_with(|| left.name.cmp(&right.name))
            });
            GitBranchList {
                observation,
                branches,
            }
        })
    }

    pub async fn branch_create(
        &self,
        path: &Path,
        options: BranchCreateOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        validate_branch_name(&observation.worktree_path, &options.branch).await?;
        let start_point = resolve_revision(
            &observation.worktree_path,
            options.start_point.as_deref().unwrap_or("HEAD"),
        )
        .await?;
        let args = if options.switch {
            vec![
                "switch",
                "-c",
                options.branch.as_str(),
                start_point.as_str(),
            ]
        } else {
            vec!["branch", options.branch.as_str(), start_point.as_str()]
        };
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("branch_create", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn branch_switch(
        &self,
        path: &Path,
        options: BranchSwitchOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        validate_local_branch_exists(&observation.worktree_path, &options.branch).await?;
        let output = run_git(
            &observation.worktree_path,
            &["switch", "--", options.branch.as_str()],
        )
        .await?;
        ensure_success("branch_switch", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn merge_preview(
        &self,
        path: &Path,
        target: &str,
        precondition: &GitPrecondition,
    ) -> Result<GitMergePreview, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, precondition)?;
        require_clean(&observation, "预览合并")?;
        let head = observation
            .head
            .as_deref()
            .ok_or_else(|| GitError::InvalidInput {
                message: "当前仓库没有可合并的 HEAD".to_string(),
            })?;
        let target_head = resolve_revision(&observation.worktree_path, target).await?;
        let merge_base_output = run_git(
            &observation.worktree_path,
            &["merge-base", head, target_head.as_str()],
        )
        .await?;
        let merge_base = merge_base_output
            .success
            .then(|| non_empty(&merge_base_output.stdout))
            .flatten();
        let fast_forward = is_ancestor(&observation.worktree_path, head, &target_head).await?;
        let already_up_to_date =
            is_ancestor(&observation.worktree_path, &target_head, head).await?;
        let incoming_commit_count = count_revisions(
            &observation.worktree_path,
            &format!("{head}..{target_head}"),
        )
        .await?;
        let diff_output = run_git(
            &observation.worktree_path,
            &["diff", "--name-only", "-z", head, target_head.as_str()],
        )
        .await?;
        let changed_paths = ensure_success("merge_preview_diff", diff_output)?
            .split('\0')
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        Ok(GitMergePreview {
            observation,
            target: target.to_string(),
            target_head,
            merge_base,
            fast_forward,
            already_up_to_date,
            incoming_commit_count,
            changed_paths,
        })
    }

    pub async fn merge(
        &self,
        path: &Path,
        options: MergeOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "合并分支")?;
        let target_head = resolve_revision(&observation.worktree_path, &options.target).await?;
        let mut args = vec!["merge", "--no-edit"];
        if options.ff_only {
            args.push("--ff-only");
        }
        args.push(target_head.as_str());
        let output = run_git(&observation.worktree_path, &args).await?;
        if !output.success {
            let after = observe_unlocked(&observation.worktree_path).await?;
            if !after.dirty.conflicted_paths.is_empty() {
                return Err(GitError::MergeConflict {
                    target: options.target,
                    conflicted_paths: after.dirty.conflicted_paths,
                    stdout: output.stdout,
                    stderr: output.stderr,
                });
            }
            return Err(command_failed("merge", output));
        }
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn pull(
        &self,
        path: &Path,
        options: PullOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        require_clean(&observation, "拉取远程更新")?;
        let current_branch =
            observation
                .branch
                .as_deref()
                .ok_or_else(|| GitError::InvalidInput {
                    message: "detached HEAD 不能执行 pull".to_string(),
                })?;
        let configured = configured_upstream(&observation.worktree_path, current_branch).await?;
        let remote = options
            .remote
            .or_else(|| configured.as_ref().map(|(remote, _)| remote.clone()))
            .ok_or_else(|| GitError::InvalidInput {
                message: "当前分支没有 upstream，必须提供 remote".to_string(),
            })?;
        let branch = options
            .branch
            .or_else(|| configured.map(|(_, branch)| branch))
            .unwrap_or_else(|| current_branch.to_string());
        validate_remote_name(&observation.worktree_path, &remote).await?;
        validate_branch_name(&observation.worktree_path, &branch).await?;

        let mut args = vec!["pull"];
        if options.ff_only {
            args.push("--ff-only");
        } else {
            args.extend(["--no-rebase", "--no-edit"]);
        }
        args.extend([remote.as_str(), branch.as_str()]);
        let output = run_git(&observation.worktree_path, &args).await?;
        if !output.success {
            let after = observe_unlocked(&observation.worktree_path).await?;
            if !after.dirty.conflicted_paths.is_empty() {
                return Err(GitError::MergeConflict {
                    target: format!("{remote}/{branch}"),
                    conflicted_paths: after.dirty.conflicted_paths,
                    stdout: output.stdout,
                    stderr: output.stderr,
                });
            }
            return Err(command_failed("pull", output));
        }
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn push(
        &self,
        path: &Path,
        options: PushOptions,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if options.force_with_lease && !options.confirm_force {
            return Err(GitError::ConfirmationRequired {
                operation: "使用 --force-with-lease 推送分支".to_string(),
            });
        }
        let current_branch =
            observation
                .branch
                .as_deref()
                .ok_or_else(|| GitError::InvalidInput {
                    message: "detached HEAD 不能执行 push".to_string(),
                })?;
        let branch = options.branch.unwrap_or_else(|| current_branch.to_string());
        validate_local_branch_exists(&observation.worktree_path, &branch).await?;
        let configured = configured_upstream(&observation.worktree_path, &branch).await?;
        let remote = options
            .remote
            .or_else(|| configured.map(|(remote, _)| remote))
            .ok_or_else(|| GitError::InvalidInput {
                message: "当前分支没有 upstream，必须提供 remote".to_string(),
            })?;
        validate_remote_name(&observation.worktree_path, &remote).await?;

        let mut args = vec!["push"];
        if options.set_upstream {
            args.push("--set-upstream");
        }
        if options.force_with_lease {
            args.push("--force-with-lease");
        }
        args.extend([remote.as_str(), branch.as_str()]);
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("push", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn branch_delete(
        &self,
        path: &Path,
        options: BranchDeleteOptions,
    ) -> Result<GitObservation, GitError> {
        self.branch_delete_internal(path, options, true).await
    }

    /// 删除已经应用到主工作树的代理分支。应用后的工作树通常有未提交改动，
    /// 因此这里只跳过 clean-worktree 检查，保留仓库身份、HEAD、当前分支及
    /// worktree 占用校验；删除分支引用本身不会改动主工作树文件。
    pub async fn branch_delete_after_revision_apply(
        &self,
        path: &Path,
        options: BranchDeleteOptions,
    ) -> Result<GitObservation, GitError> {
        self.branch_delete_internal(path, options, false).await
    }

    pub(super) async fn branch_delete_internal(
        &self,
        path: &Path,
        options: BranchDeleteOptions,
        require_clean_worktree: bool,
    ) -> Result<GitObservation, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if require_clean_worktree {
            require_clean(&observation, "删除分支")?;
        }
        validate_branch_name(&observation.worktree_path, &options.branch).await?;
        if let Some(remote) = options.remote.as_deref() {
            if !options.confirm_remote {
                return Err(GitError::ConfirmationRequired {
                    operation: format!("删除远程分支 {remote}/{}", options.branch),
                });
            }
            validate_remote_name(&observation.worktree_path, remote).await?;
            let output = run_git(
                &observation.worktree_path,
                &["push", remote, "--delete", options.branch.as_str()],
            )
            .await?;
            ensure_success("branch_delete_remote", output)?;
            return observe_unlocked(&observation.worktree_path).await;
        }
        if observation.branch.as_deref() == Some(options.branch.as_str()) {
            return Err(GitError::CurrentBranch {
                branch: options.branch,
            });
        }
        if options.force && !options.confirm_force {
            return Err(GitError::ConfirmationRequired {
                operation: format!("强制删除本地分支 {}", options.branch),
            });
        }
        let worktree_paths = worktree_list_unlocked(&observation.repository_root)
            .await?
            .into_iter()
            .filter_map(|worktree| {
                (worktree.branch.as_deref() == Some(options.branch.as_str()))
                    .then_some(worktree.path)
            })
            .collect::<Vec<_>>();
        if !worktree_paths.is_empty() {
            return Err(GitError::BranchInUse {
                branch: options.branch,
                worktree_paths,
            });
        }
        let delete_flag = if options.force { "-D" } else { "-d" };
        let output = run_git(
            &observation.worktree_path,
            &["branch", delete_flag, options.branch.as_str()],
        )
        .await?;
        ensure_success("branch_delete_local", output)?;
        observe_unlocked(&observation.worktree_path).await
    }

    pub async fn worktree_list(&self, path: &Path) -> Result<Vec<GitWorktree>, GitError> {
        let observation = observe_unlocked(path).await?;
        worktree_list_unlocked(&observation.repository_root).await
    }

    /// 恢复代理只能使用原 worktree 或原分支的已记录结果，不从当前主线生成
    /// 新快照。保留崩溃时的未提交文件/index；目录被替换、分支改名或基线漂移
    /// 都拒绝。正常清理已移除目录时，只从持久结果 commit 重建。
    pub async fn recover_agent_worktree(
        &self,
        repository: &Path,
        expected_common_dir: &Path,
        allocation: &AgentWorktreeContext,
    ) -> Result<(), GitError> {
        let (_guard, source) = self.lock_and_observe(repository).await?;
        let invalid = |message: &str| GitError::InvalidInput {
            message: message.into(),
        };
        if allocation.active
            || allocation.applied
            || !allocation.path.is_absolute()
            || (allocation.mode == AgentWorktreeMode::Writable) != allocation.branch.is_some()
            || allocation.base_head.trim().is_empty()
            || !same_path(&source.git_common_dir, expected_common_dir)
        {
            return Err(invalid("代理恢复来源仍占用、已采用或 repository 已变化"));
        }
        if let Some(branch) = allocation.branch.as_deref() {
            validate_branch_name(&source.worktree_path, branch).await?;
        }
        let base = resolve_revision(&source.worktree_path, &allocation.base_head).await?;
        if !allocation.path.exists() {
            let expected_head = allocation.result_head.as_deref().ok_or_else(|| {
                invalid("原代理目录缺失且没有持久结果 commit，不能猜测已保留全部产出")
            })?;
            let head = if let Some(branch) = &allocation.branch {
                resolve_revision(&source.worktree_path, &format!("refs/heads/{branch}")).await?
            } else {
                base.clone()
            };
            if head != expected_head {
                return Err(invalid("代理原分支 HEAD 与持久结果不一致，拒绝重建"));
            }
            ensure_success(
                "agent_worktree_recovery_ancestry",
                run_git(
                    &source.worktree_path,
                    &["merge-base", "--is-ancestor", &base, &head],
                )
                .await?,
            )?;
            // 不用 --force，不删除残留路径；Git 自身继续拒绝被其他 worktree
            // 占用的分支和未清理的注册记录。
            let destination = allocation.path.to_string_lossy();
            let mut args = vec!["worktree", "add"];
            if allocation.branch.is_none() {
                args.push("--detach");
            }
            args.push(destination.as_ref());
            args.push(allocation.branch.as_deref().unwrap_or(&base));
            ensure_success(
                "agent_worktree_recover",
                run_git(&source.worktree_path, &args).await?,
            )?;
        }
        if std::fs::symlink_metadata(&allocation.path)
            .map_err(|error| GitError::Io(error.to_string()))?
            .file_type()
            .is_symlink()
        {
            return Err(invalid("代理 worktree 目录被替换为符号链接，拒绝恢复"));
        }
        let observed = observe_unlocked(&allocation.path).await?;
        if !same_path(&observed.git_common_dir, expected_common_dir)
            || !same_path(&observed.worktree_path, &allocation.path)
            || observed.branch != allocation.branch
            || (allocation.branch.is_none() && observed.head.as_deref() != Some(base.as_str()))
        {
            return Err(invalid("代理原目录的 repository、worktree 或分支已变化"));
        }
        ensure_success(
            "agent_worktree_recovery_ancestry",
            run_git(
                &allocation.path,
                &["merge-base", "--is-ancestor", &base, "HEAD"],
            )
            .await?,
        )?;
        Ok(())
    }

    /// 把 worktree 的当前内容（含未提交和未忽略的未跟踪文件）写成以 `base` 为父的快照提交。
    ///
    /// 使用独立的临时 index，不改动调用方的 index、工作树和任何 ref。工作树与 `base`
    /// 一致时直接返回 `base` 的 commit id。
    pub async fn snapshot_worktree(
        &self,
        worktree_path: &Path,
        base: &str,
    ) -> Result<String, GitError> {
        let base = resolve_revision(worktree_path, base).await?;
        let index_path = std::env::temp_dir().join(format!(
            "magi-snapshot-index-{}-{}",
            std::process::id(),
            SNAPSHOT_INDEX_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let index_env = [("GIT_INDEX_FILE", index_path.as_os_str())];
        let result = async {
            ensure_success(
                "snapshot_read_tree",
                run_git_with_env(worktree_path, &["read-tree", base.as_str()], &index_env).await?,
            )?;
            ensure_success(
                "snapshot_add",
                run_git_with_env(worktree_path, &["add", "-A"], &index_env).await?,
            )?;
            let tree = ensure_success(
                "snapshot_write_tree",
                run_git_with_env(worktree_path, &["write-tree"], &index_env).await?,
            )?;
            let base_tree = required_text(
                worktree_path,
                &["rev-parse", "--verify", &format!("{base}^{{tree}}")],
            )
            .await?;
            if tree.trim() == base_tree.trim() {
                return Ok(base.clone());
            }
            let identity = runtime_commit_identity();
            ensure_success(
                "snapshot_commit_tree",
                run_git_with_env(
                    worktree_path,
                    &[
                        "commit-tree",
                        tree.trim(),
                        "-p",
                        base.as_str(),
                        "-m",
                        "magi: 子代理基线快照",
                    ],
                    &identity,
                )
                .await?,
            )
            .map(|commit| commit.trim().to_string())
        }
        .await;
        let _ = std::fs::remove_file(&index_path);
        result
    }

    /// 在代理 worktree 中提交全部改动，返回新的 HEAD；没有改动时返回 `None`。
    ///
    /// 这是运行时为保存代理产出做的检查点提交，不运行用户的 Git hook。
    pub async fn commit_all(
        &self,
        worktree_path: &Path,
        message: &str,
    ) -> Result<Option<String>, GitError> {
        ensure_success(
            "commit_all_add",
            run_git(worktree_path, &["add", "-A"]).await?,
        )?;
        let staged = run_git(worktree_path, &["diff", "--cached", "--quiet"]).await?;
        if staged.success {
            return Ok(None);
        }
        ensure_success(
            "commit_all",
            run_git_with_env(
                worktree_path,
                &["commit", "--no-verify", "-q", "-m", message],
                &runtime_commit_identity(),
            )
            .await?,
        )?;
        rev_parse(worktree_path, "HEAD").await.map(Some)
    }

    /// `from..to` 之间改动的路径（重命名按新旧两个路径展开）。
    pub async fn changed_paths(
        &self,
        path: &Path,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let from = resolve_revision(path, from).await?;
        let to = resolve_revision(path, to).await?;
        let output = ensure_success(
            "changed_paths",
            run_git(
                path,
                &["diff", "--name-only", "--no-renames", "-z", &from, &to],
            )
            .await?,
        )?;
        Ok(output
            .split('\0')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// 把 `base..head` 的改动应用到目标工作树（不改 index 和 ref）。
    ///
    /// 先整体检查，任一文件无法干净应用就返回 `ApplyConflict`，目标工作树保持原样。
    pub async fn apply_revision_diff(
        &self,
        target_worktree: &Path,
        base: &str,
        head: &str,
    ) -> Result<Vec<String>, GitError> {
        let (_guard, observation) = self.lock_and_observe(target_worktree).await?;
        let base = resolve_revision(&observation.worktree_path, base).await?;
        let head = resolve_revision(&observation.worktree_path, head).await?;
        let diff = run_git_bytes(
            &observation.worktree_path,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-renames",
                &base,
                &head,
            ],
        )
        .await?;
        if !diff.success {
            return Err(GitError::CommandFailed {
                operation: "apply_revision_diff_export".to_string(),
                exit_code: diff.code,
                stdout: String::new(),
                stderr: diff.stderr,
            });
        }
        if diff.stdout.is_empty() {
            return Ok(Vec::new());
        }
        let patch_path = std::env::temp_dir().join(format!(
            "magi-agent-apply-{}-{}.patch",
            std::process::id(),
            SNAPSHOT_INDEX_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&patch_path, &diff.stdout)
            .map_err(|error| GitError::Io(format!("写入代理改动补丁失败: {error}")))?;
        let patch = patch_path.to_string_lossy().to_string();
        let result = async {
            let check = run_git(&observation.worktree_path, &["apply", "--check", &patch]).await?;
            if !check.success {
                return Err(GitError::ApplyConflict {
                    conflicted_paths: apply_error_paths(&check.stderr),
                    stderr: check.stderr,
                });
            }
            ensure_success(
                "apply_revision_diff",
                run_git(&observation.worktree_path, &["apply", &patch]).await?,
            )?;
            Ok(())
        }
        .await;
        let _ = std::fs::remove_file(&patch_path);
        result?;
        self.changed_paths(&observation.worktree_path, &base, &head)
            .await
    }

    pub async fn worktree_create(
        &self,
        path: &Path,
        options: WorktreeCreateOptions,
    ) -> Result<GitWorktree, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if !options.path.is_absolute() {
            return Err(GitError::InvalidInput {
                message: "worktree path 必须是绝对路径".to_string(),
            });
        }
        if options.path.exists() {
            return Err(GitError::InvalidInput {
                message: format!("worktree path 已存在: {}", options.path.display()),
            });
        }
        if options.detached && (options.branch.is_some() || options.create_branch) {
            return Err(GitError::InvalidInput {
                message: "detached worktree 不能同时绑定或创建分支".to_string(),
            });
        }
        if options.create_branch && options.branch.is_none() {
            return Err(GitError::InvalidInput {
                message: "createBranch=true 时必须提供 branch".to_string(),
            });
        }
        if let Some(branch) = options.branch.as_deref() {
            validate_branch_name(&observation.worktree_path, branch).await?;
        }
        let base = resolve_revision(&observation.worktree_path, &options.base).await?;
        let destination = options.path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "add"];
        if options.detached {
            args.push("--detach");
        } else if options.create_branch {
            args.push("-b");
            args.push(options.branch.as_deref().unwrap_or_default());
        }
        args.push(destination.as_str());
        if options.detached || options.create_branch {
            args.push(base.as_str());
        } else if let Some(branch) = options.branch.as_deref() {
            validate_local_branch_exists(&observation.worktree_path, branch).await?;
            args.push(branch);
        } else {
            args.push(base.as_str());
        }
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("worktree_create", output)?;
        worktree_list_unlocked(&observation.repository_root)
            .await?
            .into_iter()
            .find(|worktree| same_path(&worktree.path, &options.path))
            .ok_or_else(|| GitError::Io("Git 已创建 worktree，但无法重新定位它".to_string()))
    }

    pub async fn worktree_remove(
        &self,
        path: &Path,
        options: WorktreeRemoveOptions,
    ) -> Result<Vec<GitWorktree>, GitError> {
        let (_guard, observation) = self.lock_and_observe(path).await?;
        enforce_precondition(&observation, &options.precondition)?;
        if same_path(&observation.worktree_path, &options.path) {
            return Err(GitError::InvalidInput {
                message: "不能从当前 worktree 内删除自身".to_string(),
            });
        }
        let worktrees = worktree_list_unlocked(&observation.repository_root).await?;
        if !worktrees
            .iter()
            .any(|worktree| same_path(&worktree.path, &options.path))
        {
            return Err(GitError::InvalidInput {
                message: "目标路径不是当前 repository 管理的 worktree".to_string(),
            });
        }
        if options.force && !options.confirm_force {
            return Err(GitError::ConfirmationRequired {
                operation: format!("强制移除 worktree {}", options.path.display()),
            });
        }
        let destination = options.path.to_string_lossy().to_string();
        let mut args = vec!["worktree", "remove"];
        if options.force {
            args.push("--force");
        }
        args.push(destination.as_str());
        let output = run_git(&observation.worktree_path, &args).await?;
        ensure_success("worktree_remove", output)?;
        worktree_list_unlocked(&observation.repository_root).await
    }

    pub(super) async fn lock_and_observe(
        &self,
        path: &Path,
    ) -> Result<(OwnedMutexGuard<()>, GitObservation), GitError> {
        let initial = observe_unlocked(path).await?;
        let mutex = {
            let mut mutexes = self
                .repository_mutexes
                .lock()
                .map_err(|error| GitError::Io(format!("Git mutex poisoned: {error}")))?;
            mutexes
                .entry(initial.git_common_dir.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let guard = mutex.lock_owned().await;
        let actual = observe_unlocked(path).await?;
        Ok((guard, actual))
    }
}

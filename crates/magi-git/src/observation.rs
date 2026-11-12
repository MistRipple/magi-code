use super::*;

pub(super) async fn observe_unlocked(path: &Path) -> Result<GitObservation, GitError> {
    let repository_root = required_path(path, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|error| match error {
            GitError::CommandFailed { .. } => GitError::NotRepository {
                path: path.to_path_buf(),
            },
            other => other,
        })?;
    let worktree_path = canonical_or_original(&repository_root);
    // 这些查询只读取 Git 状态，彼此没有数据依赖；并行执行可以减少每轮任务
    // preparation 中 Git context 的固定进程启动开销。最终仍由同一个
    // `GitObservation` 作为本轮事实快照，调用方继续按原有 branch/HEAD/worktree
    // precondition 做一致性校验。
    let (git_common_dir, worktree_git_dir) = tokio::join!(
        required_text(&worktree_path, &["rev-parse", "--git-common-dir"]),
        required_text(&worktree_path, &["rev-parse", "--git-dir"]),
    );
    let git_common_dir = resolve_git_path(&worktree_path, &git_common_dir?);
    let worktree_git_dir = resolve_git_path(&worktree_path, &worktree_git_dir?);

    let (branch, head, upstream, origin_url) = tokio::join!(
        optional_text(
            &worktree_path,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        ),
        optional_text(&worktree_path, &["rev-parse", "--verify", "HEAD"]),
        optional_text(
            &worktree_path,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        ),
        optional_text(&worktree_path, &["remote", "get-url", "origin"]),
    );
    let branch = branch?;
    let head = head?;
    let upstream = upstream?;
    let origin_url = origin_url?;
    let ahead_behind_future = async {
        if upstream.is_some() {
            ahead_behind(&worktree_path).await
        } else {
            Ok((0, 0))
        }
    };
    let (ahead_behind_result, dirty) = tokio::join!(
        ahead_behind_future,
        // dirty_summary 内部仍按原顺序完成 status + diff，避免改变 additions /
        // deletions 的统计口径。
        dirty_summary(&worktree_path, head.is_some()),
    );
    let (ahead, behind) = ahead_behind_result?;
    let dirty = dirty?;
    Ok(GitObservation {
        repository_root: worktree_path.clone(),
        git_common_dir: canonical_or_original(&git_common_dir),
        worktree_path,
        worktree_git_dir: canonical_or_original(&worktree_git_dir),
        branch,
        head,
        upstream,
        origin_url,
        ahead,
        behind,
        dirty,
    })
}

pub(super) async fn dirty_summary(
    path: &Path,
    has_head: bool,
) -> Result<GitDirtySummary, GitError> {
    let status_output = run_git(path, &["status", "--porcelain=v1", "-z"]).await?;
    let status_text = ensure_success("git_status", status_output)?;
    let mut summary = GitDirtySummary::default();
    let entries = status_text.split('\0').collect::<Vec<_>>();
    let mut index = 0usize;
    while index < entries.len() {
        let entry = entries[index];
        index += 1;
        if entry.len() < 3 {
            continue;
        }
        let bytes = entry.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        if (x, y) == ('?', '?') {
            summary.untracked += 1;
            continue;
        }
        if (x, y) == ('!', '!') {
            continue;
        }
        if x != ' ' {
            summary.staged += 1;
        }
        if y != ' ' {
            summary.unstaged += 1;
        }
        if is_conflicted_status(x, y) {
            summary.conflicted += 1;
            summary.conflicted_paths.push(entry[3..].to_string());
        }
        if x == 'R' || y == 'R' {
            summary.renamed += 1;
            index = index.saturating_add(1);
        }
        if x == 'D' || y == 'D' {
            summary.deleted += 1;
        }
    }
    if has_head {
        let diff_output = run_git(path, &["diff", "HEAD", "--numstat"]).await?;
        let diff_text = ensure_success("git_diff_stat", diff_output)?;
        for line in diff_text.lines() {
            let mut fields = line.split('\t');
            summary.additions += fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            summary.deletions += fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    summary.conflicted_paths.sort();
    summary.conflicted_paths.dedup();
    summary.has_uncommitted = summary.staged > 0
        || summary.unstaged > 0
        || summary.untracked > 0
        || summary.conflicted > 0;
    Ok(summary)
}

pub(super) async fn worktree_list_unlocked(path: &Path) -> Result<Vec<GitWorktree>, GitError> {
    let output = run_git(path, &["worktree", "list", "--porcelain", "-z"]).await?;
    let text = ensure_success("worktree_list", output)?;
    let mut result = Vec::new();
    let mut current: Option<GitWorktree> = None;
    for field in text.split('\0') {
        if field.is_empty() {
            continue;
        }
        if let Some(value) = field.strip_prefix("worktree ") {
            if let Some(worktree) = current.take() {
                result.push(worktree);
            }
            current = Some(GitWorktree {
                path: PathBuf::from(value),
                head: None,
                branch: None,
                bare: false,
                detached: false,
                locked: false,
                prunable: false,
            });
        } else if let Some(worktree) = current.as_mut() {
            if let Some(value) = field.strip_prefix("HEAD ") {
                worktree.head = non_empty(value);
            } else if let Some(value) = field.strip_prefix("branch refs/heads/") {
                worktree.branch = non_empty(value);
            } else if field == "bare" {
                worktree.bare = true;
            } else if field == "detached" {
                worktree.detached = true;
            } else if field.starts_with("locked") {
                worktree.locked = true;
            } else if field.starts_with("prunable") {
                worktree.prunable = true;
            }
        }
    }
    if let Some(worktree) = current {
        result.push(worktree);
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

pub(super) async fn validate_branch_name(path: &Path, branch: &str) -> Result<(), GitError> {
    if branch.trim().is_empty() || branch != branch.trim() {
        return Err(GitError::InvalidInput {
            message: "分支名不能为空或包含首尾空白".to_string(),
        });
    }
    let output = run_git(path, &["check-ref-format", "--branch", branch]).await?;
    if output.success {
        Ok(())
    } else {
        Err(GitError::InvalidInput {
            message: format!("非法分支名: {branch}"),
        })
    }
}

pub(super) async fn validate_local_branch_exists(
    path: &Path,
    branch: &str,
) -> Result<(), GitError> {
    validate_branch_name(path, branch).await?;
    let full_ref = format!("refs/heads/{branch}");
    let output = run_git(
        path,
        &["show-ref", "--verify", "--quiet", full_ref.as_str()],
    )
    .await?;
    if output.success {
        Ok(())
    } else {
        Err(GitError::InvalidInput {
            message: format!("本地分支不存在: {branch}"),
        })
    }
}

pub(super) async fn validate_remote_name(path: &Path, remote: &str) -> Result<(), GitError> {
    if remote.trim().is_empty() || remote != remote.trim() || remote.starts_with('-') {
        return Err(GitError::InvalidInput {
            message: "remote 名不能为空、包含首尾空白或以 '-' 开头".to_string(),
        });
    }
    let remotes = ensure_success("remote_list", run_git(path, &["remote"]).await?)?;
    if remotes.lines().any(|candidate| candidate == remote) {
        Ok(())
    } else {
        Err(GitError::InvalidInput {
            message: format!("remote 不存在: {remote}"),
        })
    }
}

pub(super) async fn configured_upstream(
    path: &Path,
    branch: &str,
) -> Result<Option<(String, String)>, GitError> {
    let remote_key = format!("branch.{branch}.remote");
    let merge_key = format!("branch.{branch}.merge");
    let remote = optional_text(path, &["config", "--get", remote_key.as_str()]).await?;
    let merge_ref = optional_text(path, &["config", "--get", merge_key.as_str()]).await?;
    match (remote, merge_ref) {
        (Some(remote), Some(merge_ref)) => {
            let branch =
                merge_ref
                    .strip_prefix("refs/heads/")
                    .ok_or_else(|| GitError::InvalidInput {
                        message: format!("upstream merge ref 不是分支: {merge_ref}"),
                    })?;
            Ok(Some((remote, branch.to_string())))
        }
        (None, None) => Ok(None),
        _ => Err(GitError::InvalidInput {
            message: format!("分支 {branch} 的 upstream 配置不完整"),
        }),
    }
}

pub(super) async fn resolve_revision(path: &Path, revision: &str) -> Result<String, GitError> {
    if revision.trim().is_empty() || revision != revision.trim() || revision.starts_with('-') {
        return Err(GitError::InvalidInput {
            message: "revision 不能为空、包含首尾空白或以 '-' 开头".to_string(),
        });
    }
    rev_parse(path, revision)
        .await
        .map_err(|_| GitError::InvalidInput {
            message: format!("revision 不存在或不是 commit: {revision}"),
        })
}

pub(super) fn enforce_precondition(
    actual: &GitObservation,
    expected: &GitPrecondition,
) -> Result<(), GitError> {
    let branch_matches = expected
        .expected_branch
        .as_deref()
        .is_none_or(|branch| actual.branch.as_deref() == Some(branch));
    let head_matches = expected
        .expected_head
        .as_deref()
        .is_none_or(|head| actual.head.as_deref() == Some(head));
    let worktree_matches = expected
        .expected_worktree_path
        .as_deref()
        .is_none_or(|path| same_path(path, &actual.worktree_path));
    if branch_matches && head_matches && worktree_matches {
        return Ok(());
    }
    Err(GitError::StaleContext {
        expected: Box::new(expected.clone()),
        actual_branch: actual.branch.clone(),
        actual_head: actual.head.clone(),
        actual_worktree_path: actual.worktree_path.clone(),
    })
}

pub(super) fn require_clean(observation: &GitObservation, operation: &str) -> Result<(), GitError> {
    if observation.dirty.has_uncommitted {
        return Err(GitError::DirtyWorkspace {
            operation: operation.to_string(),
            dirty: observation.dirty.clone(),
        });
    }
    Ok(())
}

pub(super) async fn ahead_behind(path: &Path) -> Result<(u64, u64), GitError> {
    let output = run_git(
        path,
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
    )
    .await?;
    if !output.success {
        return Ok((0, 0));
    }
    let mut fields = output.stdout.split_whitespace();
    let behind = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let ahead = fields
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Ok((ahead, behind))
}

pub(super) async fn is_ancestor(
    path: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, GitError> {
    let output = run_git(path, &["merge-base", "--is-ancestor", ancestor, descendant]).await?;
    match output.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_failed("merge_base_is_ancestor", output)),
    }
}

pub(super) async fn count_revisions(path: &Path, range: &str) -> Result<u64, GitError> {
    let output = run_git(path, &["rev-list", "--count", range]).await?;
    let stdout = ensure_success("rev_list_count", output)?;
    stdout
        .parse::<u64>()
        .map_err(|error| GitError::Io(format!("无法解析 Git revision count {stdout:?}: {error}")))
}

pub(super) async fn rev_parse(path: &Path, revision: &str) -> Result<String, GitError> {
    required_text(
        path,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
    .await
}

pub(super) async fn required_path(path: &Path, args: &[&str]) -> Result<PathBuf, GitError> {
    required_text(path, args).await.map(PathBuf::from)
}

pub(super) async fn required_text(path: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = run_git(path, args).await?;
    ensure_success(&args.join(" "), output)
}

pub(super) async fn optional_text(path: &Path, args: &[&str]) -> Result<Option<String>, GitError> {
    let output = run_git(path, args).await?;
    if output.success {
        Ok(non_empty(&output.stdout))
    } else {
        Ok(None)
    }
}

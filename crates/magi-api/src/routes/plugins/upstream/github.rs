//! GitHub 来源只负责取得固定提交的原包；安装、授权与恢复仍归原安装事务。
use super::*;
use magi_app_server_protocol::generated::PluginGithubSource;
use sha2::{Digest, Sha512};

pub(super) fn validate(source: &PluginGithubSource) -> Result<(), ApiError> {
    let parts: Vec<_> = source.repository.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 100
                || *part == "."
                || *part == ".."
                || part.starts_with('-')
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
    {
        return Err(ApiError::InvalidInput(
            "GitHub 仓库必须为 owner/repository".into(),
        ));
    }
    if parts[1].trim_end_matches(".git").is_empty() {
        return Err(ApiError::InvalidInput("GitHub 仓库名称无效".into()));
    }
    if source.reference.as_ref().is_some_and(|value| {
        value.is_empty()
            || value.len() > 128
            || value.chars().any(|c| c.is_control() || c.is_whitespace())
            || value.starts_with('-')
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains("..")
            || value.contains(['\\', '?', '#', ':', '~', '^', '*', '[', '@'])
    }) {
        return Err(ApiError::InvalidInput("GitHub 分支、标签或提交无效".into()));
    }
    if source
        .commit
        .as_ref()
        .is_some_and(|value| value.len() != 40 || !value.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(ApiError::InvalidInput(
            "GitHub 固定提交必须为 40 位十六进制 SHA".into(),
        ));
    }
    Ok(())
}

pub(super) async fn acquire(
    source: &PluginGithubSource,
) -> Result<(Vec<u8>, String, PluginGithubSource), ApiError> {
    use base64::Engine as _;
    validate(source)?;
    let repository = source
        .repository
        .trim_end_matches(".git")
        .to_ascii_lowercase();
    // 显式 commit 不重新解释浮动分支；预览确认后分支移动也不能改变安装内容。
    let commit = if let Some(commit) = &source.commit {
        commit.to_ascii_lowercase()
    } else {
        let reference = urlencoding::encode(source.reference.as_deref().unwrap_or("HEAD"));
        let url = format!("https://api.github.com/repos/{repository}/commits/{reference}");
        let bytes = download_bytes(&url, MAX_REGISTRY_DOCUMENT_BYTES, "GitHub 提交").await?;
        let document: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::InvalidInput("GitHub 提交响应无效".into()))?;
        let commit = document["sha"].as_str().unwrap_or_default();
        let pinned = PluginGithubSource {
            repository: repository.clone(),
            reference: source.reference.clone(),
            commit: Some(commit.into()),
        };
        validate(&pinned)?;
        commit.to_ascii_lowercase()
    };
    let url = format!("https://codeload.github.com/{repository}/tar.gz/{commit}");
    let bytes = download_bytes(&url, npm::MAX_TARBALL_BYTES, "GitHub 原包").await?;
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&bytes))
    );
    Ok((
        bytes,
        integrity,
        PluginGithubSource {
            repository,
            reference: source.reference.clone(),
            commit: Some(commit),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repository_and_revision_are_data_without_url_or_path_semantics() {
        let source = |repository: &str, reference: &str| PluginGithubSource {
            repository: repository.into(),
            reference: Some(reference.into()),
            commit: None,
        };
        for (repo, reference) in [
            ("owner/repo", "main"),
            ("owner/repo", "feature/desktop"),
            ("owner/repo.git", "v1.2.0"),
        ] {
            assert!(validate(&source(repo, reference)).is_ok());
        }
        for (repo, reference) in [
            ("https://github.com/a/b", "main"),
            ("../repo", "main"),
            ("a/b/../../c", "main"),
            ("a/b", "../main"),
            ("a/b", "main?x=y"),
            ("a/b", "--upload-pack=x"),
            ("a/b", ""),
        ] {
            assert!(
                validate(&source(repo, reference)).is_err(),
                "{repo}#{reference}"
            );
        }
    }
}

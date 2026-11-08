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
    // 从同一份 Git 归档读取实际提交并绑定预览摘要。确认提交后只取该 SHA，
    // 不再次解释浮动引用；不维护 REST 解析或 rate-limit 后的第二条来源路径。
    let revision = source
        .commit
        .as_deref()
        .or(source.reference.as_deref())
        .unwrap_or("HEAD");
    let revision = urlencoding::encode(revision);
    let url = format!("https://codeload.github.com/{repository}/tar.gz/{revision}");
    let mut bytes = download_bytes(&url, npm::MAX_TARBALL_BYTES, "GitHub 原包").await?;
    let commit = npm::repository_commit(&bytes)
        .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
    if source
        .commit
        .as_ref()
        .is_some_and(|expected| !expected.eq_ignore_ascii_case(&commit))
    {
        return Err(ApiError::InvalidInput(
            "GitHub 归档提交与预览确认身份不一致".into(),
        ));
    }
    if !revision.eq_ignore_ascii_case(&commit) {
        // GitHub 按请求引用生成归档根名，tag 与 SHA 的 tar 字节并不相同。
        // 安装准备始终取 SHA 归档，保证预览和确认使用同一完整性与摘要。
        let url = format!("https://codeload.github.com/{repository}/tar.gz/{commit}");
        bytes = download_bytes(&url, npm::MAX_TARBALL_BYTES, "GitHub 固定提交原包").await?;
        if npm::repository_commit(&bytes)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?
            != commit
        {
            return Err(ApiError::InvalidInput(
                "GitHub 固定归档的提交身份不一致".into(),
            ));
        }
    }
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

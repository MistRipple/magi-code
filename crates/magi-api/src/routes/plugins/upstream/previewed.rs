//! 预览已翻译的产物。用户确认的摘要对应的就是这份已翻译字节，安装直接使用它，
//! 不再重新翻译：依赖在 registry 上的新发布和上游启动时序都不会让确认后的包改变。
//! 条目只存在于本进程内、有数量／体积／时间上限；缺失即要求重新预览，不另走第二条翻译路径。
use super::*;
use std::{
    collections::VecDeque,
    sync::{LazyLock, Mutex},
    time::Instant,
};

const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 128 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(30 * 60);

struct Entry {
    translated: Translated,
    size: usize,
    stored_at: Instant,
}

static PREVIEWED: LazyLock<Mutex<VecDeque<Entry>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

fn lock() -> std::sync::MutexGuard<'static, VecDeque<Entry>> {
    PREVIEWED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn remember(translated: &Translated) {
    let digest = translated.package.digest().to_owned();
    let size = translated.package.files().values().map(Vec::len).sum();
    let mut entries = lock();
    entries.retain(|entry| {
        entry.stored_at.elapsed() < TTL && entry.translated.package.digest() != digest
    });
    entries.push_back(Entry {
        translated: translated.clone(),
        size,
        stored_at: Instant::now(),
    });
    // 最新的一条始终保留；其余按先进先出逐出，直到满足数量与体积预算。
    while entries.len() > 1
        && (entries.len() > MAX_ENTRIES
            || entries.iter().map(|entry| entry.size).sum::<usize>() > MAX_BYTES)
    {
        entries.pop_front();
    }
}

/// 取回预览时翻译的产物；摘要不存在、已过期或与请求的来源不符都返回 `None`。
pub(super) fn recall(digest: &str, request: &PluginUpstreamRequest) -> Option<Translated> {
    let mut entries = lock();
    entries.retain(|entry| entry.stored_at.elapsed() < TTL);
    entries
        .iter()
        .find(|entry| {
            entry.translated.package.digest() == digest
                && matches_request(&entry.translated.source, request)
        })
        .map(|entry| entry.translated.clone())
}

fn matches_request(source: &PluginSource, request: &PluginUpstreamRequest) -> bool {
    let PluginSource::Upstream {
        ecosystem,
        package,
        version,
        github,
    } = source
    else {
        return false;
    };
    if ecosystem != ecosystem_name(&request.ecosystem) {
        return false;
    }
    match (&request.github, github) {
        (Some(wanted), Some(actual)) => {
            wanted.repository.trim_end_matches(".git").to_ascii_lowercase() == actual.repository
                && wanted.commit.as_deref().map(str::to_ascii_lowercase) == actual.commit
        }
        (None, None) => {
            request.package.as_deref() == Some(package.as_str())
                && request.version.as_deref() == Some(version.as_str())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(package: &str, version: &str) -> PluginUpstreamRequest {
        PluginUpstreamRequest {
            ecosystem: PluginEcosystem::Dsh,
            package: Some(package.into()),
            version: Some(version.into()),
            expected_digest: None,
            github: None,
        }
    }

    fn source(package: &str, version: &str) -> PluginSource {
        PluginSource::Upstream {
            ecosystem: "dsh".into(),
            package: package.into(),
            version: version.into(),
            github: None,
        }
    }

    #[test]
    fn a_confirmed_digest_only_matches_its_own_source() {
        assert!(matches_request(
            &source("sample", "1.0.0"),
            &request("sample", "1.0.0")
        ));
        assert!(!matches_request(
            &source("sample", "1.0.0"),
            &request("sample", "1.0.1")
        ));
        assert!(!matches_request(
            &source("sample", "1.0.0"),
            &request("other", "1.0.0")
        ));
        let mut pi = request("sample", "1.0.0");
        pi.ecosystem = PluginEcosystem::Pi;
        assert!(!matches_request(&source("sample", "1.0.0"), &pi));
    }

    #[test]
    fn github_sources_match_by_normalized_repository_and_pinned_commit() {
        use magi_app_server_protocol::generated::PluginGithubSource;
        let commit = "a".repeat(40);
        let pinned = PluginSource::Upstream {
            ecosystem: "dsh".into(),
            package: "sample".into(),
            version: "1.0.0".into(),
            github: Some(PluginGithubSource {
                repository: "owner/repo".into(),
                reference: Some("main".into()),
                commit: Some(commit.clone()),
            }),
        };
        let mut wanted = request("sample", "1.0.0");
        wanted.package = None;
        wanted.version = None;
        wanted.github = Some(PluginGithubSource {
            repository: "Owner/Repo.git".into(),
            reference: Some("main".into()),
            commit: Some(commit.to_ascii_uppercase()),
        });
        assert!(matches_request(&pinned, &wanted));
        wanted.github.as_mut().unwrap().commit = Some("b".repeat(40));
        assert!(!matches_request(&pinned, &wanted));
        assert!(!matches_request(&pinned, &request("sample", "1.0.0")));
    }
}

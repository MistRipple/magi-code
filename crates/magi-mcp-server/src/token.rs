//! 客户端令牌：生成、哈希存储、校验与吊销。
//!
//! 令牌是「这个客户端在这个工作区里能做什么」的凭据：绑定一个工作区、一个权限档与
//! 一个归属模式。原文只在创建时返回一次；这里只保存哈希、前缀与元数据。原文不进日志、
//! 不进诊断、不进 canonical。

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::profile::Profile;

/// 令牌原文的固定前缀，便于用户和扫描工具识别。
pub const TOKEN_PREFIX: &str = "magi_mcp_";

/// 列表里展示的前缀长度（含固定前缀）。
const DISPLAY_PREFIX_LEN: usize = TOKEN_PREFIX.len() + 6;

/// 调用的归属方式（令牌属性）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionMode {
    /// 归到该令牌自己的外部工具会话（默认）。
    #[default]
    External,
    /// 归到 GPT Web 槽位拥有者进行中的 turn（仅 GPT Web 连接器使用）。
    FollowWebSlot,
}

/// 持久化的令牌元数据。**不含原文。**
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRecord {
    pub token_id: String,
    /// 原文前 15 个字符，仅用于列表识别。
    pub prefix: String,
    /// `sha256("magi-mcp-token-v1:" + 原文)` 的十六进制。
    pub hash: String,
    pub client_name: String,
    pub workspace_id: String,
    pub profile: Profile,
    #[serde(default)]
    pub attribution: AttributionMode,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<u64>,
    /// 是否允许经公网隧道使用。默认 `false`：只能在本机（回环 HTTP / stdio）使用。
    #[serde(default)]
    pub network: bool,
}

impl TokenRecord {
    pub fn is_active(&self, now_ms: u64) -> bool {
        self.revoked_at_ms.is_none() && self.expires_at_ms.is_none_or(|expires| expires > now_ms)
    }
}

/// 创建令牌的参数。
#[derive(Clone, Debug)]
pub struct IssueTokenRequest {
    pub client_name: String,
    pub workspace_id: String,
    pub profile: Profile,
    pub attribution: AttributionMode,
    /// 有效期（毫秒）。`None` 表示不过期。
    pub ttl_ms: Option<u64>,
    /// 允许经公网隧道使用（显式选择，默认不允许）。
    pub network: bool,
}

/// 创建结果：**原文只在这里出现一次**。
pub struct IssuedToken {
    pub record: TokenRecord,
    pub secret: String,
}

impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IssuedToken")
            .field("record", &self.record)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// 认证失败。所有失败原因对外统一，不区分“不存在 / 过期 / 已吊销”。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("认证失败")]
    Invalid,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("生成随机令牌失败: {0}")]
    Random(String),
    #[error("客户端名称不能为空")]
    EmptyClientName,
    #[error("工作区不能为空")]
    EmptyWorkspace,
    #[error("令牌不存在")]
    NotFound,
    #[error("令牌已吊销，不能再修改")]
    Revoked,
}

/// 编辑令牌的补丁。`None` 表示不改；不含原文，编辑从不更换原文。
#[derive(Clone, Debug, Default)]
pub struct TokenPatch {
    pub client_name: Option<String>,
    pub profile: Option<Profile>,
    /// `Some(None)` 表示改为不过期；`Some(Some(t))` 表示在 `t`（毫秒时间戳）过期。
    pub expires_at_ms: Option<Option<u64>>,
    pub network: Option<bool>,
}

/// 令牌哈希。原文本身有 256 位随机熵，因此不需要慢速 KDF。
pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"magi-mcp-token-v1:");
    hasher.update(secret.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 恒定时间比较，避免通过响应时间逐字节猜测哈希。
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn random_hex(byte_len: usize) -> Result<String, TokenError> {
    let mut bytes = vec![0u8; byte_len];
    getrandom::fill(&mut bytes).map_err(|error| TokenError::Random(error.to_string()))?;
    Ok(hex(&bytes))
}

/// 令牌存储（进程内）。持久化由调用方通过 `records` / `from_records` 完成。
#[derive(Debug, Default)]
pub struct TokenStore {
    records: Mutex<Vec<TokenRecord>>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从持久化的元数据恢复。读不出来的调用方应传空列表（无令牌，绝不让 daemon 起不来）。
    pub fn from_records(records: Vec<TokenRecord>) -> Self {
        Self {
            records: Mutex::new(records),
        }
    }

    pub fn records(&self) -> Vec<TokenRecord> {
        self.records.lock().expect("token store poisoned").clone()
    }

    pub fn issue(
        &self,
        request: IssueTokenRequest,
        now_ms: u64,
    ) -> Result<IssuedToken, TokenError> {
        if request.client_name.trim().is_empty() {
            return Err(TokenError::EmptyClientName);
        }
        if request.workspace_id.trim().is_empty() {
            return Err(TokenError::EmptyWorkspace);
        }
        let secret = format!("{TOKEN_PREFIX}{}", random_hex(32)?);
        let record = TokenRecord {
            token_id: format!("mcp-token-{}", random_hex(8)?),
            prefix: secret.chars().take(DISPLAY_PREFIX_LEN).collect(),
            hash: hash_secret(&secret),
            client_name: request.client_name.trim().to_string(),
            workspace_id: request.workspace_id.trim().to_string(),
            profile: request.profile,
            attribution: request.attribution,
            created_at_ms: now_ms,
            expires_at_ms: request.ttl_ms.map(|ttl| now_ms.saturating_add(ttl)),
            revoked_at_ms: None,
            last_used_at_ms: None,
            network: request.network,
        };
        self.records
            .lock()
            .expect("token store poisoned")
            .push(record.clone());
        Ok(IssuedToken { record, secret })
    }

    /// 编辑令牌。已吊销的令牌不可编辑。返回编辑前后的记录，调用方据此判断是否收紧了权限。
    /// 原文不变，所以客户端无需重新配置；下一次调用起按新记录生效（每次请求都重新认证）。
    pub fn update(
        &self,
        token_id: &str,
        patch: TokenPatch,
        now_ms: u64,
    ) -> Result<(TokenRecord, TokenRecord), TokenError> {
        let mut records = self.records.lock().expect("token store poisoned");
        let record = records
            .iter_mut()
            .find(|record| record.token_id == token_id)
            .ok_or(TokenError::NotFound)?;
        if record.revoked_at_ms.is_some() {
            return Err(TokenError::Revoked);
        }
        let name = match &patch.client_name {
            Some(name) if name.trim().is_empty() => return Err(TokenError::EmptyClientName),
            Some(name) => Some(name.trim().to_string()),
            None => None,
        };
        let before = record.clone();
        if let Some(name) = name {
            record.client_name = name;
        }
        if let Some(profile) = patch.profile {
            record.profile = profile;
        }
        if let Some(expires) = patch.expires_at_ms {
            record.expires_at_ms = expires;
        }
        if let Some(network) = patch.network {
            record.network = network;
        }
        let _ = now_ms;
        Ok((before, record.clone()))
    }

    /// 重新生成原文：旧原文立即失效，其余配置不变。返回新原文。
    pub fn rotate(&self, token_id: &str) -> Result<(TokenRecord, String), TokenError> {
        let mut records = self.records.lock().expect("token store poisoned");
        let record = records
            .iter_mut()
            .find(|record| record.token_id == token_id)
            .ok_or(TokenError::NotFound)?;
        if record.revoked_at_ms.is_some() {
            return Err(TokenError::Revoked);
        }
        let secret = format!("{TOKEN_PREFIX}{}", random_hex(32)?);
        record.prefix = secret.chars().take(DISPLAY_PREFIX_LEN).collect();
        record.hash = hash_secret(&secret);
        Ok((record.clone(), secret))
    }

    /// 校验令牌原文。失败统一返回 `AuthError::Invalid`。
    pub fn authenticate(&self, secret: &str, now_ms: u64) -> Result<TokenRecord, AuthError> {
        if !secret.starts_with(TOKEN_PREFIX) {
            return Err(AuthError::Invalid);
        }
        let candidate = hash_secret(secret);
        let mut records = self.records.lock().expect("token store poisoned");
        // 不在第一次命中时提前返回：所有记录都做一次恒定时间比较。
        let mut matched: Option<usize> = None;
        for (index, record) in records.iter().enumerate() {
            if constant_time_eq(record.hash.as_bytes(), candidate.as_bytes()) {
                matched = Some(index);
            }
        }
        let index = matched.ok_or(AuthError::Invalid)?;
        if !records[index].is_active(now_ms) {
            return Err(AuthError::Invalid);
        }
        records[index].last_used_at_ms = Some(now_ms);
        Ok(records[index].clone())
    }

    /// 吊销单个令牌，返回是否找到。
    pub fn revoke(&self, token_id: &str, now_ms: u64) -> bool {
        let mut records = self.records.lock().expect("token store poisoned");
        match records
            .iter_mut()
            .find(|record| record.token_id == token_id)
        {
            Some(record) => {
                record.revoked_at_ms.get_or_insert(now_ms);
                true
            }
            None => false,
        }
    }

    /// 撤销全部令牌，返回本次新撤销的数量。
    pub fn revoke_all(&self, now_ms: u64) -> usize {
        let mut records = self.records.lock().expect("token store poisoned");
        let mut revoked = 0;
        for record in records.iter_mut() {
            if record.revoked_at_ms.is_none() {
                record.revoked_at_ms = Some(now_ms);
                revoked += 1;
            }
        }
        revoked
    }

    /// 工作区被移除时使其令牌全部失效。
    pub fn revoke_workspace(&self, workspace_id: &str, now_ms: u64) -> usize {
        let mut records = self.records.lock().expect("token store poisoned");
        let mut revoked = 0;
        for record in records
            .iter_mut()
            .filter(|record| record.workspace_id == workspace_id && record.revoked_at_ms.is_none())
        {
            record.revoked_at_ms = Some(now_ms);
            revoked += 1;
        }
        revoked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(profile: Profile) -> IssueTokenRequest {
        IssueTokenRequest {
            client_name: "Cursor · 笔记本".to_string(),
            workspace_id: "workspace-a".to_string(),
            profile,
            attribution: AttributionMode::External,
            ttl_ms: None,
            network: false,
        }
    }

    #[test]
    fn issued_secret_authenticates_and_is_never_stored() {
        let store = TokenStore::new();
        let issued = store.issue(request(Profile::ReadOnly), 1_000).unwrap();
        assert!(issued.secret.starts_with(TOKEN_PREFIX));
        assert_eq!(issued.secret.len(), TOKEN_PREFIX.len() + 64);

        let serialized = serde_json::to_string(&store.records()).unwrap();
        assert!(
            !serialized.contains(&issued.secret),
            "原文不得进入持久化元数据"
        );
        assert!(
            !format!("{issued:?}").contains(&issued.secret),
            "原文不得进入 Debug"
        );

        let record = store.authenticate(&issued.secret, 2_000).unwrap();
        assert_eq!(record.token_id, issued.record.token_id);
        assert_eq!(record.workspace_id, "workspace-a");
        assert_eq!(store.records()[0].last_used_at_ms, Some(2_000));
    }

    #[test]
    fn wrong_unknown_expired_and_revoked_tokens_fail_identically() {
        let store = TokenStore::new();
        let mut short = request(Profile::Edit);
        short.ttl_ms = Some(500);
        let expiring = store.issue(short, 1_000).unwrap();
        let revoked = store.issue(request(Profile::Edit), 1_000).unwrap();
        assert!(store.revoke(&revoked.record.token_id, 1_100));

        assert_eq!(
            store.authenticate("magi_mcp_deadbeef", 1_200),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            store.authenticate("not-a-token", 1_200),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            store.authenticate(&expiring.secret, 1_500),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            store.authenticate(&revoked.secret, 1_200),
            Err(AuthError::Invalid)
        );
        // 未过期时仍可用。
        assert!(store.authenticate(&expiring.secret, 1_499).is_ok());
    }

    #[test]
    fn revoke_all_and_workspace_removal_invalidate_tokens() {
        let store = TokenStore::new();
        let a = store.issue(request(Profile::ReadOnly), 1).unwrap();
        let mut other = request(Profile::ReadOnly);
        other.workspace_id = "workspace-b".to_string();
        let b = store.issue(other, 1).unwrap();

        assert_eq!(store.revoke_workspace("workspace-a", 2), 1);
        assert!(store.authenticate(&a.secret, 3).is_err());
        assert!(store.authenticate(&b.secret, 3).is_ok());
        assert_eq!(store.revoke_all(4), 1);
        assert!(store.authenticate(&b.secret, 5).is_err());
        assert_eq!(store.revoke_all(6), 0, "重复撤销不再计数");
    }

    #[test]
    fn records_round_trip_without_secret_and_reject_blank_inputs() {
        let store = TokenStore::new();
        let issued = store.issue(request(Profile::EditTrusted), 10).unwrap();
        let restored = TokenStore::from_records(
            serde_json::from_str(&serde_json::to_string(&store.records()).unwrap()).unwrap(),
        );
        assert!(restored.authenticate(&issued.secret, 11).is_ok());

        let mut blank = request(Profile::ReadOnly);
        blank.client_name = "  ".to_string();
        assert!(matches!(
            store.issue(blank, 1),
            Err(TokenError::EmptyClientName)
        ));
        let mut blank = request(Profile::ReadOnly);
        blank.workspace_id = String::new();
        assert!(matches!(
            store.issue(blank, 1),
            Err(TokenError::EmptyWorkspace)
        ));
    }

    #[test]
    fn constant_time_compare_rejects_length_and_content_differences() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn update_changes_scope_without_changing_the_secret_and_rejects_revoked_tokens() {
        let store = TokenStore::new();
        let issued = store.issue(request(Profile::ReadOnly), 1_000).unwrap();
        let id = issued.record.token_id.clone();

        let (before, after) = store
            .update(
                &id,
                TokenPatch {
                    client_name: Some("  Claude Desktop ".to_string()),
                    profile: Some(Profile::Edit),
                    expires_at_ms: Some(Some(9_000)),
                    network: Some(true),
                },
                2_000,
            )
            .unwrap();
        assert_eq!(before.profile, Profile::ReadOnly);
        assert_eq!(after.client_name, "Claude Desktop");
        assert_eq!(after.profile, Profile::Edit);
        assert_eq!(after.expires_at_ms, Some(9_000));
        assert!(after.network);
        // 原文不变：仍用创建时的原文认证，且读到的是新的权限档。
        let authenticated = store.authenticate(&issued.secret, 3_000).unwrap();
        assert_eq!(authenticated.profile, Profile::Edit);

        // 改为不过期；空名称被拒绝。
        let (_, never) = store
            .update(
                &id,
                TokenPatch {
                    expires_at_ms: Some(None),
                    ..TokenPatch::default()
                },
                4_000,
            )
            .unwrap();
        assert_eq!(never.expires_at_ms, None);
        assert!(matches!(
            store.update(
                &id,
                TokenPatch {
                    client_name: Some("   ".to_string()),
                    ..TokenPatch::default()
                },
                4_000
            ),
            Err(TokenError::EmptyClientName)
        ));

        store.revoke(&id, 5_000);
        assert!(matches!(
            store.update(&id, TokenPatch::default(), 6_000),
            Err(TokenError::Revoked)
        ));
        assert!(matches!(
            store.update("missing", TokenPatch::default(), 6_000),
            Err(TokenError::NotFound)
        ));
    }

    #[test]
    fn rotate_replaces_the_secret_and_keeps_everything_else() {
        let store = TokenStore::new();
        let issued = store.issue(request(Profile::Edit), 1_000).unwrap();
        let (record, new_secret) = store.rotate(&issued.record.token_id).unwrap();
        assert_ne!(new_secret, issued.secret);
        assert_eq!(record.profile, Profile::Edit);
        assert_eq!(record.workspace_id, "workspace-a");
        assert!(
            store.authenticate(&issued.secret, 2_000).is_err(),
            "旧原文立即失效"
        );
        assert!(store.authenticate(&new_secret, 2_000).is_ok());
        assert!(
            !serde_json::to_string(&store.records())
                .unwrap()
                .contains(&new_secret)
        );
    }
}

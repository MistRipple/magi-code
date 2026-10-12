//! 插件中心目录：已签名的发布清单，只作为发现入口。
//!
//! 目录由受信公钥的 Ed25519 签名保护。签名只证明“这份目录由持有私钥的发布者给出”，
//! 并把每个条目绑定到发布包的 SHA-256 摘要；安装时仍走与本地包完全相同的包校验、
//! 授权和生命周期，目录签名不能代替用户对权限的授权。

use base64::Engine as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

/// 目录的最大字节数，防止恶意地址返回超大内容。
pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const CATALOG_FORMAT: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrustedKey {
    pub key_id: String,
    /// Base64 编码的 32 字节 Ed25519 公钥。
    pub public_key: String,
}

/// 传输信封：`payload` 是目录 JSON 的原始字节（Base64），签名覆盖这些字节本身，
/// 因此不依赖任何 JSON 规范化规则。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    payload: String,
    key_id: String,
    signature: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// 发布包下载地址（HTTP(S)）。
    pub url: String,
    /// 发布包 ZIP 的 SHA-256（小写十六进制），与安装包摘要同一定义。
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCatalog {
    pub format: u16,
    /// 发布时间（毫秒）。手写的目录可以省略。
    #[serde(default)]
    pub issued_at: u64,
    pub plugins: Vec<CatalogEntry>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CatalogError {
    #[error("插件中心目录无效：{0}")]
    Invalid(String),
    #[error("插件中心目录签名未通过受信公钥校验")]
    Untrusted,
    #[error("已配置受信公钥，但该插件中心目录没有签名")]
    Unsigned,
}

fn invalid(message: impl Into<String>) -> CatalogError {
    CatalogError::Invalid(message.into())
}

/// 解析结果：`signed` 表示目录通过了受信公钥验签。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCatalog {
    pub catalog: PluginCatalog,
    pub signed: bool,
}

impl PluginCatalog {
    /// 严格模式：只接受受信公钥签名的目录。
    pub fn verify(bytes: &[u8], trusted: &[TrustedKey]) -> Result<Self, CatalogError> {
        if trusted.is_empty() {
            return Err(CatalogError::Untrusted);
        }
        Self::parse(bytes, trusted).map(|parsed| parsed.catalog)
    }

    /// 解析目录。目录可以是普通目录 JSON，也可以是签名信封。
    ///
    /// - 配置了受信公钥：必须是信封且验签通过，验签先于任何内容解析；普通目录被拒绝。
    /// - 没有配置公钥：普通目录和信封都接受，但 `signed` 为 false——这样的目录只提供“发现”，
    ///   信任级别等同于指定地址安装，包仍须与目录条目的摘要一致，权限仍须用户逐项授权。
    pub fn parse(bytes: &[u8], trusted: &[TrustedKey]) -> Result<ParsedCatalog, CatalogError> {
        if bytes.is_empty() || bytes.len() > MAX_CATALOG_BYTES {
            return Err(invalid("大小超限"));
        }
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| invalid("不是有效的 JSON"))?;
        let is_envelope = value.get("payload").is_some() && value.get("signature").is_some();
        let (payload, signed) = if is_envelope {
            let envelope: Envelope =
                serde_json::from_value(value).map_err(|_| invalid("不是签名信封"))?;
            let b64 = base64::engine::general_purpose::STANDARD;
            let payload = b64
                .decode(envelope.payload.trim())
                .map_err(|_| invalid("payload 编码无效"))?;
            if trusted.is_empty() {
                (payload, false)
            } else {
                let signature = b64
                    .decode(envelope.signature.trim())
                    .map_err(|_| invalid("签名编码无效"))?;
                let key = trusted
                    .iter()
                    .find(|key| key.key_id == envelope.key_id)
                    .ok_or(CatalogError::Untrusted)?;
                let public_key = b64
                    .decode(key.public_key.trim())
                    .map_err(|_| invalid("受信公钥编码无效"))?;
                UnparsedPublicKey::new(&ED25519, public_key)
                    .verify(&payload, &signature)
                    .map_err(|_| CatalogError::Untrusted)?;
                (payload, true)
            }
        } else if trusted.is_empty() {
            (bytes.to_vec(), false)
        } else {
            return Err(CatalogError::Unsigned);
        };

        let catalog: PluginCatalog =
            serde_json::from_slice(&payload).map_err(|_| invalid("目录内容不符合合同"))?;
        catalog.validate()?;
        Ok(ParsedCatalog { catalog, signed })
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if self.format != CATALOG_FORMAT {
            return Err(invalid("目录格式版本不受支持"));
        }
        if self.plugins.len() > MAX_ENTRIES {
            return Err(invalid("条目数量超限"));
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &self.plugins {
            let url = is_http_url(&entry.url) || is_relative_path(&entry.url);
            if !seen.insert((entry.id.as_str(), entry.version.as_str()))
                || entry.id.is_empty()
                || entry.name.trim().is_empty()
                || semver::Version::parse(&entry.version).is_err()
                || entry.sha256.len() != 64
                || !entry
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || entry.size == 0
                || entry.size > crate::MAX_PACKAGE_BYTES as u64
                || !url
            {
                return Err(invalid("条目身份、版本、摘要或地址无效，或条目重复"));
            }
        }
        Ok(())
    }

    /// 目录中某插件的最新版本（按 SemVer）。
    pub fn latest(&self, id: &str) -> Option<&CatalogEntry> {
        self.plugins
            .iter()
            .filter(|entry| entry.id == id)
            .max_by_key(|entry| semver::Version::parse(&entry.version).ok())
    }
}

impl CatalogEntry {
    /// 下载得到的包必须与目录条目逐项一致：摘要、插件身份和版本。
    /// 摘要不一致说明下载内容不是发布者签名的那份包。
    pub fn check_package(&self, package: &crate::PluginPackage) -> Result<(), CatalogError> {
        if package.digest() != self.sha256 {
            return Err(invalid("发布包摘要与目录不一致"));
        }
        let manifest = package.manifest();
        if manifest.id != self.id || manifest.version != self.version {
            return Err(invalid("发布包身份或版本与目录不一致"));
        }
        Ok(())
    }
}

/// 相对地址：相对于目录自身的位置解析（由调用方完成），不能带协议、主机或反斜杠。
fn is_relative_path(url: &str) -> bool {
    !url.is_empty()
        && !url.starts_with('/')
        && !url.contains("://")
        && !url
            .split('/')
            .next()
            .is_some_and(|first| first.contains(':'))
        && !url.contains('\\')
        && !url.contains(char::is_whitespace)
}

/// 仅检查地址形态（HTTP(S) 且有主机），不在此处发起网络访问。
fn is_http_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|host| !host.is_empty() && !host.contains(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };

    fn signing_key() -> (Ed25519KeyPair, TrustedKey) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let trusted = TrustedKey {
            key_id: "test-key".into(),
            public_key: base64::engine::general_purpose::STANDARD.encode(pair.public_key()),
        };
        (pair, trusted)
    }

    fn catalog_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "format": 1, "issuedAt": 1,
            "plugins": [{
                "id": "acme.board", "name": "Board", "description": "",
                "version": "1.2.0", "url": "https://plugins.example/board-1.2.0.zip",
                "sha256": "a".repeat(64), "size": 100
            }, {
                "id": "acme.board", "name": "Board", "description": "",
                "version": "1.10.0", "url": "https://plugins.example/board-1.10.0.zip",
                "sha256": "b".repeat(64), "size": 100
            }]
        }))
        .unwrap()
    }

    fn envelope(pair: &Ed25519KeyPair, key_id: &str, payload: &[u8]) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD;
        serde_json::to_vec(&serde_json::json!({
            "payload": b64.encode(payload),
            "keyId": key_id,
            "signature": b64.encode(pair.sign(payload).as_ref()),
        }))
        .unwrap()
    }

    #[test]
    fn signed_catalog_verifies_and_picks_the_latest_semver() {
        let (pair, trusted) = signing_key();
        let catalog =
            PluginCatalog::verify(&envelope(&pair, "test-key", &catalog_json()), &[trusted])
                .unwrap();
        assert_eq!(catalog.latest("acme.board").unwrap().version, "1.10.0");
        assert!(catalog.latest("acme.other").is_none());
    }

    #[test]
    fn unknown_key_wrong_signature_and_tampered_payload_are_untrusted() {
        let (pair, trusted) = signing_key();
        let (other_pair, _) = signing_key();
        let good = catalog_json();
        // 公钥不在受信列表。
        assert_eq!(
            PluginCatalog::verify(
                &envelope(&pair, "someone-else", &good),
                std::slice::from_ref(&trusted)
            ),
            Err(CatalogError::Untrusted)
        );
        // 由另一把私钥签名，但冒用受信 keyId。
        assert_eq!(
            PluginCatalog::verify(
                &envelope(&other_pair, "test-key", &good),
                std::slice::from_ref(&trusted)
            ),
            Err(CatalogError::Untrusted)
        );
        // 签名后篡改内容：把摘要换成攻击者的包。
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut forged: serde_json::Value =
            serde_json::from_slice(&envelope(&pair, "test-key", &good)).unwrap();
        let tampered = String::from_utf8(good)
            .unwrap()
            .replace(&"a".repeat(64), &"c".repeat(64));
        forged["payload"] = serde_json::Value::String(b64.encode(tampered));
        assert_eq!(
            PluginCatalog::verify(&serde_json::to_vec(&forged).unwrap(), &[trusted]),
            Err(CatalogError::Untrusted)
        );
    }

    #[test]
    fn signed_but_malformed_catalogs_are_rejected_after_verification() {
        let (pair, trusted) = signing_key();
        let verify = |value: serde_json::Value| {
            PluginCatalog::verify(
                &envelope(&pair, "test-key", &serde_json::to_vec(&value).unwrap()),
                std::slice::from_ref(&trusted),
            )
        };
        let entry = |mutate: fn(&mut serde_json::Value)| {
            let mut entry = serde_json::json!({
                "id": "acme.board", "name": "Board", "description": "", "version": "1.0.0",
                "url": "https://plugins.example/b.zip", "sha256": "a".repeat(64), "size": 10
            });
            mutate(&mut entry);
            serde_json::json!({"format": 1, "issuedAt": 1, "plugins": [entry]})
        };
        assert!(verify(entry(|_| {})).is_ok());
        assert!(verify(entry(|e| e["url"] = "file:///etc/passwd".into())).is_err());
        assert!(verify(entry(|e| e["sha256"] = "XYZ".into())).is_err());
        assert!(verify(entry(|e| e["version"] = "latest".into())).is_err());
        assert!(verify(entry(|e| e["size"] = 0.into())).is_err());
        assert!(verify(serde_json::json!({"format": 2, "issuedAt": 1, "plugins": []})).is_err());
    }

    #[test]
    fn without_trusted_keys_plain_and_enveloped_catalogs_are_accepted_as_unsigned() {
        let plain = PluginCatalog::parse(&catalog_json(), &[]).unwrap();
        assert!(!plain.signed);
        assert_eq!(
            plain.catalog.latest("acme.board").unwrap().version,
            "1.10.0"
        );
        // 手写目录可以省略 issuedAt，包地址可以是相对路径。
        let handwritten = serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "plugins": [{
                "id": "acme.board", "name": "Board", "description": "", "version": "1.0.0",
                "url": "board-1.0.0.zip", "sha256": "a".repeat(64), "size": 10
            }]
        }))
        .unwrap();
        let parsed = PluginCatalog::parse(&handwritten, &[]).unwrap();
        assert_eq!(parsed.catalog.plugins[0].url, "board-1.0.0.zip");

        // 信封在没有公钥时也只能当作未签名目录使用，绝不标记为已验证。
        let (pair, _) = signing_key();
        let enveloped =
            PluginCatalog::parse(&envelope(&pair, "any", &catalog_json()), &[]).unwrap();
        assert!(!enveloped.signed);
    }

    #[test]
    fn configuring_a_trusted_key_requires_a_signature() {
        let (pair, trusted) = signing_key();
        assert_eq!(
            PluginCatalog::parse(&catalog_json(), std::slice::from_ref(&trusted)),
            Err(CatalogError::Unsigned)
        );
        let parsed = PluginCatalog::parse(
            &envelope(&pair, "test-key", &catalog_json()),
            std::slice::from_ref(&trusted),
        )
        .unwrap();
        assert!(parsed.signed);
        // 严格入口在没有公钥时不会退化成“接受任何目录”。
        assert_eq!(
            PluginCatalog::verify(&catalog_json(), &[]),
            Err(CatalogError::Untrusted)
        );
    }

    #[test]
    fn relative_package_paths_cannot_smuggle_other_schemes() {
        let entry = |url: &str| {
            serde_json::to_vec(&serde_json::json!({
                "format": 1,
                "plugins": [{
                    "id": "acme.board", "name": "Board", "description": "", "version": "1.0.0",
                    "url": url, "sha256": "a".repeat(64), "size": 10
                }]
            }))
            .unwrap()
        };
        assert!(PluginCatalog::parse(&entry("sub/board.zip"), &[]).is_ok());
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "/abs/board.zip",
            "a\\b.zip",
            "a b.zip",
            "",
        ] {
            assert!(PluginCatalog::parse(&entry(bad), &[]).is_err(), "{bad}");
        }
    }
}

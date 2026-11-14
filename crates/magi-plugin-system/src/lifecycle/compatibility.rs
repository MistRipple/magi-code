//! 全局兼容豁免沿既有资源 CAS 持久化，不归管理插件私有设置所有。
use super::*;
use magi_app_server_protocol::generated::{
    PluginDshCompatibilityProjection, PluginDshVersionExemptionRequest,
};

// 包 ID 的语法不允许前导下划线，外部插件不能寻址该内部 owner。
const OWNER: &str = "__magi";
const RESOURCE: &str = "dsh-compatibility";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    runtimes: BTreeMap<String, RuntimePolicy>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePolicy {
    version: String,
    targets: BTreeSet<String>,
}

impl PluginManager {
    pub fn ensure_dsh_candidate_compatible(
        &self,
        package: &PluginPackage,
        source: &PluginSource,
        runtime_version: &str,
        runtime_id: &str,
    ) -> Result<(), PluginError> {
        let PluginSource::Upstream {
            ecosystem,
            package: name,
            version,
            github,
        } = source
        else {
            return Ok(());
        };
        // 每个上游生态必须有自己的准入证据校验；没有校验器的生态不能放行。
        if ecosystem != "dsh" {
            return Err(invalid(format!(
                "上游生态 {ecosystem} 没有可用的兼容准入校验"
            )));
        }
        let report: Value = serde_json::from_slice(
            package
                .files()
                .get("extension/compat.json")
                .ok_or_else(|| invalid("DSH 译包缺少兼容证据"))?,
        )
        .map_err(|_| invalid("DSH 译包兼容证据无效"))?;
        let proof = &report["compatibility"];
        if report["upstream"]["name"] != *name
            || report["upstream"]["version"] != *version
            || proof["runtimeVersion"] != runtime_version
            || proof["runtimeId"] != runtime_id
            || report["upstream"]["github"]
                != serde_json::to_value(github).map_err(|_| invalid("GitHub 来源证据无效"))?
        {
            return Err(PluginError::Conflict(
                "DSH 译包的兼容运行时或来源已变化，请重新预览".into(),
            ));
        }
        if proof.get("requirements").is_none() {
            return Err(invalid("DSH 译包缺少版本准入结果"));
        }
        if !proof["requirements"].is_null() {
            let target = format!("{name}@{version}");
            let current = self.dsh_compatibility(runtime_version, runtime_id)?;
            if proof["exempted"] != true || !current.exemptions.contains_key(&target) {
                return Err(PluginError::NotAuthorized(
                    "DSH 精确版本豁免已撤销，请重新预览".into(),
                ));
            }
        }
        Ok(())
    }

    fn dsh_policy(&self) -> Result<(u64, Policy), PluginError> {
        match self.resources.read(OWNER, RESOURCE) {
            None => Ok((0, Policy::default())),
            Some(resource) => Ok((
                resource.version,
                serde_json::from_value(resource.value).map_err(|error| {
                    PluginError::CorruptState(format!("DSH 兼容策略损坏：{error}"))
                })?,
            )),
        }
    }

    pub fn dsh_compatibility(
        &self,
        runtime_version: &str,
        runtime_id: &str,
    ) -> Result<PluginDshCompatibilityProjection, PluginError> {
        validate_runtime(runtime_version, runtime_id)?;
        let (revision, policy) = self.dsh_policy()?;
        let mut exemptions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(runtime) = policy.runtimes.get(runtime_id) {
            if runtime.version != runtime_version {
                return Err(PluginError::CorruptState("DSH 运行时身份不一致".into()));
            }
            for target in &runtime.targets {
                exemptions.insert(target.clone(), vec![runtime.version.clone()]);
            }
        }
        Ok(PluginDshCompatibilityProjection {
            revision,
            runtime_version: runtime_version.into(),
            runtime_id: runtime_id.into(),
            exemptions,
        })
    }

    pub fn set_dsh_exemption(
        &mut self,
        runtime_version: &str,
        runtime_id: &str,
        request: &PluginDshVersionExemptionRequest,
    ) -> Result<PluginDshCompatibilityProjection, PluginError> {
        validate_runtime(runtime_version, runtime_id)?;
        validate_target(&request.target)?;
        if request.expected_runtime_id != runtime_id || request.runtime_version != runtime_version {
            return Err(PluginError::Conflict(
                "兼容运行时已变化，请刷新后再确认豁免".into(),
            ));
        }
        if request.enabled && !request.accept_risk {
            return Err(PluginError::NotAuthorized(
                "版本豁免必须明确确认运行风险".into(),
            ));
        }
        let (revision, mut policy) = self.dsh_policy()?;
        if revision != request.expected_revision {
            return Err(PluginError::Conflict(
                "版本豁免已被其他调用修改，请刷新后重试".into(),
            ));
        }
        if request.enabled {
            if !policy.runtimes.contains_key(runtime_id) && policy.runtimes.len() >= 32 {
                return Err(invalid("兼容运行时豁免记录超过上限"));
            }
            let runtime =
                policy
                    .runtimes
                    .entry(runtime_id.into())
                    .or_insert_with(|| RuntimePolicy {
                        version: runtime_version.into(),
                        targets: BTreeSet::new(),
                    });
            if runtime.targets.len() >= 512 && !runtime.targets.contains(&request.target) {
                return Err(invalid("版本豁免记录超过上限"));
            }
            runtime.targets.insert(request.target.clone());
        } else if let Some(runtime) = policy.runtimes.get_mut(runtime_id) {
            runtime.targets.remove(&request.target);
            if runtime.targets.is_empty() {
                policy.runtimes.remove(runtime_id);
            }
        }
        self.resources
            .write(
                OWNER,
                RESOURCE,
                revision,
                serde_json::to_value(policy)
                    .map_err(|error| PluginError::CorruptState(error.to_string()))?,
            )
            .map_err(PluginError::Resource)?;
        self.dsh_compatibility(runtime_version, runtime_id)
    }
}

fn validate_runtime(version: &str, id: &str) -> Result<(), PluginError> {
    if version.len() > 64
        || semver::Version::parse(version).is_err()
        || id.len() != 24
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("DSH 兼容运行时身份无效"));
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<(), PluginError> {
    let Some((name, version)) = target.rsplit_once('@') else {
        return Err(invalid("豁免目标必须是 npm 包名加精确版本"));
    };
    let valid_part = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['.', '_'])
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-._~".contains(&byte)
            })
    };
    let valid_name = match name.strip_prefix('@') {
        Some(scoped) => scoped
            .split_once('/')
            .is_some_and(|(scope, name)| valid_part(scope) && valid_part(name)),
        None => valid_part(name),
    };
    if name.len() > 214
        || !valid_name
        || version.len() > 64
        || semver::Version::parse(version).is_err()
    {
        return Err(invalid("豁免目标必须是 npm 包名加精确版本"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};
    const RUNTIME: &str = "0123456789abcdef01234567";

    #[test]
    fn candidate_exemption_is_rechecked_after_preview_and_cannot_follow_another_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(dir.path()).unwrap();
        let request = PluginDshVersionExemptionRequest {
            target: "example@1.2.3".into(),
            runtime_version: "0.2.2-rc.1".into(),
            expected_runtime_id: RUNTIME.into(),
            expected_revision: 0,
            enabled: true,
            accept_risk: true,
        };
        let policy = manager
            .set_dsh_exemption("0.2.2-rc.1", RUNTIME, &request)
            .unwrap();
        let manifest = json!({"sdkVersion":2,"id":"dsh.example","version":"1.2.3","name":"Example","description":"","extension":{"hooks":[]},"permissions":[{"kind":"extension","targets":["full-trust"]}],"dataSchemaVersion":1,"settingsSchema":{"type":"object"},"contributions":{"commands":[{"id":"status","title":"Status","description":""}]}});
        let report = json!({"upstream":{"name":"example","version":"1.2.3"},"compatibility":{"runtimeVersion":"0.2.2-rc.1","runtimeId":RUNTIME,"requirements":{"engines.dsh":"^9"},"exempted":true}});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, value) in [
            ("manifest.json", manifest),
            ("extension/compat.json", report),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&serde_json::to_vec(&value).unwrap()).unwrap();
        }
        zip.start_file(
            "extension/index.mjs",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"export default () => {};").unwrap();
        let package = PluginPackage::from_archive(&zip.finish().unwrap().into_inner()).unwrap();
        let source = PluginSource::Upstream {
            ecosystem: "dsh".into(),
            package: "example".into(),
            version: "1.2.3".into(),
            github: None,
        };
        manager
            .ensure_dsh_candidate_compatible(&package, &source, "0.2.2-rc.1", RUNTIME)
            .unwrap();
        assert!(
            manager
                .ensure_dsh_candidate_compatible(
                    &package,
                    &source,
                    "0.2.2-rc.1",
                    "abcdef0123456789abcdef01"
                )
                .is_err()
        );
        let other = PluginSource::Upstream {
            ecosystem: "dsh".into(),
            package: "another".into(),
            version: "1.2.3".into(),
            github: None,
        };
        assert!(
            manager
                .ensure_dsh_candidate_compatible(&package, &other, "0.2.2-rc.1", RUNTIME)
                .is_err()
        );
        manager
            .set_dsh_exemption(
                "0.2.2-rc.1",
                RUNTIME,
                &PluginDshVersionExemptionRequest {
                    expected_revision: policy.revision,
                    enabled: false,
                    ..request
                },
            )
            .unwrap();
        assert!(matches!(
            manager.ensure_dsh_candidate_compatible(&package, &source, "0.2.2-rc.1", RUNTIME),
            Err(PluginError::NotAuthorized(_))
        ));
        let unsupported = PluginSource::Upstream {
            ecosystem: "pi".into(),
            package: "example".into(),
            version: "1.2.3".into(),
            github: None,
        };
        assert!(
            manager
                .ensure_dsh_candidate_compatible(&package, &unsupported, "0.2.2-rc.1", RUNTIME)
                .is_err(),
            "没有准入校验器的上游生态不能默认放行"
        );
    }
    #[test]
    fn exemptions_persist_with_cas_and_do_not_follow_package_or_runtime_upgrades() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = PluginManager::open(dir.path()).unwrap();
        let mut request = PluginDshVersionExemptionRequest {
            target: "@scope/example@1.2.3".into(),
            runtime_version: "0.2.2-rc.1".into(),
            expected_runtime_id: RUNTIME.into(),
            expected_revision: 0,
            enabled: true,
            accept_risk: false,
        };
        assert!(
            manager
                .set_dsh_exemption("0.2.2-rc.1", RUNTIME, &request)
                .is_err()
        );
        request.accept_risk = true;
        let accepted = manager
            .set_dsh_exemption("0.2.2-rc.1", RUNTIME, &request)
            .unwrap();
        assert_eq!(accepted.exemptions[&request.target], ["0.2.2-rc.1"]);
        assert!(
            manager
                .set_dsh_exemption("0.2.2-rc.1", RUNTIME, &request)
                .is_err()
        );
        drop(manager);
        let mut manager = PluginManager::open(dir.path()).unwrap();
        assert_eq!(
            manager.dsh_compatibility("0.2.2-rc.1", RUNTIME).unwrap(),
            accepted
        );
        assert!(
            manager
                .dsh_compatibility("0.2.2-rc.1", "abcdef0123456789abcdef01")
                .unwrap()
                .exemptions
                .is_empty()
        );
        assert!(!accepted.exemptions.contains_key("@scope/example@1.2.4"));
        request.expected_revision = accepted.revision;
        assert!(
            manager
                .set_dsh_exemption("0.2.2-rc.1", "abcdef0123456789abcdef01", &request)
                .is_err()
        );
        request.target = "../unsafe@1.0.0".into();
        assert!(
            manager
                .set_dsh_exemption("0.2.2-rc.1", RUNTIME, &request)
                .is_err()
        );
        request.target = "@scope/example@1.2.3".into();
        request.enabled = false;
        request.accept_risk = false;
        assert!(
            manager
                .set_dsh_exemption("0.2.2-rc.1", RUNTIME, &request)
                .unwrap()
                .exemptions
                .is_empty()
        );
    }
}

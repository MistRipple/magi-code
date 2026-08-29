use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonIdentity {
    pub product_version: String,
    pub build_identity: String,
    pub startup_nonce: String,
}

impl DaemonIdentity {
    pub fn unknown() -> Self {
        Self {
            product_version: "unknown".to_string(),
            build_identity: "unknown".to_string(),
            startup_nonce: "unknown".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub service_name: String,
    pub api_version: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthDto {
    pub status: String,
    pub service_name: String,
    pub api_version: String,
    pub runtime_epoch: String,
    pub product_version: String,
    pub build_identity: String,
    pub startup_nonce: String,
}

impl HealthDto {
    pub fn from_service_info(
        service_info: &ServiceInfo,
        runtime_epoch: &str,
        identity: &DaemonIdentity,
    ) -> Self {
        Self {
            status: "ok".to_string(),
            service_name: service_info.service_name.clone(),
            api_version: service_info.api_version.clone(),
            runtime_epoch: runtime_epoch.to_string(),
            product_version: identity.product_version.clone(),
            build_identity: identity.build_identity.clone(),
            startup_nonce: identity.startup_nonce.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionHandshakeDto {
    pub service_name: String,
    pub api_version: String,
    pub min_supported_ui_version: String,
    pub host_scope: Vec<String>,
    pub product_version: String,
    pub build_identity: String,
    pub startup_nonce: String,
}

impl VersionHandshakeDto {
    pub fn from_service_info(service_info: &ServiceInfo, identity: &DaemonIdentity) -> Self {
        Self {
            service_name: service_info.service_name.clone(),
            api_version: service_info.api_version.clone(),
            min_supported_ui_version: "v0".to_string(),
            host_scope: vec!["vscode".to_string(), "idea".to_string()],
            product_version: identity.product_version.clone(),
            build_identity: identity.build_identity.clone(),
            startup_nonce: identity.startup_nonce.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_info() -> ServiceInfo {
        ServiceInfo {
            service_name: "magi".to_string(),
            api_version: "v0".to_string(),
        }
    }

    #[test]
    fn health_dto_is_derived_from_service_info() {
        let identity = DaemonIdentity {
            product_version: "3.0.51".to_string(),
            build_identity: "commit-test".to_string(),
            startup_nonce: "nonce-test".to_string(),
        };
        let dto = HealthDto::from_service_info(&service_info(), "runtime-test", &identity);

        assert_eq!(dto.status, "ok");
        assert_eq!(dto.service_name, "magi");
        assert_eq!(dto.api_version, "v0");
        assert_eq!(dto.runtime_epoch, "runtime-test");
        assert_eq!(dto.product_version, "3.0.51");
        assert_eq!(dto.build_identity, "commit-test");
        assert_eq!(dto.startup_nonce, "nonce-test");
    }

    #[test]
    fn version_handshake_uses_service_api_version() {
        let identity = DaemonIdentity {
            product_version: "3.0.51".to_string(),
            build_identity: "commit-test".to_string(),
            startup_nonce: "nonce-test".to_string(),
        };
        let dto = VersionHandshakeDto::from_service_info(&service_info(), &identity);

        assert_eq!(dto.service_name, "magi");
        assert_eq!(dto.api_version, "v0");
        assert_eq!(dto.min_supported_ui_version, "v0");
        assert_eq!(dto.host_scope, vec!["vscode", "idea"]);
        assert_eq!(dto.product_version, "3.0.51");
        assert_eq!(dto.build_identity, "commit-test");
        assert_eq!(dto.startup_nonce, "nonce-test");
    }
}

use serde::Serialize;

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
}

impl HealthDto {
    pub fn from_service_info(service_info: &ServiceInfo) -> Self {
        Self {
            status: "ok".to_string(),
            service_name: service_info.service_name.clone(),
            api_version: service_info.api_version.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionHandshakeDto {
    pub api_version: String,
    pub min_supported_ui_version: String,
    pub host_scope: Vec<String>,
}

impl VersionHandshakeDto {
    pub fn from_service_info(service_info: &ServiceInfo) -> Self {
        Self {
            api_version: service_info.api_version.clone(),
            min_supported_ui_version: "v0-shadow".to_string(),
            host_scope: vec!["vscode".to_string(), "idea".to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_info() -> ServiceInfo {
        ServiceInfo {
            service_name: "magi".to_string(),
            api_version: "v0-shadow".to_string(),
        }
    }

    #[test]
    fn health_dto_is_derived_from_service_info() {
        let dto = HealthDto::from_service_info(&service_info());

        assert_eq!(dto.status, "ok");
        assert_eq!(dto.service_name, "magi");
        assert_eq!(dto.api_version, "v0-shadow");
    }

    #[test]
    fn version_handshake_uses_service_api_version() {
        let dto = VersionHandshakeDto::from_service_info(&service_info());

        assert_eq!(dto.api_version, "v0-shadow");
        assert_eq!(dto.min_supported_ui_version, "v0-shadow");
        assert_eq!(dto.host_scope, vec!["vscode", "idea"]);
    }
}

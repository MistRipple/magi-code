use sha2::{Digest, Sha256};

use crate::types::{ExecutionBindingIdentity, LlmConfig, ModelResolutionIdentity, UrlMode};

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn canonicalize_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match url::Url::parse(trimmed) {
        Ok(parsed) => {
            let scheme = parsed.scheme().to_lowercase();
            let host = parsed.host_str().unwrap_or("").to_lowercase();
            let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
            let path = parsed.path().trim_end_matches('/');
            format!("{scheme}://{host}{port}{path}")
        }
        Err(_) => trimmed.trim_end_matches('/').to_lowercase(),
    }
}

fn fingerprint_secret(secret: Option<&str>) -> Option<String> {
    let normalized = secret.map(|s| s.trim().to_string()).unwrap_or_default();
    if normalized.is_empty() {
        return None;
    }
    Some(sha256_hex(&normalized)[..16].to_string())
}

pub fn build_model_resolution_identity(
    model_config: &LlmConfig,
    execution_binding: &ExecutionBindingIdentity,
    declared_model_spec: Option<&str>,
    effective_context_modifiers: Option<&[String]>,
) -> ModelResolutionIdentity {
    let canonical_base_url = canonicalize_base_url(&model_config.base_url);
    let base_url_fingerprint = if canonical_base_url.is_empty() {
        sha256_hex("missing-base-url")[..16].to_string()
    } else {
        sha256_hex(&canonical_base_url)[..16].to_string()
    };
    let declared = declared_model_spec
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let m = model_config.model.trim();
            if m.is_empty() { "unknown" } else { m }
        })
        .to_string();
    let resolved = {
        let m = model_config.model.trim();
        if m.is_empty() { "unknown" } else { m }
    }
    .to_string();
    let account_fingerprint = fingerprint_secret(model_config.api_key.as_deref());
    let url_mode_str = match model_config.url_mode {
        UrlMode::Full => "full",
        UrlMode::Proxy => "proxy",
        UrlMode::Default => "default",
    };
    let identity_parts = [
        model_config.provider.as_str(),
        &canonical_base_url,
        url_mode_str,
        account_fingerprint.as_deref().unwrap_or(""),
        &resolved,
        &execution_binding.binding_revision.to_string(),
    ];
    let model_identity_key = sha256_hex(&identity_parts.join("|"));

    let ecm = effective_context_modifiers
        .filter(|m| !m.is_empty())
        .map(|m| m.to_vec());

    ModelResolutionIdentity {
        provider: model_config.provider.clone(),
        declared_model_spec: declared,
        resolved_model: resolved,
        canonical_base_url,
        base_url_fingerprint,
        url_mode: model_config.url_mode,
        account_fingerprint,
        binding_revision: execution_binding.binding_revision,
        reasoning_effort: model_config.reasoning_effort,
        effective_context_modifiers: ecm,
        model_identity_key,
    }
}

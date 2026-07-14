use crate::{BridgeClientError, BridgeErrorLayer};
use base64::Engine as _;
use serde_json::{Value, json};
use std::time::Duration;

const IMAGE_GENERATION_TIMEOUT_SECS: u64 = 180;
const IMAGE_GENERATION_MAX_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageGenerationUrlMode {
    Standard,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub size: String,
    pub quality: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub revised_prompt: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HttpImageGenerationClient {
    base_url: String,
    api_key: Option<String>,
    model: String,
    url_mode: ImageGenerationUrlMode,
}

#[derive(Clone, Debug)]
pub struct BuiltImageGenerationRequest {
    pub url: String,
    pub body: Value,
    pub headers: Vec<(String, String)>,
}

impl HttpImageGenerationClient {
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        model: String,
        url_mode: ImageGenerationUrlMode,
    ) -> Self {
        Self {
            base_url,
            api_key,
            model,
            url_mode,
        }
    }

    pub fn generate(
        &self,
        request: ImageGenerationRequest,
    ) -> Result<GeneratedImage, BridgeClientError> {
        let built = self.build_request(&request)?;
        let response_body = execute_image_generation_post(built)?;
        parse_image_generation_response(&response_body)
    }

    fn build_request(
        &self,
        request: &ImageGenerationRequest,
    ) -> Result<BuiltImageGenerationRequest, BridgeClientError> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(protocol_error("image prompt is empty"));
        }
        let model = self.model.trim();
        if model.is_empty() {
            return Err(protocol_error("image model is empty"));
        }
        let url = build_image_generation_url(&self.base_url, self.url_mode)
            .map_err(|reason| protocol_error(&reason))?;
        let mut body = json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
            "size": request.size,
            "response_format": "b64_json",
        });
        if let Some(quality) = request
            .quality
            .as_deref()
            .map(str::trim)
            .filter(|quality| !quality.is_empty())
        {
            body["quality"] = Value::String(quality.to_string());
        }
        let headers = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(|key| vec![("Authorization".to_string(), format!("Bearer {key}"))])
            .unwrap_or_default();
        Ok(BuiltImageGenerationRequest { url, body, headers })
    }

    #[cfg(test)]
    pub fn build_request_for_test(
        &self,
        request: &ImageGenerationRequest,
    ) -> Result<BuiltImageGenerationRequest, BridgeClientError> {
        self.build_request(request)
    }

    #[cfg(test)]
    pub fn parse_response_for_test(
        response_body: &str,
    ) -> Result<GeneratedImage, BridgeClientError> {
        parse_image_generation_response(response_body)
    }
}

fn build_image_generation_url(
    base_url: &str,
    url_mode: ImageGenerationUrlMode,
) -> Result<String, String> {
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return Err("image generation base_url is empty".to_string());
    }
    if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
        return Err("image generation base_url must use http or https".to_string());
    }
    if url_mode == ImageGenerationUrlMode::Full {
        return Ok(normalized.to_string());
    }
    if normalized.ends_with("/v1") {
        Ok(format!("{normalized}/images/generations"))
    } else {
        Ok(format!("{normalized}/v1/images/generations"))
    }
}

fn execute_image_generation_post(
    request: BuiltImageGenerationRequest,
) -> Result<String, BridgeClientError> {
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(IMAGE_GENERATION_TIMEOUT_SECS))
            .build()
            .map_err(|_| transport_error("image generation HTTP client build failed"))?;
        let mut builder = client
            .post(request.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .send()
            .map_err(|_| transport_error("image generation provider is unreachable"))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .map_err(|_| transport_error("image generation response could not be read"))?;
        if !(200..300).contains(&status) {
            return Err(BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::RemoteBusiness,
                code: Some(i64::from(status)),
                message: format!("image generation provider rejected request with HTTP {status}"),
            });
        }
        Ok(body)
    })
    .join()
    .map_err(|_| transport_error("image generation request thread panicked"))?
}

fn parse_image_generation_response(
    response_body: &str,
) -> Result<GeneratedImage, BridgeClientError> {
    let payload: Value = serde_json::from_str(response_body)
        .map_err(|_| protocol_error("image generation response is not valid JSON"))?;
    let item = payload
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| protocol_error("image generation response has no image data"))?;
    let encoded = item
        .get("b64_json")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_error("image generation response has no b64_json"))?;
    if encoded.len() > IMAGE_GENERATION_MAX_BYTES.saturating_mul(2) {
        return Err(protocol_error("generated image payload exceeds size limit"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| protocol_error("generated image base64 is invalid"))?;
    if bytes.len() > IMAGE_GENERATION_MAX_BYTES {
        return Err(protocol_error("generated image exceeds size limit"));
    }
    let media_type = detect_image_media_type(&bytes)
        .ok_or_else(|| protocol_error("generated image format is unsupported"))?;
    let revised_prompt = item
        .get("revised_prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(GeneratedImage {
        bytes,
        media_type: media_type.to_string(),
        revised_prompt,
    })
}

fn detect_image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn protocol_error(message: &str) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Protocol,
        code: Some(-32007),
        message: message.to_string(),
    }
}

fn transport_error(message: &str) -> BridgeClientError {
    BridgeClientError::CallFailed {
        layer: BridgeErrorLayer::Transport,
        code: Some(-32005),
        message: message.to_string(),
    }
}

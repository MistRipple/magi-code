use crate::{BridgeClientError, BridgeErrorLayer};
use base64::Engine as _;
use serde_json::{Value, json};
use std::{io::Read, time::Duration};

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
    pub usage: Option<Value>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageGenerationProviderProfile {
    OpenAiCompatible,
    Xai,
}

enum GeneratedImageSource {
    Inline(Vec<u8>),
    RemoteUrl(String),
}

struct ParsedImageGenerationResponse {
    source: GeneratedImageSource,
    revised_prompt: Option<String>,
    usage: Option<Value>,
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
        execute_image_generation_request(built)
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
        let profile = image_generation_provider_profile(model);
        let mut body = json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
            "response_format": "b64_json",
        });
        match profile {
            ImageGenerationProviderProfile::OpenAiCompatible => {
                body["size"] = Value::String(request.size.clone());
                if let Some(quality) = request
                    .quality
                    .as_deref()
                    .map(str::trim)
                    .filter(|quality| !quality.is_empty())
                {
                    body["quality"] = Value::String(quality.to_string());
                }
            }
            ImageGenerationProviderProfile::Xai => {
                let (aspect_ratio, resolution) = xai_dimensions(&request.size)?;
                if let Some(aspect_ratio) = aspect_ratio {
                    body["aspect_ratio"] = Value::String(aspect_ratio);
                }
                if let Some(resolution) = resolution {
                    body["resolution"] = Value::String(resolution);
                }
            }
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
        let parsed = parse_image_generation_response(response_body)?;
        match parsed.source {
            GeneratedImageSource::Inline(bytes) => {
                generated_image_from_bytes(bytes, parsed.revised_prompt, parsed.usage)
            }
            GeneratedImageSource::RemoteUrl(_) => Err(protocol_error(
                "image generation URL response requires an HTTP client",
            )),
        }
    }
}

fn image_generation_provider_profile(model: &str) -> ImageGenerationProviderProfile {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.starts_with("grok-imagine-image") || normalized.starts_with("grok-2-image") {
        ImageGenerationProviderProfile::Xai
    } else {
        ImageGenerationProviderProfile::OpenAiCompatible
    }
}

fn xai_dimensions(size: &str) -> Result<(Option<String>, Option<String>), BridgeClientError> {
    let normalized = size.trim().to_ascii_lowercase();
    if normalized == "auto" {
        return Ok((Some("auto".to_string()), None));
    }
    let Some((width, height)) = normalized.split_once('x') else {
        return Err(protocol_error(
            "xAI image size must use WIDTHxHEIGHT or auto",
        ));
    };
    let width = width
        .parse::<u32>()
        .map_err(|_| protocol_error("xAI image width is invalid"))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| protocol_error("xAI image height is invalid"))?;
    if width == 0 || height == 0 {
        return Err(protocol_error("xAI image dimensions must be positive"));
    }
    let divisor = greatest_common_divisor(width, height);
    let reduced_width = width / divisor;
    let reduced_height = height / divisor;
    let ratio = match (reduced_width, reduced_height) {
        (13, 6) => "19.5:9".to_string(),
        (6, 13) => "9:19.5".to_string(),
        _ => format!("{reduced_width}:{reduced_height}"),
    };
    const SUPPORTED_RATIOS: &[&str] = &[
        "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3", "2:1", "1:2", "20:9", "9:20", "19.5:9",
        "9:19.5",
    ];
    if !SUPPORTED_RATIOS.contains(&ratio.as_str()) {
        return Err(protocol_error(
            "xAI image model does not support the requested aspect ratio",
        ));
    }
    let longest_edge = width.max(height);
    let resolution = match longest_edge {
        0..=1024 => "1k",
        1025..=2048 => "2k",
        _ => {
            return Err(protocol_error(
                "xAI image model supports dimensions up to 2k",
            ));
        }
    };
    Ok((Some(ratio), Some(resolution.to_string())))
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
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

fn execute_image_generation_request(
    request: BuiltImageGenerationRequest,
) -> Result<GeneratedImage, BridgeClientError> {
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(IMAGE_GENERATION_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(5))
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
        let parsed = parse_image_generation_response(&body)?;
        let bytes = match parsed.source {
            GeneratedImageSource::Inline(bytes) => bytes,
            GeneratedImageSource::RemoteUrl(url) => download_generated_image(&client, &url)?,
        };
        generated_image_from_bytes(bytes, parsed.revised_prompt, parsed.usage)
    })
    .join()
    .map_err(|_| transport_error("image generation request thread panicked"))?
}

fn parse_image_generation_response(
    response_body: &str,
) -> Result<ParsedImageGenerationResponse, BridgeClientError> {
    let payload: Value = serde_json::from_str(response_body)
        .map_err(|_| protocol_error("image generation response is not valid JSON"))?;
    let item = payload
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| protocol_error("image generation response has no image data"))?;
    let source = if let Some(encoded) = item
        .get("b64_json")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if encoded.len() > IMAGE_GENERATION_MAX_BYTES.saturating_mul(2) {
            return Err(protocol_error("generated image payload exceeds size limit"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| protocol_error("generated image base64 is invalid"))?;
        GeneratedImageSource::Inline(bytes)
    } else if let Some(url) = item
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        GeneratedImageSource::RemoteUrl(url.to_string())
    } else {
        return Err(protocol_error(
            "image generation response has no b64_json or URL",
        ));
    };
    let revised_prompt = item
        .get("revised_prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let usage = payload
        .get("usage")
        .filter(|value| value.is_object())
        .cloned();
    Ok(ParsedImageGenerationResponse {
        source,
        revised_prompt,
        usage,
    })
}

fn download_generated_image(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<u8>, BridgeClientError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| protocol_error("generated image URL is invalid"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(protocol_error("generated image URL must use http or https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(protocol_error(
            "generated image URL must not contain credentials",
        ));
    }
    let mut response = client
        .get(parsed)
        .header("Accept", "image/png,image/jpeg,image/webp,image/*;q=0.8")
        .send()
        .map_err(|_| transport_error("generated image URL is unreachable"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(i64::from(status)),
            message: format!("generated image download failed with HTTP {status}"),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > IMAGE_GENERATION_MAX_BYTES as u64)
    {
        return Err(protocol_error("generated image exceeds size limit"));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take((IMAGE_GENERATION_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| transport_error("generated image download could not be read"))?;
    if bytes.len() > IMAGE_GENERATION_MAX_BYTES {
        return Err(protocol_error("generated image exceeds size limit"));
    }
    Ok(bytes)
}

fn generated_image_from_bytes(
    bytes: Vec<u8>,
    revised_prompt: Option<String>,
    usage: Option<Value>,
) -> Result<GeneratedImage, BridgeClientError> {
    if bytes.len() > IMAGE_GENERATION_MAX_BYTES {
        return Err(protocol_error("generated image exceeds size limit"));
    }
    let media_type = detect_image_media_type(&bytes)
        .ok_or_else(|| protocol_error("generated image format is unsupported"))?;
    Ok(GeneratedImage {
        bytes,
        media_type: media_type.to_string(),
        revised_prompt,
        usage,
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

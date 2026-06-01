use magi_bridge_client::llm_types::ImageSource;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTurnImage {
    pub name: String,
    pub data_url: String,
    pub source: ImageSource,
}

impl SessionTurnImage {
    pub fn from_data_url(
        name: impl Into<String>,
        data_url: impl Into<String>,
    ) -> Result<Self, String> {
        let name = name.into().trim().to_string();
        let raw_data_url = data_url.into();
        let data_url = raw_data_url.trim();
        let Some(rest) = data_url.strip_prefix("data:") else {
            return Err("图片必须使用 data URL".to_string());
        };
        let Some((header, data)) = rest.split_once(',') else {
            return Err("图片 data URL 缺少 base64 数据".to_string());
        };
        let mut header_parts = header.split(';');
        let media_type = header_parts.next().unwrap_or_default().trim();
        if !media_type.starts_with("image/") {
            return Err("仅支持 image/* 类型的图片".to_string());
        }
        if !header_parts.any(|part| part.trim().eq_ignore_ascii_case("base64")) {
            return Err("图片 data URL 必须使用 base64 编码".to_string());
        }
        let data = data.trim();
        if data.is_empty() {
            return Err("图片 base64 数据不能为空".to_string());
        }
        Ok(Self {
            name: if name.is_empty() {
                "image".to_string()
            } else {
                name
            },
            data_url: format!("data:{media_type};base64,{data}"),
            source: ImageSource {
                kind: "base64".to_string(),
                media_type: media_type.to_string(),
                data: data.to_string(),
            },
        })
    }
}

pub fn session_turn_image_sources(images: &[SessionTurnImage]) -> Vec<ImageSource> {
    images.iter().map(|image| image.source.clone()).collect()
}

pub fn image_sources_from_metadata(metadata: &HashMap<String, Value>) -> Vec<ImageSource> {
    metadata
        .get("images")
        .and_then(Value::as_array)
        .map(|images| {
            images
                .iter()
                .filter_map(|image| {
                    let data_url = image.get("dataUrl").and_then(Value::as_str)?;
                    SessionTurnImage::from_data_url(
                        image.get("name").and_then(Value::as_str).unwrap_or("image"),
                        data_url,
                    )
                    .ok()
                    .map(|image| image.source)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn session_turn_images_metadata(images: &[SessionTurnImage]) -> HashMap<String, Value> {
    if images.is_empty() {
        return HashMap::new();
    }
    let mut metadata = HashMap::new();
    metadata.insert(
        "images".to_string(),
        Value::Array(
            images
                .iter()
                .map(|image| {
                    json!({
                        "name": image.name,
                        "dataUrl": image.data_url,
                    })
                })
                .collect(),
        ),
    );
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_data_url_for_model_and_ui_metadata() {
        let image = SessionTurnImage::from_data_url("paste.png", "data:image/png;base64,AAA")
            .expect("image data url should parse");

        assert_eq!(image.name, "paste.png");
        assert_eq!(image.data_url, "data:image/png;base64,AAA");
        assert_eq!(image.source.media_type, "image/png");
        assert_eq!(image.source.data, "AAA");

        let metadata = session_turn_images_metadata(&[image]);
        assert_eq!(metadata["images"][0]["name"], "paste.png");
        assert_eq!(
            metadata["images"][0]["dataUrl"],
            "data:image/png;base64,AAA"
        );
    }
}

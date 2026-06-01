use crate::{
    BuiltinToolAccessMode, ToolExecutionContext,
    builtin::{field_string, parse_json_object, resolve_path_with_context},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::fmt::Display;
use std::fs;
use std::path::Path;

const TOOL_NAME: &str = "view_image";
const DEFAULT_MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const HARD_MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const IMAGE_ACCESS_PUBLIC_ERROR: &str = "图片不可读取或不存在";

pub(crate) fn execute_view_image(input: &str, context: &ToolExecutionContext) -> String {
    let request = parse_json_object(input);
    let path_value = match requested_path(input, request.as_ref()) {
        Ok(path) => path,
        Err(error) => return view_image_error(error),
    };
    let max_bytes = request
        .as_ref()
        .and_then(|object| field_u64(object, &["max_bytes", "maxBytes"]))
        .unwrap_or(DEFAULT_MAX_IMAGE_BYTES)
        .min(HARD_MAX_IMAGE_BYTES);

    let path = match resolve_path_with_context(&path_value, context) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(
                requested_path = %path_value,
                error = %error,
                "view_image path resolution failed"
            );
            return view_image_error("图片路径不可解析");
        }
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return view_image_access_error("读取图片元数据失败", &path, error),
    };
    if !metadata.is_file() {
        return view_image_error("view_image 只能读取图片文件");
    }
    if metadata.len() > max_bytes {
        return view_image_error(format!(
            "图片超过大小限制: {} bytes > {} bytes",
            metadata.len(),
            max_bytes
        ));
    }

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => return view_image_access_error("读取图片失败", &path, error),
    };
    let mime = match detect_supported_image_mime(&bytes) {
        Some(mime) => mime,
        None => return view_image_error("不支持或无效的图片格式，支持 png/jpeg/gif/webp"),
    };
    let data = STANDARD.encode(&bytes);
    let summary = format!(
        "已读取图片 {} (mime={}, bytes={})",
        path.display(),
        mime,
        bytes.len()
    );

    serde_json::json!({
        "tool": TOOL_NAME,
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "path": path.display().to_string(),
        "mime": mime,
        "bytes": bytes.len(),
        "summary": summary,
        "model_content": [
            {
                "type": "text",
                "text": summary,
            },
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime,
                    "data": data,
                }
            }
        ]
    })
    .to_string()
}

fn requested_path(
    input: &str,
    request: Option<&serde_json::Map<String, Value>>,
) -> Result<String, String> {
    let value = match request {
        Some(object) => field_string(
            object,
            &["path", "file_path", "filePath", "image_path", "imagePath"],
        ),
        None => Some(input.trim().to_string()),
    }
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());

    value.ok_or_else(|| "view_image 需要 path 字段或原始路径字符串".to_string())
}

fn field_u64(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        object.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
        })
    })
}

fn detect_supported_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.len() >= 3 && bytes[0..3] == [0xff, 0xd8, 0xff] {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn view_image_error(message: impl Into<String>) -> String {
    serde_json::json!({
        "tool": TOOL_NAME,
        "status": "failed",
        "access_mode": BuiltinToolAccessMode::ReadOnly.as_str(),
        "error": message.into(),
    })
    .to_string()
}

fn view_image_access_error(action: &'static str, path: &Path, error: impl Display) -> String {
    tracing::warn!(
        action,
        path = %path.display(),
        error = %error,
        "view_image file access failed"
    );
    view_image_error(IMAGE_ACCESS_PUBLIC_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    const ONE_BY_ONE_PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, b'I',
        b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
    ];

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}-{}", name, std::process::id(), suffix));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn context(root: PathBuf) -> ToolExecutionContext {
        ToolExecutionContext {
            working_directory: Some(root),
            ..ToolExecutionContext::default()
        }
    }

    #[test]
    fn view_image_returns_model_content_for_supported_image() {
        let root = unique_temp_dir("magi-view-image-supported");
        let image_path = root.join("pixel.png");
        fs::write(&image_path, ONE_BY_ONE_PNG).expect("write png");

        let output = execute_view_image(
            &serde_json::json!({ "path": "pixel.png" }).to_string(),
            &context(root),
        );
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["mime"], "image/png");
        assert_eq!(payload["model_content"][0]["type"], "text");
        assert_eq!(payload["model_content"][1]["type"], "image");
        assert_eq!(
            payload["model_content"][1]["source"]["media_type"],
            "image/png"
        );
        assert!(
            payload["model_content"][1]["source"]["data"]
                .as_str()
                .expect("base64 data")
                .len()
                > 20
        );
    }

    #[test]
    fn view_image_accepts_camel_case_path_alias() {
        let root = unique_temp_dir("magi-view-image-file-path");
        fs::write(root.join("pixel.png"), ONE_BY_ONE_PNG).expect("write png");

        let output = execute_view_image(
            &serde_json::json!({ "filePath": "pixel.png" }).to_string(),
            &context(root),
        );
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["mime"], "image/png");
    }

    #[test]
    fn view_image_rejects_non_image_payload() {
        let root = unique_temp_dir("magi-view-image-invalid");
        fs::write(root.join("not-image.txt"), b"hello").expect("write text");

        let output = execute_view_image("not-image.txt", &context(root));
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(
            payload["error"],
            "不支持或无效的图片格式，支持 png/jpeg/gif/webp"
        );
        assert!(
            !payload.to_string().contains("not-image.txt"),
            "view_image failure should not echo file paths"
        );
    }

    #[test]
    fn view_image_hides_file_access_details() {
        let root = unique_temp_dir("magi-view-image-missing");

        let output = execute_view_image("missing.png", &context(root));
        let payload: Value = serde_json::from_str(&output).expect("json output");

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error"], IMAGE_ACCESS_PUBLIC_ERROR);
        let text = payload.to_string();
        assert!(
            !text.contains("missing.png")
                && !text.contains("No such file")
                && !text.contains("os error"),
            "view_image failure should not expose path or io details: {text}"
        );
    }
}

use crate::{
    BuiltinToolAccessMode, GeneratedImageData, ImageGenerationRequest, ToolExecutionContext,
    ToolRuntimeResources, canonicalize_tool_permission_path,
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const TOOL_NAME: &str = "image_generate";
const DEFAULT_SIZE: &str = "1024x1024";
const PUBLIC_PROVIDER_ERROR: &str = "图片生成服务暂不可用，请检查图片模型配置后重试";
const PUBLIC_WRITE_ERROR: &str = "生成图片暂不可保存，请检查工作区路径或权限";
const MAX_OUTPUT_NAME_ATTEMPTS: u32 = 10_000;

pub(crate) fn execute_image_generate(
    input: &str,
    context: &ToolExecutionContext,
    resources: &ToolRuntimeResources,
) -> String {
    let request = match parse_request(input) {
        Ok(request) => request,
        Err(error) => return image_generation_error("image_generate_invalid_input", error),
    };
    if !resources
        .image_generation_readiness_provider
        .as_ref()
        .is_some_and(|provider| provider())
    {
        return image_generation_error(
            "image_generate_not_configured",
            "图片生成模型尚未配置或未启用",
        );
    }
    let Some(executor) = resources.image_generation_executor.as_ref() else {
        return image_generation_error(
            "image_generate_not_configured",
            "图片生成模型尚未配置或未启用",
        );
    };
    let workspace_root = match canonical_workspace_root(context) {
        Ok(root) => root,
        Err(error) => return image_generation_error("image_generate_workspace_required", error),
    };
    let requested_output_path =
        match resolve_requested_output_path(&workspace_root, request.output_path.as_deref()) {
            Ok(path) => path,
            Err(error) => {
                return image_generation_error("image_generate_invalid_output_path", error);
            }
        };

    let generated = match executor(ImageGenerationRequest {
        prompt: request.prompt.clone(),
        size: request.size.clone(),
        quality: request.quality.clone(),
    }) {
        Ok(generated) => generated,
        Err(error) => {
            tracing::warn!(error = %error, "image generation provider request failed");
            return image_generation_error("image_generate_provider_failed", PUBLIC_PROVIDER_ERROR);
        }
    };

    let extension = match image_extension(&generated) {
        Ok(extension) => extension,
        Err(error) => return image_generation_error("image_generate_invalid_result", error),
    };
    let requested_output_path =
        match resolve_output_path(&workspace_root, requested_output_path.as_deref(), extension) {
            Ok(path) => path,
            Err(error) => {
                return image_generation_error("image_generate_invalid_output_path", error);
            }
        };
    let output_path = match write_generated_image(
        &workspace_root,
        &requested_output_path,
        &generated.bytes,
    ) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(path = %requested_output_path.display(), error = %error, "generated image write failed");
            return image_generation_error("image_generate_write_failed", PUBLIC_WRITE_ERROR);
        }
    };

    let relative_path = output_path
        .strip_prefix(&workspace_root)
        .unwrap_or(&output_path)
        .to_string_lossy()
        .to_string();
    serde_json::json!({
        "tool": TOOL_NAME,
        "status": "succeeded",
        "access_mode": BuiltinToolAccessMode::ExplicitWrite.as_str(),
        "path": relative_path,
        "media_type": generated.media_type,
        "bytes": generated.bytes.len(),
        "size": request.size,
        "quality": request.quality,
        "revised_prompt": generated.revised_prompt,
        "summary": format!("已生成图片 {}", relative_path),
    })
    .to_string()
}

struct ParsedRequest {
    prompt: String,
    size: String,
    quality: Option<String>,
    output_path: Option<String>,
}

fn parse_request(input: &str) -> Result<ParsedRequest, String> {
    let object = serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "输入必须为 JSON 对象".to_string())?;
    let prompt = object
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "image_generate 需要非空 prompt 字段".to_string())?
        .to_string();
    let size = object
        .get("size")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SIZE)
        .to_string();
    validate_size(&size)?;
    let quality = object
        .get("quality")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    if let Some(quality) = quality.as_deref()
        && !matches!(
            quality,
            "auto" | "low" | "medium" | "high" | "standard" | "hd"
        )
    {
        return Err("quality 仅支持 auto/low/medium/high/standard/hd".to_string());
    }
    let output_path = object
        .get("output_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    Ok(ParsedRequest {
        prompt,
        size,
        quality,
        output_path,
    })
}

fn validate_size(size: &str) -> Result<(), String> {
    if size == "auto" {
        return Ok(());
    }
    let Some((width, height)) = size.split_once('x') else {
        return Err("size 必须为 auto 或 WIDTHxHEIGHT".to_string());
    };
    let width = width
        .parse::<u32>()
        .map_err(|_| "size 宽度必须为整数".to_string())?;
    let height = height
        .parse::<u32>()
        .map_err(|_| "size 高度必须为整数".to_string())?;
    if !(64..=4096).contains(&width) || !(64..=4096).contains(&height) {
        return Err("图片宽高必须位于 64 到 4096 像素之间".to_string());
    }
    Ok(())
}

fn canonical_workspace_root(context: &ToolExecutionContext) -> Result<PathBuf, String> {
    let root = context
        .working_directory
        .as_deref()
        .ok_or_else(|| "缺少当前工作区目录，无法保存生成图片".to_string())?;
    root.canonicalize()
        .map_err(|_| "当前工作区目录不可访问，无法保存生成图片".to_string())
}

fn resolve_output_path(
    workspace_root: &Path,
    requested: Option<&Path>,
    extension: &str,
) -> Result<PathBuf, String> {
    let requested_path = match requested {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from("generated-images").join(format!(
            "image-{}.{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "系统时间异常，无法生成文件名".to_string())?
                .as_millis(),
            extension
        )),
    };
    let mut candidate = if requested_path.is_absolute() {
        requested_path
    } else {
        workspace_root.join(requested_path)
    };
    candidate.set_extension(extension);
    let candidate = canonicalize_tool_permission_path(&candidate);
    if !candidate.starts_with(workspace_root) {
        return Err("output_path 必须位于当前工作区内".to_string());
    }
    if candidate == workspace_root || candidate.file_name().is_none() {
        return Err("output_path 必须指向具体图片文件".to_string());
    }
    Ok(candidate)
}

fn resolve_requested_output_path(
    workspace_root: &Path,
    requested: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    let candidate = magi_core::HostPath::resolve_native_input(
        requested,
        Some(workspace_root),
        dirs::home_dir().as_deref(),
    )
    .map(magi_core::HostPath::into_path_buf)
    .map_err(|error| error.to_string())?;
    let candidate = canonicalize_tool_permission_path(&candidate);
    if !candidate.starts_with(workspace_root) {
        return Err("output_path 必须位于当前工作区内".to_string());
    }
    if candidate == workspace_root || candidate.file_name().is_none() {
        return Err("output_path 必须指向具体图片文件".to_string());
    }
    Ok(Some(candidate))
}

fn image_extension(generated: &GeneratedImageData) -> Result<&'static str, String> {
    match generated.media_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => Ok("png"),
        "image/jpeg" | "image/jpg" => Ok("jpg"),
        "image/webp" => Ok("webp"),
        _ => Err("图片生成服务返回了不支持的图片格式".to_string()),
    }
}

fn write_generated_image(
    root: &Path,
    output_path: &Path,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    let parent = output_path
        .parent()
        .ok_or_else(|| std::io::Error::other("output path has no parent"))?;
    fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize()?;
    if !canonical_parent.starts_with(root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "output parent escaped workspace",
        ));
    }
    for attempt in 0..MAX_OUTPUT_NAME_ATTEMPTS {
        let candidate = if attempt == 0 {
            output_path.to_path_buf()
        } else {
            output_path_with_suffix(output_path, attempt)
        };
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        file.write_all(bytes)?;
        return Ok(candidate);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "无法为生成图片分配未占用的文件名",
    ))
}

fn output_path_with_suffix(path: &Path, suffix: u32) -> PathBuf {
    let mut file_name = path
        .file_stem()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    file_name.push(format!("-{suffix}"));
    if let Some(extension) = path.extension() {
        file_name.push(".");
        file_name.push(extension);
    }
    path.with_file_name(file_name)
}

fn image_generation_error(code: &str, message: impl Into<String>) -> String {
    serde_json::json!({
        "tool": TOOL_NAME,
        "status": "failed",
        "error_code": code,
        "error": message.into(),
    })
    .to_string()
}

//! 插件私有图片：元数据沿现有资源事实持久化，图片文件只是内容寻址载荷。
//! 不写 Session；用户发送图片后，canonical 消息沿原入口保存自己的原始字节。
use super::*;
use base64::Engine as _;
use magi_app_server_protocol::generated::{
    PluginImageAttachment, PluginImageAttachmentBegin, PluginImageAttachmentChunk,
    PluginImageAttachmentRead,
};
use sha2::{Digest, Sha256};

const PREFIX: &str = "__image:";
const MAX_BYTES: usize = 10 * 1024 * 1024;
const MAX_PLUGIN_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PLUGIN_IMAGES: usize = 1024;

fn digest(bytes: &[u8]) -> String {
    magi_core::hex_lower(Sha256::digest(bytes))
}
fn image_key(id: &str) -> Result<String, PluginError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(PluginError::InvalidInput("图片附件身份无效".into()));
    }
    Ok(format!("{PREFIX}{id}"))
}
fn metadata(value: Value) -> Result<PluginImageAttachment, PluginError> {
    let result: PluginImageAttachment =
        serde_json::from_value(value).map_err(|e| PluginError::CorruptState(e.to_string()))?;
    if result.sha256.len() != 64
        || !result
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || result.bytes == 0
        || result.bytes > MAX_BYTES as u64
    {
        return Err(PluginError::CorruptState("图片附件元数据无效".into()));
    }
    Ok(result)
}
fn validate_image(input: &PluginImageAttachmentBegin, bytes: &[u8]) -> Result<(), PluginError> {
    if bytes.is_empty()
        || bytes.len() > MAX_BYTES
        || bytes.len() as u64 != input.bytes
        || input.name.is_empty()
        || input.name.len() > 256
    {
        return Err(PluginError::InvalidInput("图片大小或文件名无效".into()));
    }
    let extension = match input.media_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
        "image/svg+xml" => "svg",
        _ => return Err(PluginError::InvalidInput("不支持的图片类型".into())),
    };
    magi_media::validate_image(format!("image.{extension}"), bytes)
        .map_err(|e| PluginError::InvalidInput(e.into()))?;
    Ok(())
}

impl PluginManager {
    pub fn ensure_image_attachment_access(&self, plugin: &str) -> Result<(), PluginError> {
        self.ensure_editing_admitted(plugin)?;
        if !self.is_enabled(plugin)?
            || !self.permission_allowed(plugin, PluginPermissionKind::Attachments, "images")?
        {
            return Err(PluginError::NotAuthorized(
                "缺少 attachments:[images] 授权".into(),
            ));
        }
        Ok(())
    }

    pub fn save_image_attachment(
        &mut self,
        plugin: &str,
        input: PluginImageAttachmentBegin,
        bytes: &[u8],
    ) -> Result<PluginImageAttachment, PluginError> {
        self.ensure_image_attachment_access(plugin)?;
        validate_image(&input, bytes)?;
        // 清理失败提交或已完成数据重置留下的载荷；以现有资源元数据为唯一依据。
        self.collect_image_attachment_files()?;
        let current = self
            .resources
            .entries(plugin)
            .into_iter()
            .filter(|(key, _)| key.starts_with(PREFIX))
            .map(|(_, value)| metadata(value.value))
            .collect::<Result<Vec<_>, _>>()?;
        if current.len() >= MAX_PLUGIN_IMAGES
            || current.iter().map(|image| image.bytes).sum::<u64>() + input.bytes > MAX_PLUGIN_BYTES
        {
            return Err(PluginError::InvalidInput(
                "插件图片存储配额已满，请释放不再使用的附件".into(),
            ));
        }
        let sha256 = digest(bytes);
        let directory = self.root.join("images");
        fs::create_dir_all(&directory)?;
        let path = directory.join(&sha256);
        if fs::read(&path).ok().as_deref() != Some(bytes) {
            write_atomic(&path, bytes)?;
        }
        let attachment_id = magi_core::EventId::unique("plugin-image").to_string();
        let record = PluginImageAttachment {
            attachment_id,
            media_type: input.media_type,
            name: input.name,
            bytes: input.bytes,
            sha256,
        };
        // 元数据提交是唯一事实点。失败时没有可读附件；残留载荷在重新打开时回收。
        self.resources.write(
            plugin,
            &image_key(&record.attachment_id)?,
            0,
            serde_json::to_value(&record).map_err(|e| PluginError::InvalidInput(e.to_string()))?,
        )?;
        Ok(record)
    }

    pub fn image_attachment(
        &self,
        plugin: &str,
        id: &str,
    ) -> Result<PluginImageAttachment, PluginError> {
        self.ensure_image_attachment_access(plugin)?;
        let value = self
            .resources
            .read(plugin, &image_key(id)?)
            .ok_or_else(|| PluginError::NotFound("图片附件不存在".into()))?;
        let record = metadata(value.value)?;
        if record.attachment_id != id {
            return Err(PluginError::CorruptState("图片附件身份不符".into()));
        }
        Ok(record)
    }

    pub fn read_image_attachment(
        &self,
        plugin: &str,
        input: PluginImageAttachmentRead,
    ) -> Result<PluginImageAttachmentChunk, PluginError> {
        let record = self.image_attachment(plugin, &input.attachment_id)?;
        if input.offset > record.bytes || input.length == 0 || input.length > 786_432 {
            return Err(PluginError::InvalidInput("图片读取区间无效".into()));
        }
        let bytes = fs::read(self.root.join("images").join(&record.sha256))?;
        if bytes.len() as u64 != record.bytes || digest(&bytes) != record.sha256 {
            return Err(PluginError::CorruptState(
                "图片附件载荷缺失或摘要不符".into(),
            ));
        }
        let start = input.offset as usize;
        let end = bytes.len().min(start + input.length as usize);
        Ok(PluginImageAttachmentChunk {
            offset: input.offset,
            total_bytes: record.bytes,
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]),
        })
    }

    pub fn release_image_attachment(
        &mut self,
        plugin: &str,
        id: &str,
    ) -> Result<bool, PluginError> {
        self.ensure_image_attachment_access(plugin)?;
        let key = image_key(id)?;
        let Some(value) = self.resources.read(plugin, &key) else {
            return Ok(false);
        };
        let record = metadata(value.value)?;
        self.resources.remove(plugin, &key)?;
        if !self
            .resources
            .reserved_entries(PREFIX)
            .into_iter()
            .any(|value| {
                value.value.get("sha256").and_then(Value::as_str) == Some(record.sha256.as_str())
            })
        {
            // 删除失败不复活已释放的身份；重新打开时按唯一元数据事实回收载荷。
            let _ = fs::remove_file(self.root.join("images").join(record.sha256));
        }
        Ok(true)
    }

    pub(super) fn collect_image_attachment_files(&self) -> Result<(), PluginError> {
        let directory = self.root.join("images");
        let entries = match fs::read_dir(&directory) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let live = self
            .resources
            .reserved_entries(PREFIX)
            .into_iter()
            .map(|resource| metadata(resource.value).map(|m| m.sha256))
            .collect::<Result<BTreeSet<_>, _>>()?;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && !live.contains(&entry.file_name().to_string_lossy().to_string())
            {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}

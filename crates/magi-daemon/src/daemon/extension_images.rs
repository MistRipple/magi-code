//! 有界、实例所属的图片上传暂存；提交只走 PluginManager 的持久附件入口。
use base64::Engine as _;
use magi_app_server_protocol::generated::*;
use magi_extension_host::protocol::{ErrorKind, RpcError};
use magi_plugin_system::PluginManager;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

struct Upload {
    lease: String,
    input: PluginImageAttachmentBegin,
    bytes: Vec<u8>,
    committed: Option<PluginImageAttachment>,
    touched: Instant,
}
#[derive(Default)]
pub(super) struct ImageUploads {
    values: HashMap<String, Upload>,
}
fn parse<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value)
        .map_err(|error| RpcError::new(ErrorKind::InvalidParams, error.to_string()))
}
fn output<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(ErrorKind::Internal, error.to_string()))
}
fn map_error(error: magi_plugin_system::PluginError) -> RpcError {
    use magi_plugin_system::PluginError as E;
    let kind = match &error {
        E::NotAuthorized(_) => ErrorKind::PermissionDenied,
        E::NotFound(_) => ErrorKind::MethodNotFound,
        E::InvalidInput(_) | E::InvalidPackage(_) => ErrorKind::InvalidParams,
        E::Conflict(_) => ErrorKind::Conflict,
        _ => ErrorKind::Internal,
    };
    RpcError::new(kind, error.to_string())
}
impl ImageUploads {
    pub fn discard_instance(&mut self, lease: &str) {
        self.values.retain(|_, upload| upload.lease != lease);
    }
    pub fn call(
        &mut self,
        manager: &mut PluginManager,
        plugin: &str,
        lease: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        manager.check_runtime(plugin, lease).map_err(map_error)?;
        manager
            .ensure_image_attachment_access(plugin)
            .map_err(map_error)?;
        self.values
            .retain(|_, upload| upload.touched.elapsed() < Duration::from_secs(60));
        match method {
            "images.begin" => {
                let input: PluginImageAttachmentBegin = parse(params)?;
                if input.bytes == 0
                    || input.bytes > 10 * 1024 * 1024
                    || input.name.is_empty()
                    || input.name.len() > 256
                    || input.media_type.len() > 64
                {
                    return Err(RpcError::new(
                        ErrorKind::InvalidParams,
                        "图片上传元数据无效",
                    ));
                }
                let open = self
                    .values
                    .values()
                    .filter(|upload| upload.committed.is_none())
                    .collect::<Vec<_>>();
                if open.len() >= 2
                    || open.iter().map(|upload| upload.input.bytes).sum::<u64>() + input.bytes
                        > 20 * 1024 * 1024
                    || self.values.len() >= 128
                {
                    return Err(RpcError::new(
                        ErrorKind::Backpressure,
                        "图片上传暂存配额已满",
                    ));
                }
                let id = magi_core::EventId::unique("image-upload").to_string();
                self.values.insert(
                    id.clone(),
                    Upload {
                        lease: lease.into(),
                        input,
                        bytes: Vec::new(),
                        committed: None,
                        touched: Instant::now(),
                    },
                );
                output(PluginImageAttachmentUpload { upload_id: id })
            }
            "images.append" => {
                let input: PluginImageAttachmentAppend = parse(params)?;
                if input.data_base64.len() > 1_048_576 {
                    return Err(RpcError::new(ErrorKind::Backpressure, "图片分块过大"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(input.data_base64)
                    .map_err(|_| {
                        RpcError::new(ErrorKind::InvalidParams, "图片分块不是有效 base64")
                    })?;
                let upload = self.upload(&input.upload_id, lease)?;
                if upload.committed.is_some()
                    || bytes.is_empty()
                    || input.offset > upload.bytes.len() as u64
                    || input.offset + bytes.len() as u64 > upload.input.bytes
                {
                    return Err(RpcError::new(ErrorKind::Conflict, "图片分块偏移或状态无效"));
                }
                if input.offset < upload.bytes.len() as u64 {
                    let start = input.offset as usize;
                    if upload.bytes.get(start..start + bytes.len()) != Some(bytes.as_slice()) {
                        return Err(RpcError::new(ErrorKind::Conflict, "重传的图片分块不一致"));
                    }
                } else {
                    upload.bytes.extend_from_slice(&bytes);
                }
                output(PluginImageAttachmentAppendResult {
                    offset: upload.bytes.len() as u64,
                })
            }
            "images.commit" => {
                let input: PluginImageAttachmentUpload = parse(params)?;
                let upload = self.upload(&input.upload_id, lease)?;
                if let Some(record) = &upload.committed {
                    return output(
                        manager
                            .image_attachment(plugin, &record.attachment_id)
                            .map_err(map_error)?,
                    );
                }
                if upload.bytes.len() as u64 != upload.input.bytes {
                    return Err(RpcError::new(ErrorKind::Conflict, "图片尚未完整上传"));
                }
                let record = manager
                    .save_image_attachment(plugin, upload.input.clone(), &upload.bytes)
                    .map_err(map_error)?;
                upload.bytes.clear();
                upload.bytes.shrink_to_fit();
                upload.committed = Some(record.clone());
                output(record)
            }
            "images.abort" => {
                let input: PluginImageAttachmentUpload = parse(params)?;
                if self
                    .values
                    .get(&input.upload_id)
                    .is_some_and(|upload| upload.lease != lease)
                {
                    return Err(RpcError::new(
                        ErrorKind::StaleGeneration,
                        "上传不属于当前实例",
                    ));
                }
                output(PluginImageAttachmentAbortResult {
                    removed: self.values.remove(&input.upload_id).is_some(),
                })
            }
            "images.describe" => {
                let input: PluginImageAttachmentRef = parse(params)?;
                output(
                    manager
                        .image_attachment(plugin, &input.attachment_id)
                        .map_err(map_error)?,
                )
            }
            "images.read" => output(
                manager
                    .read_image_attachment(plugin, parse(params)?)
                    .map_err(map_error)?,
            ),
            "images.release" => {
                let input: PluginImageAttachmentRef = parse(params)?;
                output(PluginImageAttachmentReleaseResult {
                    released: manager
                        .release_image_attachment(plugin, &input.attachment_id)
                        .map_err(map_error)?,
                })
            }
            _ => Err(RpcError::new(ErrorKind::MethodNotFound, method)),
        }
    }
    fn upload(&mut self, id: &str, lease: &str) -> Result<&mut Upload, RpcError> {
        let value = self
            .values
            .get_mut(id)
            .ok_or_else(|| RpcError::new(ErrorKind::MethodNotFound, "上传不存在或已过期"))?;
        if value.lease != lease {
            return Err(RpcError::new(
                ErrorKind::StaleGeneration,
                "上传不属于当前实例",
            ));
        }
        value.touched = Instant::now();
        Ok(value)
    }
}

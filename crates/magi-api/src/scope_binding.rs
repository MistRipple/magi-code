use serde_json::{Map, Value};

/// 请求体与设置条目中的 scope 绑定字段（只有 camelCase 形状）。
pub(crate) const SCOPE_BINDING_FIELDS: [&str; 3] = ["workspaceId", "workspacePath", "sessionId"];

pub(crate) fn strip_scope_binding_fields(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        strip_scope_binding_fields_from_map(object);
    }
}

pub(crate) fn strip_scope_binding_fields_from_map(object: &mut Map<String, Value>) {
    for key in SCOPE_BINDING_FIELDS {
        object.remove(key);
    }
}

pub(crate) fn without_scope_binding_fields(mut value: Value) -> Value {
    strip_scope_binding_fields(&mut value);
    value
}

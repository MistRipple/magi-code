use serde_json::{Map, Value};

pub(crate) const SCOPE_BINDING_FIELDS: [&str; 6] = [
    "workspaceId",
    "workspace_id",
    "workspacePath",
    "workspace_path",
    "sessionId",
    "session_id",
];

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

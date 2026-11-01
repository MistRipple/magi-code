//! 配置页面的动作边界、证据核验和脱敏；不选择模型、不维护对话历史。
use serde::Deserialize;
use serde_json::Value;
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkpoint {
    pub outcome: String,
    #[serde(default)]
    pub tunnel_id: Option<String>,
    pub detail: String,
    #[serde(default)]
    pub handoff_stage: Option<String>,
    /// 页面当前显示的组织和项目名。key_prepared 必须明确并逐字取自快照，
    /// daemon 会把它们写进用户可见的说明，让用户在生成密钥前核对作用域。
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// 必须逐字来自本次快照。模型不能凭旧会话或自己生成的说明宣布成功。
    pub evidence: Vec<String>,
}

pub(crate) fn validate_action(name: &str, args: &Value, snapshot: &Value) -> Result<(), String> {
    if !args.is_object() {
        return Err("操作参数必须是对象".into());
    }
    if args.get("tab_id").is_some()
        || args.get("init_script").is_some()
        || args.get("selector").is_some()
        || args.get("submit_key").is_some()
    {
        return Err("配置只允许当前页面及快照节点，不接受脚本或选择器".into());
    }
    if name == "browser_navigate" {
        if args["action"]
            .as_str()
            .is_some_and(|action| action != "url")
        {
            return Err("配置导航只接受明确 URL".into());
        }
        if !allowed_origin(args["url"].as_str().unwrap_or_default()) {
            return Err("配置只允许访问 ChatGPT 和 OpenAI 平台".into());
        }
        return Ok(());
    }
    if name == "browser_snapshot" {
        return Ok(());
    }
    if !allowed_origin(snapshot["url"].as_str().unwrap_or_default()) {
        return Err("当前页面需要用户完成外部登录".into());
    }
    if name == "browser_viewport" {
        if !matches!(args["action"].as_str(), Some("get" | "set")) {
            return Err("无效视口动作".into());
        }
        return Ok(());
    }
    if name == "browser_wait_for" {
        if args["timeout_ms"].as_u64().unwrap_or(5000) > 5000 {
            return Err("配置等待单次最多 5 秒".into());
        }
        if let Some(url) = args["url"].as_str()
            && !allowed_origin(url)
        {
            return Err("不能等待允许域之外的页面".into());
        }
        if args.get("stable").and_then(Value::as_bool) == Some(true)
            || args.get("text").is_some()
            || args.get("texts").is_some()
            || args.get("url").is_some()
        {
            return Ok(());
        }
        return Err("请指定已观察的文本、URL 或稳定条件".into());
    }
    let elements = snapshot["elements"]
        .as_array()
        .ok_or("请先读取当前页面快照")?;
    let validate_ref = |reference: &Value| -> Result<(), String> {
        let reference = reference.as_str().ok_or("缺少 element_ref")?;
        let node = elements
            .iter()
            .find(|node| node["element_ref"].as_str() == Some(reference))
            .ok_or("节点不属于当前页面快照，请重新观察")?;
        if node
            .get("sensitive_input_kind")
            .is_some_and(|kind| !kind.is_null())
        {
            return Err("敏感输入需要用户在页面操作".into());
        }
        if node["value"]
            .as_str()
            .is_some_and(|value| value.contains("[redacted]"))
        {
            return Err("凭据字段需要用户操作".into());
        }
        if node["disabled"].as_bool() == Some(true) {
            return Err("控件尚未就绪，请观察或等待".into());
        }
        Ok(())
    };
    // 这些是操作权限边界，不是网页流程适配：配置模型不能生成凭据、删除账号资源或越权确认。
    if matches!(name, "browser_click" | "browser_hover") {
        let reference = args["element_ref"].as_str().unwrap_or_default();
        if let Some(node) = elements
            .iter()
            .find(|node| node["element_ref"].as_str() == Some(reference))
        {
            let label = node["name"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            // 打开表单的入口（如 "Create new secret key"）必须可点击；只有提交生成的最终按钮交还用户。
            let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if matches!(
                normalized.as_str(),
                "create secret key"
                    | "generate secret key"
                    | "生成密钥"
                    | "创建密钥"
                    | "创建机密密钥"
            ) {
                return Err("密钥表单准备好后必须交还用户生成密钥".into());
            }
            if ["delete", "revoke", "remove", "删除", "撤销", "移除"]
                .iter()
                .any(|word| label.contains(word))
            {
                return Err("此配置操作不包含删除或撤销已有资源，请保留并核对可复用项".into());
            }
        }
    }
    match name {
        "browser_press" => {
            let key = args["key"].as_str().ok_or("缺少按键")?;
            if ![
                "Escape",
                "Tab",
                "Shift+Tab",
                "ArrowDown",
                "ArrowUp",
                "ArrowLeft",
                "ArrowRight",
                "Home",
                "End",
                "PageUp",
                "PageDown",
            ]
            .contains(&key)
            {
                return Err(
                    "提交及确认请点击可核对的按钮；配置代理不发送 Enter 或快捷键提交".into(),
                );
            }
            Ok(())
        }
        "browser_scroll" => {
            if args.get("element_ref").is_some() {
                validate_ref(&args["element_ref"])?;
            }
            Ok(())
        }
        "browser_read" => {
            if args["max_chars"].as_u64().unwrap_or(12000) > 16000 {
                return Err("配置正文单次最多 16000 字符，请分页读取".into());
            }
            if args.get("element_ref").is_some() {
                validate_ref(&args["element_ref"])?;
            }
            Ok(())
        }
        "browser_click" | "browser_type" | "browser_hover" => validate_ref(&args["element_ref"]),
        "browser_fill_form" => {
            let fields = args["fields"].as_array().ok_or("缺少 fields")?;
            if fields.is_empty() || fields.len() > 10 {
                return Err("每次只填写 1–10 个可核验字段".into());
            }
            for field in fields {
                validate_ref(&field["element_ref"])?;
            }
            Ok(())
        }
        _ => Err("配置工具不支持此操作".into()),
    }
}

pub(crate) fn allowed_origin(url: &str) -> bool {
    reqwest::Url::parse(url).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
            && matches!(
                url.host_str(),
                Some("chatgpt.com" | "chat.openai.com" | "platform.openai.com")
            )
    })
}

fn snapshot_text(snapshot: &Value) -> String {
    let mut text = snapshot["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|node| {
            ["name", "value", "description"]
                .into_iter()
                .filter_map(move |key| node[key].as_str())
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(value) = snapshot.get("page_text") {
        text.push_str(&value.to_string());
    }
    text
}

pub(crate) fn validate_checkpoint(checkpoint: &Checkpoint, snapshot: &Value) -> Result<(), String> {
    let text = snapshot_text(snapshot);
    if checkpoint.evidence.is_empty()
        || checkpoint
            .evidence
            .iter()
            .any(|value| value.trim().is_empty() || !text.contains(value))
    {
        return Err("完成证据必须逐字来自最新页面快照，请重新核对".into());
    }
    if checkpoint.outcome == "human_required" {
        return Ok(());
    }
    if !["tunnel_ready", "key_prepared", "connector_ready"].contains(&checkpoint.outcome.as_str()) {
        return Err("配置检查点无效".into());
    }
    if checkpoint.outcome == "tunnel_ready" {
        let id = checkpoint.tunnel_id.as_deref().ok_or("缺少 Tunnel ID")?;
        if !magi_web_model::tunnel::valid_tunnel_id(id) || !text.contains(id) {
            return Err("Tunnel ID 必须从当前平台页面回读".into());
        }
    }
    // 准备生成密钥必须核对作用域。页面不显示时先人工交接，不能把缺少证据当成默认项目。
    if checkpoint.outcome == "key_prepared"
        && [&checkpoint.organization, &checkpoint.project]
            .iter()
            .any(|value| value.as_deref().is_none_or(|value| value.trim().is_empty()))
    {
        return Err(
            "组织或项目尚未核实，请先 human_required 交接确认，不能宣布密钥表单已准备".into(),
        );
    }
    for (label, value) in [
        ("组织", &checkpoint.organization),
        ("项目", &checkpoint.project),
    ] {
        if let Some(value) = value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && !text.contains(value)
        {
            return Err(format!("{label}名必须逐字来自最新快照"));
        }
    }
    if checkpoint.outcome == "key_prepared" {
        // “表单已准备”必须由页面上真实选中的控件证明，而不是模型的描述。
        let elements = snapshot["elements"].as_array().cloned().unwrap_or_default();
        let claimed = checkpoint
            .evidence
            .iter()
            .filter_map(|value| {
                elements.iter().find(|node| {
                    node["name"].as_str().map(str::trim) == Some(value.trim())
                        && matches!(node["role"].as_str(), Some("radio" | "checkbox" | "switch"))
                })
            })
            .collect::<Vec<_>>();
        if claimed.is_empty() {
            return Err("key_prepared 的证据必须包含页面上已选中的权限控件（被选中的单选项、勾选的复选框）；没有核实到就继续操作表单或用 human_required 说明还缺什么".into());
        }
        for node in claimed {
            let checked = node["states"]
                .as_array()
                .is_some_and(|states| states.iter().any(|state| state.as_str() == Some("checked")));
            if !checked {
                return Err(format!(
                    "证据“{}”当前并未选中，表单还没有准备好",
                    node["name"].as_str().unwrap_or_default()
                ));
            }
        }
    }
    if ["tunnel_ready", "connector_ready"].contains(&checkpoint.outcome.as_str())
        && !text.contains("Magi")
    {
        return Err("必须在当前页面核对 Magi 资源归属".into());
    }
    let url = snapshot["url"].as_str().unwrap_or_default();
    let valid = allowed_origin(url)
        && reqwest::Url::parse(url).ok().is_some_and(|url| {
            if checkpoint.outcome == "connector_ready" {
                matches!(url.host_str(), Some("chatgpt.com" | "chat.openai.com"))
            } else {
                url.host_str() == Some("platform.openai.com")
            }
        });
    if !valid {
        return Err("当前页面不属于本步骤的平台".into());
    }
    Ok(())
}

/// 密钥在网页上可能以 text input 或正文呈现；不依赖 type=password 才脱敏。
pub(crate) fn redact(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(index) = ["sk-", "sk_"]
        .iter()
        .flat_map(|prefix| rest.match_indices(prefix).map(|(index, _)| index))
        .filter(|index| secret_boundary(rest, *index))
        .min()
    {
        result.push_str(&rest[..index]);
        let secret = &rest[index..];
        let end = secret
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(secret.len());
        result.push_str("[redacted]");
        rest = &secret[end..];
    }
    result.push_str(rest);
    result
}
fn secret_boundary(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_'))
}
pub(crate) fn contains_secret(text: &str) -> bool {
    ["sk-", "sk_"].iter().any(|prefix| {
        text.match_indices(prefix).any(|(index, _)| {
            secret_boundary(text, index)
                && text[index + prefix.len()..]
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
                    .count()
                    >= 8
        })
    })
}
pub(crate) fn redact_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(key, value)| (key, redact_value(value)))
                .collect(),
        ),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn snapshot() -> Value {
        json!({"url":"https://platform.openai.com/settings","elements":[
            {"element_ref":"e1","name":"Name","sensitive_input_kind":null,"value":"Magi"},
            {"element_ref":"e2","name":"Password","sensitive_input_kind":"password"},
            {"element_ref":"e3","name":"Create secret key","sensitive_input_kind":null},
            {"element_ref":"e4","name":"Delete resource","sensitive_input_kind":null}
        ]})
    }
    #[test]
    fn ordinary_nodes_with_null_sensitive_marker_are_operable() {
        assert!(
            validate_action(
                "browser_type",
                &json!({"element_ref":"e1","text":"Magi"}),
                &snapshot()
            )
            .is_ok()
        );
        for node in ["e2", "old-ref", "e3", "e4"] {
            assert!(
                validate_action("browser_click", &json!({"element_ref":node}), &snapshot())
                    .is_err(),
                "{node}"
            );
        }
    }
    #[test]
    fn origin_parser_rejects_lookalikes_credentials_ports_and_other_schemes() {
        for url in [
            "https://chatgpt.com/",
            "https://chat.openai.com/a",
            "https://platform.openai.com:443/settings",
        ] {
            assert!(allowed_origin(url), "{url}");
        }
        for url in [
            "http://chatgpt.com",
            "https://chatgpt.com.evil.test",
            "https://evil.test/chatgpt.com",
            "https://user@chatgpt.com",
            "https://chatgpt.com:444",
            "https://auth.openai.com/",
            "javascript:alert(1)",
        ] {
            assert!(!allowed_origin(url), "{url}");
        }
    }
    #[test]
    fn forbidden_entrypoints_and_submit_aliases_cannot_bypass_scope() {
        for name in [
            "browser_screenshot",
            "browser_click_at",
            "browser_evaluate",
            "browser_storage",
            "browser_network",
            "browser_download",
            "browser_upload_file",
            "browser_dialog",
            "browser_tabs",
        ] {
            assert!(
                validate_action(name, &json!({}), &snapshot()).is_err(),
                "{name}"
            );
        }
        for args in [
            json!({"element_ref":"e1","submit_key":"Enter"}),
            json!({"selector":"body"}),
            json!({"tab_id":"other"}),
            json!({"init_script":"alert(1)"}),
        ] {
            assert!(validate_action("browser_click", &args, &snapshot()).is_err());
        }
        for key in ["Enter", "Space", " ", "Control+Enter", "Meta+Enter"] {
            assert!(validate_action("browser_press", &json!({"key":key}), &snapshot()).is_err());
        }
    }
    #[test]
    fn read_and_wait_are_bounded_and_cross_origin_pages_are_handed_off() {
        assert!(validate_action("browser_read", &json!({"max_chars":16000}), &snapshot()).is_ok());
        assert!(validate_action("browser_read", &json!({"max_chars":16001}), &snapshot()).is_err());
        assert!(
            validate_action(
                "browser_wait_for",
                &json!({"stable":true,"timeout_ms":5000}),
                &snapshot()
            )
            .is_ok()
        );
        assert!(
            validate_action(
                "browser_wait_for",
                &json!({"stable":true,"timeout_ms":5001}),
                &snapshot()
            )
            .is_err()
        );
        assert!(
            validate_action(
                "browser_wait_for",
                &json!({"selector":"input"}),
                &snapshot()
            )
            .is_err()
        );
        let mut page = snapshot();
        page["url"] = json!("https://login.example.com");
        assert!(validate_action("browser_read", &json!({}), &page).is_err());
    }
    #[test]
    fn secrets_are_removed_recursively_before_delivery() {
        assert_eq!(
            redact("task-local-agent-1234567890"),
            "task-local-agent-1234567890"
        );
        assert!(!contains_secret("task-local-agent-1234567890"));
        let data = json!({"text":"before sk-private_0123456789 after","nodes":[{"value":"sk_proj_123456789"}]});
        assert!(contains_secret(&data.to_string()));
        let clean = redact_value(data).to_string();
        assert!(!contains_secret(&clean));
        assert!(clean.contains("[redacted]"));
    }
    #[test]
    fn checkpoints_require_current_page_evidence_and_checked_permissions() {
        let mut page = snapshot();
        page["elements"][0]["name"] = json!("Restricted");
        page["elements"][0]["role"] = json!("radio");
        page["page_text"] = json!({"text":"Acme Org / Default project"});
        let checkpoint: Checkpoint = serde_json::from_value(
            json!({"outcome":"key_prepared","detail":"ready","organization":"Acme Org","project":"Default project","evidence":["Restricted"]}),
        )
        .unwrap();
        assert!(validate_checkpoint(&checkpoint, &page).is_err());
        page["elements"][0]["states"] = json!(["checked"]);
        assert!(validate_checkpoint(&checkpoint, &page).is_ok());
        let mut unknown_scope: Checkpoint = serde_json::from_value(
            json!({"outcome":"key_prepared","detail":"ready","evidence":["Restricted"]}),
        )
        .unwrap();
        assert!(validate_checkpoint(&unknown_scope, &page).is_err());
        unknown_scope.outcome = "human_required".into();
        assert!(validate_checkpoint(&unknown_scope, &page).is_ok());
        page["elements"][0]["name"] = json!("All");
        assert!(validate_checkpoint(&checkpoint, &page).is_err());
    }
}

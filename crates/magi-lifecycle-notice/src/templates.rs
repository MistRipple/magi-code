//! 模板渲染：`include_str!` 加载，运行时只做 `{{key}}` 字符串替换。
//!
//! 为什么不用 format! / handlebars：模板字符串里有大量自由 Markdown，
//! format! 的 `{}` 会和 `{{` `}}` escape 互相干扰；引入完整模板引擎对于
//! 4 个简短模板是过度工程。这里维持最小依赖。
//!
//! 缺 key 返回 placeholder `<missing:key>`，不 panic——模型即便看到也能
//! 反推语义，比静默丢字段更安全。

const MISSION_RESUMED: &str = include_str!("../templates/mission_resumed.md");
const HUMAN_CHECKPOINT_APPROVED: &str = include_str!("../templates/human_checkpoint_approved.md");
const HUMAN_CHECKPOINT_REJECTED: &str = include_str!("../templates/human_checkpoint_rejected.md");
const PLAN_STEP_COMPLETED: &str = include_str!("../templates/plan_step_completed.md");

pub(crate) fn render_mission_resumed(vars: &[(&str, &str)]) -> String {
    render(MISSION_RESUMED, vars)
}

pub(crate) fn render_human_checkpoint_approved(vars: &[(&str, &str)]) -> String {
    render(HUMAN_CHECKPOINT_APPROVED, vars)
}

pub(crate) fn render_human_checkpoint_rejected(vars: &[(&str, &str)]) -> String {
    render(HUMAN_CHECKPOINT_REJECTED, vars)
}

pub(crate) fn render_plan_step_completed(vars: &[(&str, &str)]) -> String {
    render(PLAN_STEP_COMPLETED, vars)
}

fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        let token = format!("{{{{{key}}}}}");
        out = out.replace(&token, value);
    }
    // 兜底：未替换 token 会以 `{{key}}` 形式残留，标记为 missing 让模型可见。
    if out.contains("{{") {
        // 不强行解析，只输出告警字符串，避免 panic。
        out = out.replace("{{", "<missing:").replace("}}", ">");
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_known_keys() {
        let body = render_mission_resumed(&[
            ("mission_id", "M-1"),
            ("recovery_id", "R-1"),
            ("checkpoint_sequence", "7"),
        ]);
        assert!(body.contains("M-1"));
        assert!(body.contains("R-1"));
        assert!(body.contains("7"));
        assert!(!body.contains("{{"));
    }

    #[test]
    fn missing_key_is_marked_not_panicked() {
        let body = render_mission_resumed(&[("mission_id", "M-1")]);
        assert!(body.contains("<missing:recovery_id>"));
        assert!(body.contains("<missing:checkpoint_sequence>"));
    }
}

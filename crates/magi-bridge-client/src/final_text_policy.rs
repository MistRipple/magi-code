use crate::llm_types::LlmResponse;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalTextAction {
    Accept,
    Reject,
    Truncate { max_chars: usize },
}

pub fn evaluate_final_text(response: &LlmResponse, max_length: Option<usize>) -> FinalTextAction {
    if response.content.trim().is_empty() {
        return FinalTextAction::Accept;
    }

    if crate::llm_types::is_summary_hijack_text(&response.content) {
        return FinalTextAction::Reject;
    }

    if let Some(max) = max_length
        && response.content.len() > max
    {
        return FinalTextAction::Truncate { max_chars: max };
    }

    FinalTextAction::Accept
}

pub fn apply_final_text_policy(content: &str, action: FinalTextAction) -> String {
    match action {
        FinalTextAction::Accept => content.to_string(),
        FinalTextAction::Reject => String::new(),
        FinalTextAction::Truncate { max_chars } => {
            if content.len() <= max_chars {
                content.to_string()
            } else {
                let truncated: String = content.chars().take(max_chars).collect();
                format!("{}...", truncated)
            }
        }
    }
}

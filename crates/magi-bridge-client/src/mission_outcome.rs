use serde::Deserialize;

const MISSION_OUTCOME_START: &str = "[[MISSION_OUTCOME]]";
const MISSION_OUTCOME_END: &str = "[[/MISSION_OUTCOME]]";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MissionOutcomeStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct MissionOutcomeBlock {
    pub status: Option<MissionOutcomeStatus>,
    pub next_steps: Vec<String>,
}

pub struct MissionOutcomeExtractor {
    buffer: String,
    in_block: bool,
    latest_outcome: Option<MissionOutcomeBlock>,
}

pub struct ConsumeResult {
    pub text: String,
    pub outcome: Option<MissionOutcomeBlock>,
}

impl MissionOutcomeExtractor {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_block: false,
            latest_outcome: None,
        }
    }

    pub fn consume(&mut self, chunk: &str) -> ConsumeResult {
        if chunk.is_empty() {
            return ConsumeResult {
                text: String::new(),
                outcome: self.latest_outcome.clone(),
            };
        }

        self.buffer.push_str(chunk);
        let mut output = String::new();

        loop {
            if self.buffer.is_empty() {
                break;
            }

            if !self.in_block {
                if let Some(start_idx) = self.buffer.find(MISSION_OUTCOME_START) {
                    output.push_str(&self.buffer[..start_idx]);
                    self.buffer = self.buffer[start_idx + MISSION_OUTCOME_START.len()..].to_string();
                    self.in_block = true;
                    continue;
                }
                let holdback = start_marker_holdback_len(&self.buffer);
                let safe_len = self.buffer.len().saturating_sub(holdback);
                output.push_str(&self.buffer[..safe_len]);
                self.buffer = self.buffer[safe_len..].to_string();
                break;
            }

            if let Some(end_idx) = self.buffer.find(MISSION_OUTCOME_END) {
                let raw_json = self.buffer[..end_idx].trim().to_string();
                self.buffer = self.buffer[end_idx + MISSION_OUTCOME_END.len()..].to_string();
                self.in_block = false;
                if let Some(parsed) = parse_outcome(&raw_json) {
                    self.latest_outcome = Some(parsed);
                }
            } else {
                break;
            }
        }

        ConsumeResult {
            text: output,
            outcome: self.latest_outcome.clone(),
        }
    }

    pub fn finalize(&mut self) -> ConsumeResult {
        let text = if self.in_block {
            String::new()
        } else {
            std::mem::take(&mut self.buffer)
        };
        self.buffer.clear();
        self.in_block = false;
        ConsumeResult {
            text,
            outcome: self.latest_outcome.clone(),
        }
    }
}

pub fn extract_mission_outcome(text: &str) -> ConsumeResult {
    if text.is_empty() {
        return ConsumeResult {
            text: String::new(),
            outcome: None,
        };
    }
    let mut extractor = MissionOutcomeExtractor::new();
    let extracted = extractor.consume(text);
    let tail = extractor.finalize();
    let combined = format!("{}{}", extracted.text, tail.text);
    let sanitized = sanitize_outcome_protocol_text(&combined);
    ConsumeResult {
        text: sanitized,
        outcome: tail.outcome.or(extracted.outcome),
    }
}

pub fn sanitize_outcome_protocol_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut sanitized = text.to_string();
    for _ in 0..10 {
        if !sanitized.contains(MISSION_OUTCOME_START) && !sanitized.contains(MISSION_OUTCOME_END) {
            break;
        }
        let mut ext = MissionOutcomeExtractor::new();
        let extracted = ext.consume(&sanitized);
        let tail = ext.finalize();
        let next = format!("{}{}", extracted.text, tail.text);
        if next == sanitized {
            break;
        }
        sanitized = next;
    }

    sanitized = sanitized
        .replace(MISSION_OUTCOME_START, "")
        .replace(MISSION_OUTCOME_END, "");

    let partial_markers = build_partial_markers();
    for fragment in &partial_markers {
        if sanitized.ends_with(fragment) {
            sanitized.truncate(sanitized.len() - fragment.len());
            break;
        }
    }

    sanitized
}

fn start_marker_holdback_len(input: &str) -> usize {
    let max_holdback = input.len().min(MISSION_OUTCOME_START.len() - 1);
    for len in (1..=max_holdback).rev() {
        let pos = input.len() - len;
        if !input.is_char_boundary(pos) {
            continue;
        }
        if MISSION_OUTCOME_START.starts_with(&input[pos..]) {
            return len;
        }
    }
    0
}

fn build_partial_markers() -> Vec<String> {
    let mut fragments = Vec::new();
    for marker in [MISSION_OUTCOME_START, MISSION_OUTCOME_END] {
        for len in (1..marker.len()).rev() {
            fragments.push(marker[..len].to_string());
        }
    }
    fragments.sort_by(|a, b| b.len().cmp(&a.len()));
    fragments.dedup();
    fragments
}

#[derive(Deserialize)]
struct RawOutcome {
    status: Option<String>,
    next_steps: Option<Vec<serde_json::Value>>,
    #[serde(rename = "nextSteps")]
    next_steps_alt: Option<Vec<serde_json::Value>>,
}

fn parse_outcome(raw: &str) -> Option<MissionOutcomeBlock> {
    if raw.is_empty() {
        return None;
    }
    let data: RawOutcome = serde_json::from_str(raw).ok()?;

    let status = data.status.as_deref().map(|s| s.to_lowercase()).and_then(|s| match s.as_str() {
        "running" => Some(MissionOutcomeStatus::Running),
        "completed" => Some(MissionOutcomeStatus::Completed),
        "failed" => Some(MissionOutcomeStatus::Failed),
        _ => None,
    });

    let raw_steps = data.next_steps.or(data.next_steps_alt);
    let next_steps: Vec<String> = raw_steps
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    if status.is_none() && next_steps.is_empty() {
        return None;
    }

    Some(MissionOutcomeBlock { status, next_steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_outcome_from_text() {
        let text = r#"任务完成。[[MISSION_OUTCOME]]{"status":"completed","next_steps":["review code"]}[[/MISSION_OUTCOME]]"#;
        let result = extract_mission_outcome(text);
        assert_eq!(result.text.trim(), "任务完成。");
        let outcome = result.outcome.unwrap();
        assert_eq!(outcome.status, Some(MissionOutcomeStatus::Completed));
        assert_eq!(outcome.next_steps, vec!["review code"]);
    }

    #[test]
    fn extract_running_status() {
        let text = r#"[[MISSION_OUTCOME]]{"status":"running","next_steps":["step1","step2"]}[[/MISSION_OUTCOME]] 继续执行"#;
        let result = extract_mission_outcome(text);
        assert!(result.text.contains("继续执行"));
        let outcome = result.outcome.unwrap();
        assert_eq!(outcome.status, Some(MissionOutcomeStatus::Running));
        assert_eq!(outcome.next_steps.len(), 2);
    }

    #[test]
    fn handles_no_outcome_block() {
        let text = "这是普通文本，没有任何标记。";
        let result = extract_mission_outcome(text);
        assert_eq!(result.text, text);
        assert!(result.outcome.is_none());
    }

    #[test]
    fn handles_empty_input() {
        let result = extract_mission_outcome("");
        assert!(result.text.is_empty());
        assert!(result.outcome.is_none());
    }

    #[test]
    fn streaming_consume_across_chunks() {
        let mut ext = MissionOutcomeExtractor::new();

        let r1 = ext.consume("前缀文本 [[MISSION_");
        assert_eq!(r1.text, "前缀文本 ");

        let r2 = ext.consume(r#"OUTCOME]]{"status":"completed"}[[/MISSION_OUTCOME]] 后缀"#);
        let r3 = ext.finalize();
        let full_text = format!("{}{}{}", r1.text, r2.text, r3.text);
        assert!(full_text.contains("后缀"));

        let outcome = r3.outcome.or(r2.outcome).unwrap();
        assert_eq!(outcome.status, Some(MissionOutcomeStatus::Completed));
    }

    #[test]
    fn sanitize_removes_partial_markers() {
        let text = "some text [[MISSION";
        let sanitized = sanitize_outcome_protocol_text(text);
        assert_eq!(sanitized, "some text ");
    }

    #[test]
    fn handles_camel_case_next_steps() {
        let text = r#"[[MISSION_OUTCOME]]{"status":"failed","nextSteps":["fix bug"]}[[/MISSION_OUTCOME]]"#;
        let result = extract_mission_outcome(text);
        let outcome = result.outcome.unwrap();
        assert_eq!(outcome.status, Some(MissionOutcomeStatus::Failed));
        assert_eq!(outcome.next_steps, vec!["fix bug"]);
    }

    #[test]
    fn handles_invalid_json_gracefully() {
        let text = "[[MISSION_OUTCOME]]not json[[/MISSION_OUTCOME]] rest";
        let result = extract_mission_outcome(text);
        assert!(result.text.contains("rest"));
        assert!(result.outcome.is_none());
    }

    #[test]
    fn handles_invalid_status() {
        let text = r#"[[MISSION_OUTCOME]]{"status":"unknown"}[[/MISSION_OUTCOME]]"#;
        let result = extract_mission_outcome(text);
        assert!(result.outcome.is_none());
    }

    #[test]
    fn multiple_outcome_blocks_takes_latest() {
        let text = r#"[[MISSION_OUTCOME]]{"status":"running"}[[/MISSION_OUTCOME]] middle [[MISSION_OUTCOME]]{"status":"completed"}[[/MISSION_OUTCOME]]"#;
        let result = extract_mission_outcome(text);
        let outcome = result.outcome.unwrap();
        assert_eq!(outcome.status, Some(MissionOutcomeStatus::Completed));
    }
}

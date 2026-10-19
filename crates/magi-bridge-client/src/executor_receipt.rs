//! 可信执行器将外部执行事实归一化为证据；这些记录不属于 Magi 待执行工具。
pub use magi_core::{
    ExecutorCommandEvidence, ExecutorCommandStatus, ExecutorFailureEvidence, ExecutorReceipt,
};

pub const EXECUTOR_RECEIPT_KIND: &str = "executor_receipt";

/// 只读取已声明的归一化证据；未知或旧格式上下文继续按原始资料保存，不能猜造命令。
pub fn executor_receipts(
    contexts: &[crate::ModelProviderContext],
) -> Result<Vec<ExecutorReceipt>, String> {
    contexts
        .iter()
        .filter(|context| context.kind == EXECUTOR_RECEIPT_KIND)
        .filter_map(|context| {
            context
                .data
                .get("evidence")
                .map(|evidence| (context, evidence))
        })
        .map(|(context, evidence)| {
            let receipt: ExecutorReceipt = serde_json::from_value(evidence.clone())
                .map_err(|error| format!("执行器证据格式无效：{error}"))?;
            receipt.validate()?;
            if receipt.executor != context.provider {
                return Err("执行器证据与 provider 来源不匹配".into());
            }
            Ok(receipt)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn executor_evidence_keeps_unknown_exit_status_and_rejects_forged_success() {
        let receipt = ExecutorReceipt {
            executor: "fixture".into(),
            execution_id: "external-id".into(),
            commands: vec![ExecutorCommandEvidence {
                command: "inspect".into(),
                exit_code: None,
                status: ExecutorCommandStatus::OutcomeUnknown,
                output: String::new(),
            }],
        };
        receipt.validate().unwrap();
        let mut invalid = receipt.clone();
        invalid.commands[0].status = ExecutorCommandStatus::Succeeded;
        assert!(invalid.validate().is_err());
        invalid.commands[0].exit_code = Some(3);
        assert!(invalid.validate().is_err());
        invalid.commands[0].status = ExecutorCommandStatus::Failed;
        invalid.validate().unwrap();
        let mut context = crate::ModelProviderContext {
            provider: "fixture".into(),
            kind: EXECUTOR_RECEIPT_KIND.into(),
            data: json!({"evidence":receipt}),
        };
        assert_eq!(
            executor_receipts(&[context.clone()]).unwrap(),
            vec![receipt]
        );
        context.provider = "other".into();
        assert!(executor_receipts(&[context]).is_err());
        assert!(
            executor_receipts(&[crate::ModelProviderContext {
                provider: "legacy".into(),
                kind: EXECUTOR_RECEIPT_KIND.into(),
                data: json!({"items":[{"type":"command_execution"}]})
            }])
            .unwrap()
            .is_empty()
        );
    }
}

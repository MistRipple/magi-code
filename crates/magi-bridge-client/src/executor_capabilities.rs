//! 执行能力由可信适配器声明；它描述实现合同，不扩大任何 Task/用户权限。
use crate::{BridgeClientError, BridgeErrorLayer, ModelInvocationRequest, ModelResponse};
use magi_app_server_protocol::{
    ExecutorApprovalMode, ExecutorCancellationMode, ExecutorCapabilities, ExecutorContextOwnership,
    ExecutorInputModality, ExecutorResumeMode, ExecutorSteerMode, ExecutorToolMode,
};

pub fn native_executor_capabilities() -> ExecutorCapabilities {
    ExecutorCapabilities {
        input_modalities: vec![ExecutorInputModality::Text, ExecutorInputModality::Image],
        steer: ExecutorSteerMode::BetweenSteps,
        resume: ExecutorResumeMode::Checkpoint,
        tools: ExecutorToolMode::NativeCalls,
        approvals: ExecutorApprovalMode::Interactive,
        cancellation: ExecutorCancellationMode::Signal,
        context: ExecutorContextOwnership::MagiOwned,
        structured_output: false,
        output_limit: true,
        background: true,
        inspection: true,
    }
}

pub fn text_executor_capabilities() -> ExecutorCapabilities {
    ExecutorCapabilities {
        input_modalities: vec![ExecutorInputModality::Text],
        steer: ExecutorSteerMode::None,
        resume: ExecutorResumeMode::None,
        tools: ExecutorToolMode::None,
        approvals: ExecutorApprovalMode::Unavailable,
        cancellation: ExecutorCancellationMode::Signal,
        context: ExecutorContextOwnership::MagiOwned,
        structured_output: false,
        output_limit: false,
        background: false,
        inspection: false,
    }
}

pub fn validate_executor_request(
    caps: &ExecutorCapabilities,
    request: &ModelInvocationRequest,
) -> Result<(), BridgeClientError> {
    let unsupported = if caps.validate().is_err() {
        Some("invalid_capabilities")
    } else if !caps.input_modalities.contains(&ExecutorInputModality::Text)
        && (!request.prompt.is_empty()
            || request.messages.as_ref().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .content
                        .as_ref()
                        .is_some_and(|text| !text.is_empty())
                })
            }))
    {
        Some("text_unsupported")
    } else if !caps
        .input_modalities
        .contains(&ExecutorInputModality::Image)
        && request
            .messages
            .as_ref()
            .is_some_and(|messages| messages.iter().any(|message| !message.images.is_empty()))
    {
        Some("image_unsupported")
    } else if caps.tools != ExecutorToolMode::NativeCalls
        && (request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
            || request.tool_choice.is_some())
    {
        Some("native_tools_unsupported")
    } else {
        None
    };
    if let Some(code) = unsupported {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::RemoteBusiness,
            code: Some(-32096),
            message: format!("executor_{code}: 当前执行器不支持此请求，调用尚未发送"),
        });
    }
    Ok(())
}

/// 外部执行器自己的工具不交付给 Magi 的工具循环执行。
pub fn validate_executor_response(
    caps: &ExecutorCapabilities,
    response: &ModelResponse,
) -> Result<(), BridgeClientError> {
    if caps.tools == ExecutorToolMode::ExecutorOwned {
        crate::executor_receipts(&response.provider_context).map_err(|message| {
            BridgeClientError::CallFailed {
                layer: BridgeErrorLayer::Protocol,
                code: Some(-32096),
                message: format!("executor_invalid_receipt: {message}"),
            }
        })?;
    }
    if caps.tools != ExecutorToolMode::NativeCalls
        && (!response.tool_calls.is_empty()
            || response.status == crate::ModelResponseStatus::RequiresToolExecution)
    {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: Some(-32096),
            message:
                "executor_native_tools_unsupported: 执行器返回了合同外的 Magi 工具调用，未交付执行"
                    .into(),
        });
    }
    if caps.resume != ExecutorResumeMode::Checkpoint
        && (response.status == crate::ModelResponseStatus::Incomplete
            || (response
                .content
                .as_ref()
                .is_none_or(|text| text.trim().is_empty())
                && response.tool_calls.is_empty()))
    {
        return Err(BridgeClientError::CallFailed {
            layer: BridgeErrorLayer::Protocol,
            code: Some(-32096),
            message:
                "executor_incomplete_response: 执行器未交付完整结果，请核对远端进展；未自动重发"
                    .into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelBridgeClient;

    fn request() -> ModelInvocationRequest {
        ModelInvocationRequest {
            provider: "test".into(),
            prompt: "work".into(),
            messages: None,
            tools: None,
            tool_choice: None,
        }
    }

    #[test]
    fn capabilities_are_host_contracts_and_cannot_enable_native_tools_by_gateway_claim() {
        let mut caps = text_executor_capabilities();
        validate_executor_request(&caps, &request()).unwrap();
        caps.input_modalities.clear();
        assert!(
            validate_executor_request(&caps, &request())
                .unwrap_err()
                .to_string()
                .contains("executor_invalid_capabilities")
        );
        caps = text_executor_capabilities();
        caps.input_modalities.push(ExecutorInputModality::Text);
        assert!(caps.validate().is_err());
        caps = text_executor_capabilities();
        caps.input_modalities = vec![ExecutorInputModality::Image];
        assert!(
            validate_executor_request(&caps, &request())
                .unwrap_err()
                .to_string()
                .contains("executor_text_unsupported")
        );
        for tools in [
            ExecutorToolMode::None,
            ExecutorToolMode::MagiGateway,
            ExecutorToolMode::ExecutorOwned,
        ] {
            caps = text_executor_capabilities();
            caps.tools = tools;
            let mut invocation = request();
            invocation.tool_choice = Some(crate::ChatToolChoice::force_function("shell_exec"));
            assert!(validate_executor_request(&caps, &invocation).is_err());
        }
    }

    #[test]
    fn external_empty_or_incomplete_response_is_never_accepted_for_native_auto_recovery() {
        for resume in [ExecutorResumeMode::None, ExecutorResumeMode::NativeSession] {
            let mut caps = text_executor_capabilities();
            caps.resume = resume;
            assert!(validate_executor_response(&caps, &ModelResponse::completed("")).is_err());
            let mut partial = ModelResponse::completed("partial");
            partial.status = crate::ModelResponseStatus::Incomplete;
            assert!(validate_executor_response(&caps, &partial).is_err());
            validate_executor_response(&caps, &ModelResponse::completed("done")).unwrap();
        }
    }

    #[test]
    fn declared_output_limit_without_an_implementation_never_sends_an_unbounded_request() {
        struct DeclaredOnlyExecutor;
        impl ModelBridgeClient for DeclaredOnlyExecutor {
            fn executor_capabilities(&self) -> ExecutorCapabilities {
                native_executor_capabilities()
            }
            fn invoke(
                &self,
                _: ModelInvocationRequest,
            ) -> Result<ModelResponse, BridgeClientError> {
                panic!("不能把带输出上限的请求降级为无上限调用")
            }
            fn invoke_streaming(
                &self,
                _: ModelInvocationRequest,
                _: &dyn Fn(&crate::ModelStreamingDelta),
            ) -> Result<ModelResponse, BridgeClientError> {
                panic!("不能把带输出上限的请求降级为无上限流式调用")
            }
        }
        let client = DeclaredOnlyExecutor;
        for result in [
            client.invoke_with_cancellation_and_output_limit(request(), 100, &|| false),
            client.invoke_with_retry_events_cancellation_and_output_limit(
                request(),
                100,
                &|_| {},
                &|| false,
            ),
            client.invoke_streaming_with_output_limit(request(), 100, &|_| {}, &|_| {}, &|| false),
        ] {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("executor_output_limit_unsupported")
            );
        }
    }
}

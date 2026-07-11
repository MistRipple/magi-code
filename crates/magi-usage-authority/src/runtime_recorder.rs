use crate::authority::UsageAuthority;
use crate::types::{
    ExecutionBindingIdentity, LlmConfig, UsageCallIdentity, UsageCallRecordInput, UsageCallStatus,
    UsageTokenInput,
};

pub struct RuntimeRecorder<'a> {
    authority: &'a mut UsageAuthority,
    workspace_id: String,
    session_id: String,
}

pub struct RuntimeCallRecordInput {
    pub execution_binding: ExecutionBindingIdentity,
    pub model_config: LlmConfig,
    pub call_identity: UsageCallIdentity,
    pub usage: UsageTokenInput,
    pub status: UsageCallStatus,
    pub turn_id: Option<String>,
    pub dispatch_wave_id: Option<String>,
    pub assignment_id: Option<String>,
    pub error_code: Option<String>,
}

impl<'a> RuntimeRecorder<'a> {
    pub fn new(
        authority: &'a mut UsageAuthority,
        workspace_id: String,
        session_id: String,
    ) -> Self {
        Self {
            authority,
            workspace_id,
            session_id,
        }
    }

    pub fn record_call(&mut self, input: RuntimeCallRecordInput) -> u64 {
        let input = UsageCallRecordInput {
            workspace_id: self.workspace_id.clone(),
            session_id: self.session_id.clone(),
            turn_id: input.turn_id,
            dispatch_wave_id: input.dispatch_wave_id,
            assignment_id: input.assignment_id,
            event_id: None,
            timestamp: None,
            execution_binding: input.execution_binding,
            model_config: input.model_config,
            call_identity: input.call_identity,
            usage: input.usage,
            status: input.status,
            error_code: input.error_code,
        };
        self.authority.append_call_record(input)
    }
}

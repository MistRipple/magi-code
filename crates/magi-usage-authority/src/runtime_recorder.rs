use crate::authority::UsageAuthority;
use crate::types::{
    ExecutionBindingIdentity, LlmConfig, UsageCallIdentity, UsageCallRecordInput,
    UsageCallStatus, UsageTokenInput,
};

pub struct RuntimeRecorder<'a> {
    authority: &'a mut UsageAuthority,
    workspace_id: String,
    session_id: String,
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

    pub fn record_call(
        &mut self,
        execution_binding: ExecutionBindingIdentity,
        model_config: LlmConfig,
        call_identity: UsageCallIdentity,
        usage: UsageTokenInput,
        status: UsageCallStatus,
        turn_id: Option<String>,
        dispatch_wave_id: Option<String>,
        assignment_id: Option<String>,
        error_code: Option<String>,
    ) -> u64 {
        let input = UsageCallRecordInput {
            workspace_id: self.workspace_id.clone(),
            session_id: self.session_id.clone(),
            turn_id,
            dispatch_wave_id,
            assignment_id,
            event_id: None,
            timestamp: None,
            execution_binding,
            model_config,
            call_identity,
            usage,
            status,
            error_code,
        };
        self.authority.append_call_record(input)
    }
}

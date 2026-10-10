use crate::{PluginManager, workflow::WorkflowCore, workflow_runtime::PluginWorkflowCore};
use magi_plugin_runtime::{CapabilityHandler, InvocationIdentity, PluginHost, RuntimeLimits};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowCoreInvocationSpec {
    pub session_id: String,
    pub project_id: String,
    pub run_id: String,
    pub workflow_id: String,
    pub checkpoint_version: u32,
    pub config: Value,
}

pub trait WorkflowCoreFactory: Send + Sync {
    fn supports(&self, workflow_id: &str) -> bool;
    fn build_workflow_core(
        &self,
        spec: WorkflowCoreInvocationSpec,
    ) -> Result<Arc<dyn WorkflowCore>, String>;
}

pub struct WorkflowCoreRouter {
    factories: Vec<Arc<dyn WorkflowCoreFactory>>,
}

impl WorkflowCoreRouter {
    pub fn new(factories: Vec<Arc<dyn WorkflowCoreFactory>>) -> Result<Self, String> {
        if factories.is_empty() {
            return Err("未注册工作流核心工厂".into());
        }
        Ok(Self { factories })
    }
}

impl WorkflowCoreFactory for WorkflowCoreRouter {
    fn supports(&self, workflow_id: &str) -> bool {
        self.factories
            .iter()
            .any(|factory| factory.supports(workflow_id))
    }

    fn build_workflow_core(
        &self,
        spec: WorkflowCoreInvocationSpec,
    ) -> Result<Arc<dyn WorkflowCore>, String> {
        let mut matches = self
            .factories
            .iter()
            .filter(|factory| factory.supports(&spec.workflow_id));
        let Some(factory) = matches.next() else {
            return Err(format!("未注册工作流核心：{}", spec.workflow_id));
        };
        if matches.next().is_some() {
            return Err(format!("工作流核心注册冲突：{}", spec.workflow_id));
        }
        factory.build_workflow_core(spec)
    }
}

pub struct PluginWorkflowCoreFactory {
    manager: Arc<std::sync::Mutex<PluginManager>>,
    host: PluginHost,
    handler: Arc<dyn CapabilityHandler>,
    limits: RuntimeLimits,
}

impl PluginWorkflowCoreFactory {
    pub fn new(
        manager: Arc<std::sync::Mutex<PluginManager>>,
        host: PluginHost,
        handler: Arc<dyn CapabilityHandler>,
        limits: RuntimeLimits,
    ) -> Self {
        Self {
            manager,
            host,
            handler,
            limits,
        }
    }

    fn identity(workflow_id: &str) -> Result<(String, String), String> {
        let rest = workflow_id
            .strip_prefix("plugin/")
            .ok_or_else(|| "插件工作流身份无效".to_string())?;
        let (plugin_id, contribution_id) = rest
            .rsplit_once('/')
            .filter(|(plugin, contribution)| !plugin.is_empty() && !contribution.is_empty())
            .ok_or_else(|| "插件工作流身份必须为 plugin/{插件}/{贡献}".to_string())?;
        Ok((plugin_id.to_string(), contribution_id.to_string()))
    }
}

impl WorkflowCoreFactory for PluginWorkflowCoreFactory {
    fn supports(&self, workflow_id: &str) -> bool {
        workflow_id.starts_with("plugin/")
    }

    fn build_workflow_core(
        &self,
        spec: WorkflowCoreInvocationSpec,
    ) -> Result<Arc<dyn WorkflowCore>, String> {
        let (plugin_id, contribution_id) = Self::identity(&spec.workflow_id)?;
        let scope = if spec.project_id.trim().is_empty() {
            "application".to_string()
        } else {
            format!("workspace:{}", spec.project_id)
        };
        let manager = self
            .manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        if !manager
            .is_active_for_scope(&plugin_id, &scope)
            .map_err(|error| error.to_string())?
        {
            return Err("插件工作流未在当前作用域激活".into());
        }
        let package = manager
            .package(&plugin_id)
            .map_err(|error| error.to_string())?;
        if !package
            .manifest()
            .contributions
            .workflows
            .iter()
            .any(|workflow| workflow.id == contribution_id)
        {
            return Err("插件工作流贡献不存在".into());
        }
        let identity = InvocationIdentity {
            plugin_id,
            package_digest: package.digest().to_string(),
            instance_id: scope.clone(),
            invocation_id: spec.run_id,
            workspace_id: (!spec.project_id.trim().is_empty()).then_some(spec.project_id),
            run_id: None,
            attempt_id: None,
        };
        Ok(Arc::new(PluginWorkflowCore::new(
            Arc::clone(&self.manager),
            self.host.clone(),
            package.source().to_string(),
            identity,
            Arc::clone(&self.handler),
            self.limits,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NamespaceFactory(&'static str);

    impl WorkflowCoreFactory for NamespaceFactory {
        fn supports(&self, workflow_id: &str) -> bool {
            workflow_id.starts_with(self.0)
        }

        fn build_workflow_core(
            &self,
            _spec: WorkflowCoreInvocationSpec,
        ) -> Result<Arc<dyn WorkflowCore>, String> {
            Err("测试工厂不应被调用".into())
        }
    }

    #[test]
    fn router_rejects_unregistered_and_ambiguous_workflows() {
        let router = WorkflowCoreRouter::new(vec![Arc::new(NamespaceFactory("plugin/"))])
            .expect("router should have a factory");
        assert!(router.supports("plugin/example/workflow"));
        assert!(!router.supports("plugin:example:workflow"));
        let spec = WorkflowCoreInvocationSpec {
            session_id: String::new(),
            project_id: String::new(),
            run_id: String::new(),
            workflow_id: "builtin/default".into(),
            checkpoint_version: 1,
            config: Value::Null,
        };
        let error = match router.build_workflow_core(spec) {
            Ok(_) => panic!("unregistered workflow must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("未注册工作流核心"));

        let ambiguous = WorkflowCoreRouter::new(vec![
            Arc::new(NamespaceFactory("plugin/")),
            Arc::new(NamespaceFactory("plugin/")),
        ])
        .unwrap();
        let error = match ambiguous.build_workflow_core(WorkflowCoreInvocationSpec {
            session_id: String::new(),
            project_id: String::new(),
            run_id: String::new(),
            workflow_id: "plugin/example/workflow".into(),
            checkpoint_version: 1,
            config: Value::Null,
        }) {
            Ok(_) => panic!("ambiguous workflow must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("注册冲突"));
    }
}

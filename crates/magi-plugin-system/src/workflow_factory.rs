use crate::{
    APPLICATION_SCOPE, ContributionRef, PluginManager, workflow::WorkflowCore,
    workflow_runtime::PluginWorkflowCore,
};
use magi_plugin_runtime::{CapabilityHandler, InvocationIdentity, PluginHost, RuntimeLimits};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowCoreInvocationSpec {
    pub session_id: String,
    pub project_id: String,
    pub run_id: String,
    pub workflow_id: String,
    pub package_digest: String,
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
}

impl WorkflowCoreFactory for PluginWorkflowCoreFactory {
    fn supports(&self, workflow_id: &str) -> bool {
        workflow_id.starts_with("plugin/")
    }

    fn build_workflow_core(
        &self,
        spec: WorkflowCoreInvocationSpec,
    ) -> Result<Arc<dyn WorkflowCore>, String> {
        let selected = ContributionRef::parse("工作流", &spec.workflow_id)?;
        let scope = APPLICATION_SCOPE;
        let manager = self
            .manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        let package = manager
            .prepare_workflow(&selected)
            .map_err(|error| error.to_string())?;
        if spec.package_digest != package.digest() {
            return Err("插件工作流包已变化；原检查点不能交给新版本执行".into());
        }
        let ContributionRef {
            plugin_id,
            contribution_id,
        } = selected;
        let identity = InvocationIdentity {
            plugin_id,
            package_digest: package.digest().to_string(),
            instance_id: scope.to_owned(),
            invocation_id: spec.run_id.clone(),
            workspace_id: (!spec.project_id.trim().is_empty()).then_some(spec.project_id),
            session_id: Some(spec.session_id),
            turn_id: None,
            run_id: Some(spec.run_id),
            attempt_id: None,
            contribution: Some(magi_plugin_runtime::InvocationContribution {
                kind: "workflow".into(),
                id: contribution_id,
            }),
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

    struct NoCapabilities;
    impl CapabilityHandler for NoCapabilities {
        fn call(
            &self,
            _: magi_plugin_runtime::CapabilityRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Value, magi_plugin_runtime::ExecutionError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async { panic!("版本校验必须先于 Worker 和能力执行") })
        }
    }

    #[test]
    fn workflow_checkpoint_cannot_switch_to_an_upgraded_package() {
        let root = tempfile::tempdir().unwrap();
        let package = |version| {
            crate::PluginPackage::from_manifest_and_source(
            serde_json::from_value(serde_json::json!({
                "sdkVersion":1,"id":"test.flow","version":version,"name":"Flow","description":"",
                "backend":"plugin.mjs","permissions":[],"dataSchemaVersion":1,
                "settingsSchema":{"type":"object","additionalProperties":false},
                "contributions":{"workflows":[{"id":"main","title":"Flow","description":""}]}
            })).unwrap(), "export default () => null;"
        ).unwrap()
        };
        let original = package("1.0.0");
        let upgraded = package("1.1.0");
        let mut manager = PluginManager::open(root.path()).unwrap();
        manager
            .install(
                &original,
                crate::PluginSource::Local {
                    name: "flow.zip".into(),
                },
            )
            .unwrap();
        manager.enable("test.flow", APPLICATION_SCOPE).unwrap();
        manager.activate("test.flow", APPLICATION_SCOPE).unwrap();
        let manager = Arc::new(std::sync::Mutex::new(manager));
        let factory = PluginWorkflowCoreFactory::new(
            manager.clone(),
            PluginHost::new(
                root.path().join("missing-worker"),
                magi_process::ManagedProcessGroup::new(),
            ),
            Arc::new(NoCapabilities),
            RuntimeLimits::default(),
        );
        let spec = WorkflowCoreInvocationSpec {
            session_id: "session".into(),
            project_id: String::new(),
            run_id: "run".into(),
            workflow_id: "plugin/test.flow/main".into(),
            package_digest: original.digest().into(),
            checkpoint_version: 1,
            config: serde_json::json!({"saved":"original checkpoint"}),
        };
        assert!(factory.build_workflow_core(spec.clone()).is_ok());
        let drain = PluginManager::begin_upgrade_drain(manager.clone(), "test.flow").unwrap();
        assert!(
            factory
                .build_workflow_core(spec.clone())
                .err()
                .unwrap()
                .contains("排空")
        );
        {
            let mut manager = manager.lock().unwrap();
            manager
                .upgrade(
                    &upgraded,
                    crate::PluginSource::Local {
                        name: "flow.zip".into(),
                    },
                    &drain,
                    None,
                )
                .unwrap();
            drain.commit(&mut manager);
            manager.activate("test.flow", APPLICATION_SCOPE).unwrap();
        }
        assert!(
            factory
                .build_workflow_core(spec.clone())
                .err()
                .unwrap()
                .contains("包已变化")
        );
        assert!(
            factory
                .build_workflow_core(WorkflowCoreInvocationSpec {
                    package_digest: upgraded.digest().into(),
                    ..spec
                })
                .is_ok(),
            "新的显式运行可以选择新包"
        );
    }

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
            package_digest: String::new(),
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
            package_digest: String::new(),
            checkpoint_version: 1,
            config: Value::Null,
        }) {
            Ok(_) => panic!("ambiguous workflow must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("注册冲突"));
    }
}

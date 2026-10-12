//! 升级时的数据格式转换。
//!
//! 转换由新版插件自己的后台入口执行：宿主逐条把旧数据交给 Worker，入口以
//! `sdk.invocation.contribution = {kind:"migration", id:"data"}` 识别这是转换调用。
//! 转换是纯函数——宿主不提供任何 SDK 能力（读写资源、设置、模型、网络、进程都被拒绝），
//! 只能读输入、返回结果；结果在宿主校验后写入暂存数据，提交前不会被业务使用。

use crate::{PluginError, PluginPackage, PluginResource, invalid};
use magi_plugin_runtime::{
    CapabilityHandler, CapabilityRequest, ExecutionError, ExecutionErrorCode, Invocation,
    InvocationContribution, InvocationIdentity, PluginHost, RunCancellation, RuntimeLimits,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// 一条待转换的持久数据。
#[derive(Clone, Debug, PartialEq)]
pub struct MigrationEntry {
    pub scope: String,
    pub resource_id: String,
    pub resource: PluginResource,
}

/// 升级前对现有数据的读取结果。`fingerprint` 在提交时与当前数据比对，
/// 转换期间数据若有变化（理论上排空已禁止）则整个升级作废。
#[derive(Clone, Debug)]
pub struct DataMigrationPlan {
    pub plugin_id: String,
    pub from_version: i64,
    pub to_version: i64,
    pub entries: Vec<MigrationEntry>,
}

impl DataMigrationPlan {
    pub fn fingerprint(&self) -> Vec<(String, String, u64)> {
        self.entries
            .iter()
            .map(|entry| {
                (
                    entry.scope.clone(),
                    entry.resource_id.clone(),
                    entry.resource.version,
                )
            })
            .collect()
    }
}

/// 转换后的数据，与计划的条目一一对应；`None` 表示新版不再保留该条目。
#[derive(Clone, Debug)]
pub struct ConvertedData {
    pub plan_fingerprint: Vec<(String, String, u64)>,
    pub entries: Vec<(MigrationEntry, Option<Value>)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationInput<'a> {
    from_version: i64,
    to_version: i64,
    scope: &'a str,
    resource_id: &'a str,
    value: &'a Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationOutput {
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    drop: bool,
}

/// 转换执行者。生产实现是 [`WorkerDataMigrator`]；测试可以替换。
pub trait PluginDataMigrator: Send + Sync {
    fn convert(
        &self,
        package: &PluginPackage,
        plan: &DataMigrationPlan,
    ) -> Result<ConvertedData, PluginError>;
}

/// 拒绝一切 SDK 能力的处理器：转换不得产生业务副作用。
struct DenyAll;

impl CapabilityHandler for DenyAll {
    fn call(
        &self,
        _request: CapabilityRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ExecutionError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionError::new(
                ExecutionErrorCode::CapabilityRejected,
                "数据转换不开放任何宿主能力",
            ))
        })
    }
}

/// 在受 OS 沙箱限制的 QuickJS Worker 中执行转换。
pub struct WorkerDataMigrator {
    host: PluginHost,
    limits: RuntimeLimits,
}

impl WorkerDataMigrator {
    pub fn new(host: PluginHost) -> Self {
        Self {
            host,
            limits: RuntimeLimits::default(),
        }
    }
}

impl PluginDataMigrator for WorkerDataMigrator {
    fn convert(
        &self,
        package: &PluginPackage,
        plan: &DataMigrationPlan,
    ) -> Result<ConvertedData, PluginError> {
        let cancellation = RunCancellation::default();
        let mut converted = Vec::with_capacity(plan.entries.len());
        for (index, entry) in plan.entries.iter().enumerate() {
            let input = serde_json::to_value(MigrationInput {
                from_version: plan.from_version,
                to_version: plan.to_version,
                scope: &entry.scope,
                resource_id: &entry.resource_id,
                value: &entry.resource.value,
            })
            .map_err(|_| invalid("数据转换输入无效"))?;
            let invocation = Invocation {
                identity: InvocationIdentity {
                    plugin_id: plan.plugin_id.clone(),
                    package_digest: package.digest().to_owned(),
                    instance_id: "migration".into(),
                    invocation_id: format!("migration:{}:{index}", package.digest()),
                    workspace_id: None,
                    session_id: None,
                    turn_id: None,
                    run_id: None,
                    attempt_id: None,
                    contribution: Some(InvocationContribution {
                        kind: "migration".into(),
                        id: "data".into(),
                    }),
                },
                source: package.source().to_owned(),
                input,
                limits: self.limits,
            };
            let output = run_blocking(&self.host, invocation, &cancellation).map_err(|error| {
                invalid(format!(
                    "数据转换失败（{} / {}）：{error}",
                    entry.scope, entry.resource_id
                ))
            })?;
            let output: MigrationOutput = serde_json::from_value(output)
                .map_err(|_| invalid("数据转换必须返回 {value} 或 {drop:true}"))?;
            let value = match (output.drop, output.value) {
                (true, None) => None,
                (false, Some(value)) => Some(value),
                _ => return Err(invalid("数据转换必须返回 {value} 或 {drop:true}，且二选一")),
            };
            converted.push((entry.clone(), value));
        }
        Ok(ConvertedData {
            plan_fingerprint: plan.fingerprint(),
            entries: converted,
        })
    }
}

fn run_blocking(
    host: &PluginHost,
    invocation: Invocation,
    cancellation: &RunCancellation,
) -> Result<Value, ExecutionError> {
    let host = host.clone();
    let cancellation = cancellation.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| {
                ExecutionError::new(ExecutionErrorCode::WorkerUnavailable, "插件执行器不可用")
            })
            .and_then(|runtime| runtime.block_on(host.invoke(invocation, &DenyAll, &cancellation)))
    })
    .join()
    .map_err(|_| {
        ExecutionError::new(
            ExecutionErrorCode::WorkerUnavailable,
            "插件执行线程异常退出",
        )
    })?
}

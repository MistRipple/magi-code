# 配置访问与来源同步内部合同

本批补齐公开 Loader 对象和 Include 同步所需的内部访问合同；没有注册公开 `ctx.loader`，不计 Mgr 页面或阶段 1 业务通过。完整记录见 [result.json](result.json)。

## 结果与归属

原 Fiber 保存队列已由同一个 `ConfigurationAccess` 承担，后续对象变更与同步使用同一队列、容量和调用作用域。普通操作在调用时复制；连续 Fiber 保存按其已确认配置构造下一次提案。每次失败仍回给原调用者并记录诊断，后续显式动作不会重试旧动作。回执不得早于请求修订，来源写入和配置裁决仍在 `PluginManager` 原入口。

内部 `loading.sources.synchronize` 校验实际实例、执行作用域、最低修订与待恢复事务。普通同步等待已接纳图应用；只有显式刷新才重捕获文件，并经原配置接纳方法提交。接纳和图应用依旧分别串行，不在持有准入锁时等待执行。同步不等待所有业务启动，也不把 `starting` 变成成功业务。

真实 Context 决定调用所属实例，子插件沿所属根实例使用合同，其他 Context 不获得写入权。同步在调用作用域撤销时向调用者结束等待，包括仍排队的情况；迟到回执不能复活作用域或提高其可用修订。保存回执仍表示持久接纳，不能因后续实例失效而谎称回滚。

## 验证

- Node 55 项通过，含排序、不可变提案、连续保存基础值、错误与非法回执、容量、排队／在途同步失效及迟到回复。实际 Cordis Context 验证根／子插件归属，拒绝其他 Context，撤除实例后旧访问不能发起请求。
- 两 crate 全目标验证 181 项通过，5 个原包专项默认忽略；它们在本批同一次显式原包运行中全部通过。clippy `-D warnings`、相关 Rust 格式及差异空白检查通过。
- `host_synchronization_joins_saved_intent_and_refreshes_only_when_requested` 通过生产 `LoadingHostServices` 处理器与真实 Node 图执行验证：已保存修订 2 在同步前尚未执行，同步后才运行；外部文件变更不会被普通同步冒充已读取，显式刷新接纳修订 3。坏文件保留原文与已生效配置，修复后接纳修订 4；未来修订、过期作用域、未知字段与已停用调用者拒绝。
- `host_synchronization_does_not_wait_for_business_startup` 验证同步在真实插件仍 `starting` 时返回已应用修订 2；随后接纳提供方至修订 3，该插件才完成其实际启动。没有把它伪装成提前就绪。

新同步请求的领域处理与 Node 调用队列分别验证；尚未由公开 Loader 对象让原包端到端发起该请求。原 Fiber 保存通过真实 Node 双向请求继续回归。此范围不能当作公开 `Include.flush/refresh` 已交付。

同一 `dsh-plugin-mgr@0.2.13` 使用独立项目 `target/dsh-phase-1/projects/mgr-source-sync-2026-11-18`，执行前后验证 tarball、全部 9 个原包文件、运行脚本及依赖锁。静态准备、原图应用、文件通知启停、单行编辑及整组／根树移动在同一批次通过；原包字节不变，实际仍等待 `loader`、`webServer`，业务为 **not-run**。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-source-sync-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host --locked -- --ignored --nocapture
```

## 待交付

公开 Loader／Entry／Group 的对象和选项合同、Include `flush/refresh` 的正式对象接线、动态来源、安装事务和发行 daemon 协调器仍需完成。真实 Electron、Mgr 页面和安装／更新／卸载业务、完整应用重启及 Windows/Linux 未验收。未测性能，也不计算生态支持率。

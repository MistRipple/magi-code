# Loader 内存根的正式接纳与恢复

本批解除公开 Loader 的根树阻塞：执行器不再固定创建 Include；真实根条目与文件树在同一已接纳图、配置 CAS 和执行入口中管理。当前仍是内部加载能力，公开 `ctx.loader`、Mgr 页面业务和阶段 1 未交付。结果与源码／依赖摘要见 [result.json](result.json)。

## 合同与实现

内部图版本 2 的 `root.entries` 是真实 Loader 根。首次准备由应用创建普通 Include；后续准备保留已接纳的根意愿，包括空根。根操作使用空字符串树 ID，条目 ID 不加前缀；文件树使用实际 Include ID。创建、更新、移除、整组与跨树移动复用原候选准备；普通模块仍从同一已准入安装物导入，所有可执行实例仍由 daemon 签发身份。

根意愿仅保存在 `PluginManager` 原配置图内，不生成影子文件。纯根操作虽无文件编辑，也须提交原配置 CAS。混合变更由原 `pendingEdit` 保存完整图及文件意图；普通文件通知不能提前提交缺少根变更的计划。根 fiber 保存复用现有双向请求与运行作用域校验。根和文件输入共用结构及 JSON 值校验，防止非有限数字在传输中变为 `null`。

Include 的文件补丁层与自有补丁保留真实来源归属：前者保存对应文件，后者保存父配置；父级是内存根时只提交根图。有效补丁随已接纳节点进入执行器。删除了旧的固定 Include 引导及只允许文件归属的根 fiber 保存限制；没有第二套根状态或执行路径。

## 运行证据

- Node 49 项通过，涵盖根增删改、顺序、整组、清空、根／文件往返、来源归属、非法身份与路径、数据边界及既有来源和生命周期行为。已有断言随实际根层级改为检查 Include 子树，文件语义和业务标准保持原要求。
- 两 crate 全目标定向验证 179 项通过、5 项原包专项默认忽略；这些默认忽略项在本批同一次显式原包运行中全部通过。clippy `-D warnings`、相关 Rust 格式和差异空白检查通过。
- `memory_root_entries_use_the_same_commit_and_survive_a_real_host_rebuild`：根业务仅在修订 2 提交后执行；配置更新保持实例身份，非法候选与旧修订不写入；根／文件移动切换身份。停止旧 Node，重开真实管理器并启动新宿主后，同一修订 5 恢复根配置，重新签发宿主和激活代次。随后删除 Include、清空根至修订 7，不改文件或补回默认 Include。
- `a_memory_root_fiber_saves_through_the_owner_before_the_new_config_executes`：根插件在启动中保存配置；原来源监视器驱动修订 3 应用，业务读到已提交的新配置；两个配置文件逐字不变。
- `an_interrupted_file_to_root_move_recovers_one_complete_configuration`：模拟来源文件已替换而 CAS 未完成。普通文件通知拒绝缺少根变化的候选，旧已接纳图保持修订 1。停止宿主、重开管理器后，原编辑意图完成整张图至修订 2，不执行或重放业务。

固定 `dsh-plugin-mgr@0.2.13` 使用独立项目 `target/dsh-phase-1/projects/mgr-memory-root-2026-11-18`。执行前后验证 tarball、全部 9 个原包文件、依赖锁和运行脚本。静态准备、原图应用、文件启停、单行编辑、整组及根树移动在同一原包批次执行。整组用例经过修订 1 至 8，包含组内、来源根与真正内存根的往返；旧身份失效，原包字节不变。实际仍缺少 `loader`、`webServer`，业务记录为 **not-run**。

```sh
MAGI_LOADING_PREPARED_PROJECT="$PWD/target/dsh-phase-1/projects/mgr-memory-root-2026-11-18" \
  cargo test -p magi-plugin-system --test loading_host --locked -- --ignored --nocapture
```

## 未验证与待交付

公开 Loader／Entry／Group 对象及完整选项合同、Include `flush`、动态来源、安装事务和发行 daemon 协调器仍需完成。真实 Electron、Mgr 页面及安装／升级／卸载业务、完整应用重启和 Windows/Linux 未验收。上述宿主重建由真实领域组件与 Node 进程完成，不是完整发行应用的重启验收。未测性能，不计算插件生态支持率。

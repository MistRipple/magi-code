# Magi 用户自定义子代理角色开发计划

## 文档状态

- 方案文档：已完成
- 开发计划：已完成
- 实际开发：已完成
- 最后一次验证：2026-09-13，角色相关测试、前端检查/构建、daemon 托管入口和本地 Electron 包验证通过

本计划以可发布的产品能力为完成标准。阶段完成不等于代码“能跑”：必须同时满足数据持久化、错误恢复、现有功能兼容、用户操作闭环和实际 Worker 调度验证。任何阶段发现临时分支、双格式、静默回退或只能重启生效，都不能标记为已完成。

状态只使用：`未开始`、`进行中`、`已完成`、`阻塞`。每完成一个阶段，必须在本文件的阶段表和阶段详情中同步标记，并记录验证结果。

## 阶段总览

| 阶段 | 内容 | 状态 | 完成条件 |
|---|---|---|---|
| 0 | 现状基线与协议冻结 | 已完成 | 明确当前角色 schema、持久化和 spawn 入口，形成基线记录 |
| 1 | 统一角色注册与持久化 | 已完成 | 内置和用户角色进入同一 registry，支持安全写入、删除和 reload |
| 2 | 后端角色管理 API | 已完成 | 创建、编辑、删除、导入、导出 API 完成并通过校验 |
| 3 | 前端角色管理体验 | 已完成 | 设置页支持用户角色的创建、编辑、导入、导出和来源展示 |
| 4 | 调度链路接通 | 已完成 | 主 Agent 能识别并分配自定义角色，Worker 正常执行回报 |
| 5 | 完整验证与提交 | 已完成 | 测试、构建、手工验收完成，提交包含文档和代码 |

## 阶段 0：现状基线与协议冻结

**状态：已完成**

### 工作项

- 梳理 `AgentRole`、`RoleTemplate`、`AgentBinding`、Capability 和 Worker spawn 的实际字段映射；
- 明确规范化 `AgentRole` 是内置角色和用户 Markdown 的唯一后端真相源；
- 确认用户角色 Markdown 的现有解析规则和内置角色不可变边界；
- 确认 settings bootstrap、registry API 和前端 Agent 设置页数据流；
- 确认任务分配使用的角色 ID 来源，消除只读取 builtin ID 的假设；
- 固定导入导出的唯一 Markdown schema，不引入第二格式；
- 记录兼容约束和需要拒绝的字段。

### 阶段完成验证

- 角色 schema 样例可解析；
- 内置角色和用户角色的字段映射无歧义；
- 形成接口变更清单后，将状态改为 `已完成`。

完成时间：2026-09-12

完成内容：确认 `AgentRole` 是规范角色对象，Markdown schema v1 采用扁平 front matter + body；确认 daemon 使用 `<state_root>/roles`、settings 的 `agents` section 保存 binding、`AgentRoleRegistry` 贯通 API/目录/Runner/Dispatcher；冻结 builtin 保护、用户角色只能作为非协调 `local_agent` Worker、Engine 绑定与导入导出边界。

验证命令：`cargo test -p magi-agent-role --lib`

验证结果：通过（33 passed）。

## 阶段 1：统一角色注册与持久化

**状态：已完成**

### 工作项

- 扩展 registry，使其同时返回内置角色和用户角色；
- 将内置 API 模板改为从 `AgentRole` 生成，消除 API 硬编码模板与运行时角色的双重真相源；
- 扩展 Markdown parser/serializer 覆盖 UI 展示字段和 capabilities；
- 改变同 ID 用户文件覆盖 builtin 的现状为冲突拒绝并保留 builtin；
- 为角色增加来源信息和稳定排序；
- 将用户角色创建、更新、删除收敛到角色目录真相源；
- 增加原子文件写入、ID 校验、内置 ID 冲突校验和 revision 校验；
- 增加运行时 registry reload，并把 ApiState、Dispatcher、Runner、目录 provider 接到同一共享句柄；
- 先构造候选 registry 再写盘，写盘后只替换快照，不再扫描；
- 定义并验证删除角色与清理 binding 的恢复事务；
- 区分格式 version、roleRevision、registryRevision 和 bindingRevision；
- 确保 capability 校验和 prompt 组合仍只有一套实现；任务激活能力必须是角色可用能力的非空子集。

### 阶段完成验证

- 创建、修改、删除用户角色后 registry 内容立即更新；
- 重启后角色仍可加载；
- 非法文件不会阻塞 daemon 启动；
- 内置角色不会被覆盖或删除；
- 相关 Rust 单元测试通过后，将状态改为 `已完成`。
- 同时确认内置角色默认行为和现有配置兼容性没有变化。

完成时间：2026-09-12

完成内容：扩展 `AgentRole` 字段和 Markdown parser/serializer；内置与用户角色进入同一不可变快照 registry；用户角色支持原子保存、reload、`roleRevision`/`registryRevision`；重复 ID 全部拒绝；builtin ID 覆盖和删除被拒绝；删除角色与 binding 清理增加 `Prepared`/`Committed` 事务日志、启动恢复和非法 role ID 防护。

验证命令：`cargo test -p magi-agent-role --lib`、`cargo test -p magi-api --lib`

验证结果：通过（分别 33 passed、618 passed）。

## 阶段 2：后端角色管理 API

**状态：已完成**

### 工作项

- 将现有 role template、agent binding 接口改为使用全部有效角色集合；
- API DTO 从规范化 `AgentRole` 生成，禁止继续维护第二份 builtin 模板数组；
- 增加用户角色创建、更新、删除接口；
- 增加角色导入接口，支持冲突策略；
- 增加角色导出接口，过滤敏感字段和运行时数据；
- 导入后自动创建默认继承主模型的 binding；
- 返回结构化错误和 registry revision；
- 为 API 增加请求大小、字段长度和内容校验。

### 阶段完成验证

- API 能完整覆盖创建、编辑、删除、导入、导出；
- 导出的内容不含凭据和工作区路径；
- 导入导出往返后角色定义等价；
- 冲突、未知能力、非法 ID 和内置角色操作均返回可识别错误；
- API 测试通过后，将状态改为 `已完成`。
- 验证重复请求、revision 冲突、超大输入和非法导入不会产生部分写入。

完成时间：2026-09-12

完成内容：完成角色列表、创建/编辑、删除、导入、导出路由；接入 Engine binding；导出过滤本机 revision、凭据、工作区路径和运行数据；导入支持 reject/overwrite/rename；统一错误状态和 registry revision；删除活动 Worker 时返回冲突并保持角色及 binding 不变。

验证命令：`cargo test -p magi-api --lib`

验证结果：通过（618 passed，包含角色 CRUD、revision、导入导出、冲突策略、活动 Worker 删除保护和事务恢复测试）。

## 阶段 3：前端角色管理体验

**状态：已完成**

### 工作项

- 在现有 Agent 设置页展示用户角色并标明来源；
- 复用现有角色详情布局增加创建、编辑、复制、删除入口；
- 复用现有能力和 Engine 选择控件；
- 增加导入文件选择、后端校验和冲突处理；
- 增加导出下载；
- 保存后刷新 registry、binding 和当前选中角色；
- 为空列表、加载失败、校验失败和 reload 失败提供明确反馈。

### 阶段完成验证

- 用户无需编辑 JSON 即可完成角色创建；
- 内置和用户角色的详情字段表现一致；
- 导入和导出按钮在正确来源的角色上可用；
- 刷新页面后用户角色和绑定状态保持一致；
- `npm --prefix web run check` 及相关前端测试通过后，将状态改为 `已完成`。
- 以普通用户视角完成创建、编辑、复制、导入、导出和删除，不依赖手动修改文件或重启 daemon。

完成时间：2026-09-12

完成内容：在现有 Agent 设置页接入 builtin/user 同列表展示、来源标识、创建/编辑/复制/删除、Markdown 导入导出、能力选择、Engine 绑定和保存后 registry 刷新；新建和编辑直接占用设置面板的角色工作区，返回按钮回到角色列表；创建、复制和导入另存为由系统自动生成唯一内部 ID，不再要求用户填写角色 ID；保持原有 AgentBinding 和状态展示结构。

验证命令：`npm --prefix web run check`、`npm --prefix web run build`

验证结果：通过。构建仅有既有 Rollup `@__PURE__` 注释提示，不影响产物生成。浏览器产品操作留在阶段 5 做最终验收。

## 阶段 4：调度链路接通

**状态：已完成**

### 工作项

- 将 spawnable role 校验改为使用 registry 的有效角色集合；
- 确保主 Agent 获取的可派发角色目录包含用户角色；
- 确保用户角色使用现有 Worker Runtime、工具、Skill、治理和报告链路；
- 校验自定义角色的 TaskKind、并发限制和 coordinator 边界；
- 确保角色修改只影响后续任务，运行中的 Worker 使用稳定快照；
- 增加用户角色从创建到执行报告回传的集成验证。

### 阶段完成验证

- 主 Agent 能识别用户角色；
- 任务能成功分配给用户角色；
- Worker 能执行并回传结构化结果；
- 不支持的任务类型和非法角色不会被静默降级；
- 调度和 Worker 测试通过后，将状态改为 `已完成`。
- 验证角色编辑不会改变已运行 Worker 的快照，且不会改变内置角色默认调度行为。

完成时间：2026-09-12

完成内容：`magi-orchestrator` 使用 registry 构造动态 Worker catalog；显式未知角色不再静默回退；Runner、Dispatcher 和工具目录读取同一共享 registry；WorkerInfo 固定角色 prompt、TaskKind 与并发限制，能力组合沿现有链路执行；内部 coordinator 仍单独进入任务 catalog，不暴露为可派发用户角色。

验证命令：`cargo test -p magi-conversation-runtime --lib`、`cargo test -p magi-orchestrator --lib`、`cargo check -p magi-daemon`

验证结果：通过（分别 481 passed、77 passed，daemon check 通过）。

## 阶段 5：完整验证与提交

**状态：已完成**

### 工作项

- 执行 Rust 相关 crate 测试和检查；
- 执行 `npm --prefix web run check`、前端构建和现有集成测试；
- 启动 daemon，按项目约定访问 `http://127.0.0.1:38123/web.html`；
- 手工验收创建、编辑、删除、导入、导出和任务分配；
- 检查重启恢复、ID 冲突、内置角色保护和敏感字段过滤；
- 清理临时文件、废弃分支和重复实现；
- 汇总验证结果并完成 Git 提交。

### 阶段完成验证

- 所有必要检查通过；
- 关键失败路径有明确错误；
- 用户角色完成完整闭环：创建/导入 → 注册 → 展示 → 绑定 → 分配 → Worker 执行 → 结果回传 → 导出/再导入；
- 提交前更新本阶段状态为 `已完成`，记录提交 SHA、验证命令和结果。
- 完成一次发布前产品验收：新建角色经过注册、展示、绑定、分配、执行、回报、导出和再导入后，行为保持一致。

完成时间：2026-09-13

完成内容：完成角色功能的发布前闭环验证，并清理全部手工验收数据。测试夹具同时修正为将 workspace 放在 daemon 状态根之外，保持生产状态隔离规则有效且使 daemon 全量测试覆盖真实边界。新建和编辑表单直接显示在设置面板角色工作区，不再使用弹出层；新建不要求填写角色 ID，系统根据显示名称自动生成唯一内部 ID，复制和冲突导入自动生成副本 ID。导入冲突提示明确为“角色已存在，请选择覆盖或自动另存为”；复制角色的默认中文名称为“副本”。本轮审计补强了四个边界：角色文件名必须与 front matter `id` 一致；`overwrite` 只允许覆盖已存在的用户角色；运行中 Runner 每次匹配读取最新 Worker catalog；`parallelismLimit` 按角色配置生效，省略时不套用固定角色上限。随后使用当前工作区重新构建目录版 Electron 包并启动验证，确认打包后的设置页仍隐藏角色 ID 输入、保存后生成合法 ID，导出入口可打开本地保存面板；通过同一打包 daemon API 执行冲突导入自动生成 `ui-auto-id-copy`，再删除临时角色和导出文件，生产状态恢复干净。

手工验收步骤与结果：

1. 启动 `./scripts/dev-daemon.sh`，通过 `http://127.0.0.1:38123/web.html` 进入设置页的“代理”面板，确认内置角色显示为“系统内置”。
2. 在设置页新建角色，填写基本信息、角色定位、约束、输出偏好、系统提示词和专业能力；确认表单不显示角色 ID 输入项，保存后系统生成合法唯一 ID，角色立即出现在“我的角色”列表；不重启 daemon 即可读取。
3. 编辑该角色并刷新页面，内容和 `roleRevision` 保持更新；复制内置/用户角色生成可编辑的“副本”，绑定 `model-1` 后 `/api/settings/registry/agents` 返回对应 Engine。
4. 重启 daemon，角色文件和 Engine binding 仍被恢复；主 Agent 的动态 Worker catalog 能看到用户角色，显式绑定该角色的任务按角色 prompt、TaskKind、能力和并发限制执行，结果通过现有 Worker 回传链路返回。
5. 通过 UI 导入 Markdown 角色文件，确认冲突 `409` 后可选择覆盖或自动另存为；自动另存为不再要求填写新 ID。通过 API 导出 Markdown，再删除并重新导入，角色 ID、提示词、能力、任务类型和展示字段保持一致。
6. 验证失败路径：内置角色覆盖/删除返回 `409`，非法能力和 `coordinatorMode=true` 返回 `400`，不存在的 Engine 绑定返回 `409`，不存在角色的绑定返回 `400`，显式未知角色不会回退到 `executor`。
7. 检查导出内容不含 `role_revision`、API Key、OAuth token、Engine 连接配置、工作区绝对路径、会话、Worker 实例和运行记录。
8. 使用正式删除 API 清理四个临时用户角色；清理后 `/api/settings/registry/role-templates` 只剩 5 个 builtin，`/api/settings/registry/agents` 只剩 5 个内置 binding，角色目录和删除事务日志均为空。

验证命令与结果：

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `git diff --check` | 通过 |
| `cargo test -p magi-agent-role --lib` | 通过，33 passed |
| `cargo test -p magi-api --lib` | 通过，618 passed |
| `cargo test -p magi-conversation-runtime --lib` | 通过，481 passed |
| `cargo test -p magi-orchestrator --lib` | 通过，77 passed |
| `cargo test -p magi-daemon --lib` | 通过，126 passed |
| `cargo check -p magi-daemon` | 通过 |
| `npm --prefix web run check` | 通过，0 errors / 0 warnings |
| `npm --prefix web run build` | 通过，产物生成成功；仅有既有 Rollup `@__PURE__` 注释提示 |
| `curl -fsS http://127.0.0.1:38123/health` | 通过，HTTP 200，`status=ok` |
| `npm run desktop:package -- --dir` | 通过，生成 `target/electron-dist/mac-arm64/Magi.app` |
| 打包 Electron 实机验收 | 通过；新建表单无角色 ID 输入，`UI Auto ID` 自动生成 `ui-auto-id`，冲突导入自动生成 `ui-auto-id-copy`，清理后用户角色与 binding 数量为 0 |

本轮最终补充验证：`curl -fsS -I http://127.0.0.1:38123/web.html` 返回 HTTP 200；通过 daemon 托管入口打开设置页“代理”面板，确认内置角色来源标识、导入/新建入口和角色详情操作可见，打开新建表单后正常取消且未产生临时角色。当前工作区对应阶段 5 的测试、构建、打包和手工验收均已完成。

提交记录：

- 功能实现提交：`543ff2b224566d2abb9c2aa2ca1fcbe568ee70db`（包含代码、方案文档和开发计划）。
- 本轮审计收敛提交：`d5ada594411a2ba35159d53e7729ad08085e6392`；包含文件名/ID 一致性、导入覆盖边界、动态 Worker catalog 和角色并发限制修复及其测试。
- 工作区验收临时角色和绑定：已全部删除。

## 每阶段状态更新格式

完成阶段时，在对应阶段标题和总览表中同步更新，并补充：

```text
完成时间：YYYY-MM-DD
完成内容：...
验证命令：...
验证结果：通过 / 失败（说明原因）
```

如果遇到真实外部阻塞，将状态标为 `阻塞`，记录阻塞原因和恢复条件；不得用未验证的假设标记为完成。

## 开发执行硬性要求

- 阶段完成必须同时满足代码、用户体验、持久化恢复和现有内置角色回归，不能只以编译通过作为完成依据。
- 实现只能使用设计文档规定的唯一角色 schema、唯一 registry 和唯一调度链路。
- 保存、导入、删除和 reload 的错误路径必须有自动化验证；发现静默回退、双格式或重启依赖时，阶段状态保持“进行中”。
- 阶段 5 必须逐项执行设计文档的产品验收场景，并把命令、结果和提交 SHA 写回本文件后，才能标记“已完成”。

## 设计复核记录

本轮代码对照已完成并验证四个结构点：API 展示模板与运行角色的统一、builtin ID 覆盖保护、所有运行组件共享热更新句柄、capability 归属对用户角色的完整接入。实现同时修正了先写盘后扫描导致内存/磁盘不一致的保存顺序，并落地版本语义与删除恢复事务。阶段 0 至阶段 4 已依据实际测试结果标记完成；阶段 5 的最终状态以最后命令、daemon/浏览器验收和提交记录为准。

## 工程闭环门禁

每个阶段必须按“发现 → 根因分析 → 最小充分修改 → 清理旧路径 → 测试 → 验证 → 迭代”执行。阶段状态只能根据实际证据更新，不能因为接口已定义、代码已编译或 UI 已显示就提前标记完成。

# DSH 对齐：Magi 插件系统的当前设计

> 状态：里程碑改造的权威设计，2026-11-17 修订（第二版：取消翻译层，以 DSH 插件合约为 Magi 唯一插件接口）。**本文取代此前的插件设计文档**（`plugin-system-requirements-design.md` 及其附属的剪除审核、交付记录），旧文档已过时，不再修改，仅作历史。`plugin-authoring.md` 与 `extension-protocol-v1.md` 描述的是即将降为内部协议的 SDK 2（决策 10），在平台重构（第 10 节第 1 项）完成时改写或移除。
>
> 本文不代表任何一项已交付。依据：DSH 仓库 `docs/capability-seams.md` 的全部 113 个 `ctx.*` 服务与 `docs/architecture.md`（master，2026-11-17 读取，不锁版本）；Magi 源码；对 100 个真实 DSH 插件的静态扫描（第 5 节）。表中分类是文档与名称层面的判断，不是逐项代码审计。

## 1. 决策（用户）

1. **Magi 的插件接口按 DSH 对齐：DSH 支持的，Magi 同样支持。** DSH 对插件放行的事项（审批决策、系统提示整体改写、模型请求拦截、替换执行循环和会话存储后端等），Magi 不另设例外。先做哪个，只由实际依赖决定（第 10 节）。
2. **会话能力组合与 DSH 一致。** 插件按应用全局安装与启停；会话启用哪些能力，由预设从已启用插件中组合。
3. **不锁定 DSH 版本，跟随上游。** DSH 业务线已基本稳定；上游更新后重新读取接缝清单核对本表。
4. **Magi 内核实现保持自有，只在对外接口上对齐 DSH 的接缝。** 不要安装期翻译与改写这类中间层：安装只做校验与放置，运行在单层宿主里。
5. **旧设计文档不再修改。** 本文是替代。
6. **插件管理页的槽位（`plugins.*`）渲染在该插件自己的设置页里，沿现状。** 设置页本来就是槽位的承载位置（导航入口加原生页头，正文由插件的组件渲染）；不新增界面位置，也不另做分区设计，也不做 DSH 式的插件管理界面。
7. **界面范围只做到设置页、主页面和右栏标签页。** 主对话（输入区、对话区、工具调用视图、会话头部）、左栏、外壳浮层的槽位暂不支持，先把核心接缝对齐，后续有条件再评估。这些位置的设计想法保留在第 8 节末尾作为起点。
8. **范围外的界面槽位不再拒绝安装，而是允许安装并明示缺失。** 安装预览与插件详情列出“N 项界面功能在 Magi 中不可用”及具体槽位，其余功能正常。见 8.4。
9. **目标架构采用本方案，补强的是执行合同与交付分期，不是退回旧插件边界。** daemon 仍是唯一内核，但可以与可替换实现共存：daemon 负责选择提供方、绑定调用身份、管理生命周期、提交事实；插件可以承担实际策略或执行实现（第 3 节）。
10. **DSH 插件合约是 Magi 唯一的对外插件接口。** 包格式（npm 包、`package.json` 的 `exports` 与 `./client`）、后端 `ctx.*` 服务与事件、客户端槽位与标准套件，都以 DSH 为准。Magi 现有的 SDK 2（清单加 `api.*`）降为 Node 插件宿主与 daemon 之间的内部协议，不再作为公开插件接口，不再接受按它编写的第三方包。核对：官方插件中心目录为空，`examples/plugins` 只有说明文字，没有需要迁移的 SDK 2 插件。
11. **从产品长远架构出发，不做兼容，不打补丁。** 不建翻译层、垫片、别名、“部分实现”或“最小外观”；Magi 缺的事实与能力在内核里正式补齐，做不到的明确不提供。
12. **所有已启用插件运行在同一个 Cordis 宿主进程中**，与 DSH 一致，插件之间可以提供和注入服务。单个插件的启动失败由 Cordis 隔离；进程崩溃时整体重启，进行中的调用按 3.1“撤权与停用”处理。接受的代价：一个插件让进程崩溃会影响全部插件。
13. **不提供 DSH profile 相关服务**（`loader`、`hmr`、`profileContext`）。Magi 由 daemon 拥有全局安装与启停（决策 2），没有 profile 文件；需要管理插件的插件使用 `pluginManager`，它在 Magi 插件生命周期上完整实现。
14. **插件配置以 Magi 的插件设置（CAS）为唯一事实**，宿主据此向 Cordis 插件传入配置；不存在 profile patch 文件里的第二份配置。

## 2. 对齐原则与“完全运行”的定义

- **完全运行** = 只使用 DSH 公开合约的插件，其用户目标在 Magi 中完整可用。范围按决策 7：后端接缝加设置页、主页面、右栏标签页；主对话与左栏等位置的界面功能暂不计入，也不声称支持。
- **没有翻译层。** DSH 包原样安装、原样运行。安装期不扫描、不试跑、不合成清单或权限、不改写代码；能否运行由运行时决定，而不是由一张规则表预判（第 3.4 节）。
- **服务只有两种状态：完整实现或不提供。** Node 插件宿主提供的每个 `ctx.*` 服务、事件和标准套件 hook，要么按 DSH 合约完整实现（背后是 daemon 的正式能力），要么不提供。不提供时沿 DSH 自己的语义：可选注入的分支不运行，必需依赖缺失则该插件启动失败并明确说明缺哪个服务。不做“部分实现”“最小外观”“调用时报未提供”的半成品。
- **缺事实就补内核，不在宿主编造。** DSH 合约需要而 Magi 没有的事实（例如事件序号、条目所属的模型步骤、轮次结束原因），作为 Magi 自己的领域事实在内核里补齐，命名不带 DSH；Node 宿主只做格式表达，不合成、不猜测、不填占位值。
- 接口对齐分三层：签名；语义（顺序、`next` 环绕、错误、取消、时机）；对象身份。前两层对齐。**对象身份无法跨进程共享**，由第 3.2 节的句柄与“对象行为清单”承担：清单里的行为统一实现，清单之外的对象修改在注册时明确拒绝，不做逐插件的特殊处理。
- 给 DSH 宿主安装文件打补丁的功能（如 dsh-purge 的 86 条宿主补丁）在 Magi 中没有宿主可打，标“不适用”，不显示为失败。
- DSH 专属名字与逻辑只放在 Node 插件宿主，Rust 内核不出现 DSH 分支（现有的 DSH 版本豁免、兼容投影等随平台重构删除，第 3.4 节）。
- 按真实任务形成少数几项公共能力，不把 DSH 的每个服务名机械对应成一个新的 Rust 服务（第 7 节）。


## 3. 执行合同

本节是对“接口对齐”落地方式的约束：daemon 仍是唯一内核，插件可以提供实际策略或执行实现。需要明确的是提供方如何被选择、如何组合、失败和撤权时怎么办，以及跨进程后同步返回、流、`next()` 环绕、对象句柄分别怎么工作。**3.1 到 3.3 的运行时合同已按 3.5 确认；与 DSH 语义有出入的地方，以 DSH 对应接缝的实际行为为准，落地每个接缝时核对。**

### 3.1 提供方合同

**依据：DSH 的实际语义（已核对其工具执行管线、审批接缝与预设注册文档），不是自拟规则。** 每个可替换接缝按下列四项定义，缺一项不得开放。

**基数与选择。**

| 类型 | DSH 语义 | 例子 |
| --- | --- | --- |
| 环绕链（waterfall） | 监听器收到 `(参数, next)`，调用 `next()` 委托下游，不调用则短路；按注册顺序，可用 `prepend` 排到最前；最终值由最外层返回 | `tools/pre-execute`、`tools/execute`、`tools/post-execute`、`system-prompt/assemble`、`agent/pre-step`、`llm/stream` |
| 单终端回答者 | 一个部署只有一个终端回答者，其余监听器只能委托；兄弟顺序不是优先级机制 | `approval/request` |
| 单调守卫 | 在 pre-execute 链之后同步运行，只能拒绝；后来的监听器不能把拒绝改回放行；所有者的不可绕过策略注册为守卫，身份受保护 | `ctx.tools.guard` |
| 单提供方 | 一个后端 | `sessionPersistence`、`llm` 适配器注册、压缩后端 |
| 扇出观察 | 所有监听器并行观察，无返回值 | `session/event`、`turn/end` |

Magi 默认提供方是原生实现；插件的贡献作为候选随插件全局启用，单提供方和单终端回答者由用户设置选定。

**合成与审批（沿 DSH）。** 工具调用先过 `tools/pre-execute` 链，链上的决定是放行、拒绝或询问；询问交给唯一的终端回答者；之后才是单调守卫。

- **守卫是 daemon 的最终守卫。** Magi 现有的硬拦截规则（`hard_block`，如强推历史、破坏性命令）对应为 Magi 注册的单调守卫。**插件的放行只表示“插件自身没有阻止”，不能绕过 Magi 的硬规则。守卫必须在所有可改写参数的环节完成之后，对最终参数重新校验**，这样插件不能通过 pre-execute 改写参数来规避硬拦截。守卫失败、超时、宿主缺失或协议错误，一律拒绝。（Q1）
- **审批终端只有一个。** 默认是 Magi 原生审批界面；用户可以显式选择某个插件作为终端回答者，其余插件只能观察或委托。选择结果按提供方版本持久化；已有审批不会因设置变化被另一个回答者接管；选定的插件崩溃、超时或返回非法结果时，按 `unavailable` 拒绝，**不能因为插件回答者不可用而静默切回 Magi 原生审批**，否则会改变用户已经选择的安全策略。（Q2）
- 审批策略沿 DSH 的两档（`ask`、`never`；`never` 直接拒绝，不询问），Magi 现有的 Restricted 与 FullAccess 访问档位对应 DSH 的权限预设。审批与放行的事实始终由 daemon 记录，并标注决定来自哪个提供方。

**失败。**
- **门类链**（审批、pre-execute、守卫）：没有回答者、回答者失败、超时或非法返回，一律拒绝（fail closed）；链里任一环抛错，该工具调用记为错误结果，不执行（沿 DSH 管线归一化）。现有 `ExtensionBus` 已是这个行为：guard 模式的失败、超时、熔断打开都返回阻止（`bus.rs`）。
- **转换类链**（提示装配、模型请求环绕、步骤前环绕）：插件崩溃或超时，按**下游状态**细分，**不能一概“继续”**（Q3）：
  1. 尚未调用下游：跳过该插件，继续用当前候选值；
  2. 已调用下游且已有结果：使用该下游结果，**不重复调用**；
  3. 下游调用已开始但没有可复用结果：把本次调用标记为**结果未确定**，不自动重放，不伪造原生结果。
  
  所有情况都记录结构化告警和插件健康状态。原因：`llm/stream` 可能已经发出真实模型请求，再调用一次原生链会造成重复请求、重复计费或重复副作用。现有 `ExtensionBus` 对非 guard 的错误已有“记录失败并继续”的基础，但未来的模型环绕必须增加“是否已调用 `next`、下游是否已有结果”的调用状态。

**撤权与停用。**（Q5）插件停用、卸载或崩溃后，**立即停止接收新的调用**；正在进行的调用按其合同排空、取消或标记为结果未确定。到下一个决策边界（下一次审批、下一步模型请求、下一次提交）：转换类和可选提供方回落到 Magi 原生实现，并向用户展示回落事实；**硬守卫仍然拒绝；选定的审批终端不可用仍按 `unavailable` 拒绝，不静默更换回答者**。插件可用性变化影响已有会话的后续决策边界，但**不能修改已经提交的历史事实**。

### 3.2 运行时语义（跨进程后怎么工作）

Node 插件宿主原生实现 DSH 合约，由四个部件组成：**服务实现**（`ctx.*` 服务调用 daemon 的正式能力）、**事件桥**、**页面宿主**（客户端槽位与标准套件）、**依赖解析**。DSH 专属语义只在这一层，Rust 只提供正式业务能力。

| 语义 | 承载方式 |
| --- | --- |
| 同步返回 | 宿主持有 daemon 推送的只读快照，同步读取读快照。**快照 revision 单调递增；任何需要当前权威事实的写入或裁决都必须异步重新读取并校验 revision。** 需要同步写语义的调用，按调用类别逐类处理，不承诺一律由快照满足 |
| 流 | 事件桥承载，有界队列，背压，取消随调用租约传播 |
| `next()` 环绕 | 瀑布链在 Node 宿主内运行。daemon 在定义好的点（步骤前、提示装配、模型请求与流、工具执行）调用宿主一次，宿主跑完整条链；链里调用 `next()` 触达原生部分时，经双向调用回到 daemon 执行（例如 `llm/stream` 里调用 `next()` 即由 daemon 发出一次模型请求，调用两次就发两次）。daemon 为整条链设置 deadline，链上任意环失败按 3.1 处理 |
| 对象句柄 | 插件拿到的是句柄：`(类型, id, revision, 实例代次)`，不是内核活对象，也不能依赖跨调用的对象引用。**实例代次**在插件进程重启、插件升级、重新启用后变化，此前签发的句柄全部失效；不能只靠可能重新从 0 开始的 revision 判断 |

**宿主传输原语与对象业务行为要分开**（Q6）：
- **传输原语**：读取属性（来自快照）、订阅事件、调用公开服务。这是宿主与插件之间的通道，不代表对象的某个方法被开放。
- **对象业务行为**：插件对内核对象“做事”的能力，**必须逐项登记，不能因为存在通用调用通道就自动开放**。v1 只开放一项：**命名转换端口“消息投影转换”**，替代“给实例的方法打补丁”（来源：dsh-purge 对会话 `deriveMessages` 的回退过滤）。新增条目必须对应一项通用能力，而不是一个插件。

不在清单内的做法（给对象写属性、替换任意方法、持有对象长期引用并依赖其内部状态）：注册时明确拒绝并说明，计入缺失说明。

### 3.3 能力组合最小合同

**依据：DSH 预设注册表的语义。** DSH 的预设是声明式的：“通过预设选择一个 Agent 的工具、提示段和技能”；预设定义是普通的插件行（`plugins: [...]`），更新或删除一个定义会使旧修订退役，**已有 Agent 继续使用它已绑定的组合**，新会话取用户默认预设。

“插件全局安装启停，会话通过预设组合能力”会影响工具目录、提示构建、事件分发和提供方选择，所以基础合同先于具体接缝确定。最小合同五项：

1. **能力身份**：`<种类>:<插件 id>:<贡献 id>`，跨版本稳定；种类包括工具、技能、提示段、提供方。
2. **提供方选择**：同 3.1；预设是“启用哪些工具、提示段、技能，选定哪些提供方”的一份声明。
3. **组合顺序**（Q4）：按 **daemon 持久化的插件激活序号**形成稳定顺序，**不是 Node 进程某次启动的加载顺序**；同一插件内按声明顺序；`prepend` 只影响同一接缝内的确定性排序。这样重启、在线升级和并发激活不会改变链路顺序。当前不提供用户调序入口，留到后续。
4. **会话绑定**：会话在创建时绑定预设的一个修订，由 daemon 保存为会话事实；该会话的工具目录、提示构建、事件分发都以这份修订为准；会话恢复时使用其绑定的修订。
5. **生效时机，分两类**（Q5）：
   - **组合变更**（编辑预设、改默认预设）：只影响新会话，已有会话保留已绑定的修订，沿 DSH；
   - **可用性变化**（插件停用、卸载、崩溃）：影响已有会话的后续决策边界，规则见 3.1“撤权与停用”：立即停止新调用，到下一个决策边界回落，硬守卫与选定审批终端不可用时仍拒绝，不改写已提交历史。已绑定的修订引用了不可用的插件时，会话照常使用其余部分，并向用户展示缺失。

预设的编辑界面可以后做，不需要先建设庞大的通用框架。

### 3.4 插件平台结构（取代安装期翻译）

**安装。** 来源是 npm 包（或用户给的包文件）。daemon 拥有安装、启停、升级、卸载的唯一事实；安装只做四件事：解析并锁定依赖（DSH SDK 包由 Magi 随发行提供同一份，原包其余依赖按其 `package.json` 安装）、校验包结构与入口、放置到 Magi 的插件目录、向用户展示安装确认。授权沿 DSH：插件以用户权限完全受信运行，用户在安装时确认。**不做**：静态扫描与兼容结论、受限试跑、合成清单与权限、包装入口、改写客户端代码、按扫描结果拒绝安装。

**贡献全部在运行时注册。** 工具、命令、技能、模型数据源、设置结构、路由、槽位组件、事件订阅，都由插件在 `apply` 中注册，经 daemon 的运行期目录进入产品；停用、撤权、崩溃时随实例回收（已有运行期目录与租约机制沿用）。观察类事件已是按需订阅（第 10 节第 2 项）。

**版本。** Node 插件宿主声明自己实现的 DSH SDK 版本；原包对 DSH SDK 的 `peerDependencies` 不满足时，安装失败并说明需要的版本。不设版本豁免。

**插件宿主进程（决策 12）。** 所有已启用插件运行在同一个 Cordis 树里，插件之间可以互相提供和注入服务；取代现在的一插件一宿主进程。Cordis 负责单个插件的启动失败隔离；进程崩溃时整体重启，进行中的调用按 3.1“撤权与停用”处理。实例代次（3.2）随宿主进程重启变化。**插件配置**由 Magi 插件设置提供（决策 14）。

**删除清单。** 平台重构完成时一并删除，不保留双份：
- 安装期：`scripts/compat/core` 的扫描、规则、报告、能力清单，`scripts/compat/dsh` 的规则表与翻译器，`scripts/compat/install` 的准备、记录器、客户端分析与改写、槽位记录；
- 运行时垫片：按 SDK 2 `api.*` 转接的 `dsh-host.mjs` 结构、`nullableEvents` 协商、DSH profile 目录模拟（`profile*.mjs`、`DSH_HOME` 重定向）；其中仍需要的服务按“完整实现或不提供”在新宿主中重写；
- Rust：插件系统中的 DSH 版本豁免策略（`lifecycle/compatibility.rs`）、翻译运行时状态与 `PluginDshCompatibilityProjection` 等兼容投影、对 `extension/compat.json` 的读取；
- 文档：本文第 5 节的方法改为真实激活回归后，扫描结论不再作为依据。

**验收。** 以真实激活代替静态扫描：第 5 节的样本包在 Magi 中原样安装并激活，记录每个包“激活成功／缺哪个必需服务／运行期错误”。平台重构前后，能真正激活的包数不得下降；之后每补一项能力，以这份真实结果衡量。

### 3.5 已确认的运行时合同

| 编号 | 结论 |
| --- | --- |
| Q1 | 采用。硬规则是 daemon 的最终守卫；对最终参数重新校验；守卫失败、超时、宿主缺失、协议错误均拒绝 |
| Q2 | 采用。唯一审批终端，默认原生；选择按提供方版本持久化；选定回答者不可用按 `unavailable` 拒绝，不静默切回原生；已有审批不被接管 |
| Q3 | 采用，按下游状态细分：未调用则跳过；已有结果则复用且不重复调用；已开始无结果则标记结果未确定，不重放、不伪造；均记录告警与健康状态 |
| Q4 | 采用。按持久化的激活序号排序，插件内按声明顺序，`prepend` 只影响同接缝排序；暂不提供调序入口 |
| Q5 | 采用修订版。立即停止新调用；进行中的调用排空、取消或标记未确定；下一个决策边界转换类与可选提供方回落；硬守卫与选定审批终端仍拒绝；不改写已提交历史；组合变更只影响新会话 |
| Q6 | 采用修订版。快照 revision 单调递增，权威写入与裁决异步重读并校验；句柄带实例代次；区分传输原语与对象业务行为，v1 仅开放“消息投影转换” |

以上六项作为运行时合同的正式版本。

## 4. 处置分类

| 处置 | 含义 |
| --- | --- |
| 已有能力 | Magi 内核已有对应的正式能力；Node 插件宿主按 DSH 合约完整实现该服务即可 |
| 补端口 | Magi 内部有该能力，要新增或放宽对插件的端口，必要时补齐领域事实 |
| 新建 | Magi 没有对应能力，需先设计 |
| 宿主服务 | DSH 的宿主、客户端或 API 外壳服务；对应 Magi 自有的产品服务。要么经 DSH 合约完整提供，要么不提供（依赖它的插件按 DSH 语义降级或启动失败），不做“最小外观” |
| 不提供 | 遥测、账号、实验、调试类；DSH 插件自身以可选注入使用，Magi 不提供，插件按 DSH 语义自行跳过 |

## 5. 真实插件的依赖实测

**方法。** 取 npm 上关键字 `dsh-plugin` 搜索结果前 100 个包（2026-11-17），排除 4 个 PDF 原生二进制子包，其余 95 个用 `npm pack --ignore-scripts` 下载解压（不执行任何包代码），用仓库内 `scripts/compat` 的静态扫描器分析。**该扫描器属于第 3.4 节的删除清单；这里的数字只说明需求分布，验收改为真实激活回归。**

**覆盖。** 95 个中 79 个被识别为 DSH 插件并完成扫描；16 个不是插件（目录、命令行工具、规范文档）或没有可扫描源码。**这是静态扫描，不是业务验收；样本按搜索相关度取得，不代表整个生态。** 按当前翻译器的**后端**准入规则：50 个无阻塞，29 个被阻塞。**只看后端会高估：再加上界面槽位的安装准入，两项都通过的是 43 个（54%），见第 8 节；这仍是安装准入，不是业务验收。**

**后端依赖：使用最多的服务**（使用该服务的插件数，79 个中）：`tools` 34，`webServer` 28，`settings` 14，`commands` 14，`connection` 14，`llm` 9，`skills` 9，`credentials` 9，`agents` 8，`sessions` 6，`pluginManager` 5，`systemPrompt` 4，`storageDomain` 4，`workspaceRegistry` 4。使用事件最多的是 `session/event` 7、`agent/created` 5、`agent/pre-step` 3、`tools/pre-execute` 3；`system-prompt/assemble` 仅 1 个，`llm/stream` 在该样本中为 0。

**29 个被阻塞的原因**（受影响插件数）：`session/event` 逐条事件 6；`ctx.agents` 5；`skills.registerProvider`（含 `dsh-skill` 导入）5；`dsh-session` 导入 4；`agent/pre-step` 3；`agent/created` 3；`storageDomain` 3；`tools.guard` 3；`credentials` 约 3；`sessionQuery` 2；`tools/result`、`pre-execute`、`post-execute` 各 2；`workspaceRegistry` 2。

**结论。**
- 绝大多数插件依赖的是工具、页面路由、设置、命令。Magi 已有这些能力（目前经翻译层接通，平台重构后在原生插件宿主中按合约重写）。
- 阻塞集中在**读取会话与 Agent、会话与 Agent 事件、工具执行管线事件、技能提供方、凭据、存储域**。这些都是“补端口”或“新建”，而且以只读或观察为主。
- **要求替换执行循环或会话存储的插件，在样本里没有出现**（`agentLoop` 0；`sessionPersistence` 有 3 个插件注入它，但不构成阻塞原因，需要核对具体用法）。系统提示整体改写与模型请求拦截在样本中各只有 1 个或 0 个插件（dsh-purge 在样本之外单独分析）。

**两个指定样本。**
- **dsh-rea 1.2.0**：只读取 `tools`（`tools.schemas`），14 项全部已接，无阻塞。它本质是把上游 REA 的 MCP 服务注册为 `mcp__rea__*` 工具。
- **dsh-purge-community 1.7.18**：148 项，7 项已映射、8 项 shim、115 项随包、5 项部分、1 项丢弃、12 项未登记。它需要：`agent/pre-step` 环绕（过滤步骤消息）、`llm/stream` 环绕（拒答检测后重试）、`system-prompt/assemble` 与 `systemPrompt.ctx`、`session/created` 与 `session/event`（读取 `header.cwd`）、`agent-preset/selected`、`agent/inbox/claimed`，以及 `agents.get`、`sessions.all/list`、`connection.trustedHosts`。扫描器把它挂在 `ctx` 上的 8 个私有标记（`ctx.__dshPurge*`）也报成“未登记”，这是扫描器对插件私有属性的误报。它还直接修改会话实例（给 `deriveMessages` 打补丁），按第 2 节只能识别目的，落成“消息投影变换”端口。它的宿主补丁、备份回滚、DSH 自更新等功能不适用。

## 6. 对照表

按 DSH `capability-seams.md` 的 113 个服务逐项列出。最后一列是第 5 节样本（79 个已扫描插件）中使用该服务的插件数，0 表示样本中没有用到。

汇总：已有能力 8，补端口 39，新建 19，宿主服务 26，不提供 21（共 113 项）。

### 一、代理运行时与会话（决定插件能不能改变代理的行为）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.agentLoop` | bundle | 补端口 | Turn 执行器；外部执行器接口（ExecutorCapabilities） | DSH 中循环本身是一个可替换插件。落点是 Magi 已预留的外部执行器接口，见第 9 节 | 0 |
| `ctx.agents` | core | 补端口 | Task / Thread / 角色 | DSH 是活 Agent 句柄与 create/resume 工厂；Magi 以句柄代理加提案表达，不暴露活对象 | 8 |
| `ctx.agentPresets` | core | 新建 | 角色（agent-role）与能力单元 | 按决议：插件全局安装启停，会话能力由预设从全局已启用插件中组合；需先做能力单元注册表 | 1 |
| `ctx.agentDefaultModel` | core | 已有能力 | 主对话默认模型选择 | 默认模型的只读无凭据投影 | 3 |
| `ctx.agentTeams` | core | 新建 | 子代理派发与计划 | 共享任务 DAG 无对应，需设计 | 0 |
| `ctx.subagents` | seam | 补端口 | agent_spawn 与角色绑定 | DSH 有 6 种实现（进程内 spawn/fork 等）；现仅有只能阻止的 guard 钩子 | 1 |
| `ctx.subagentModelSelection` | core | 补端口 | 角色模型绑定 | DSH 默认关闭的设置命名空间；Magi 绑定在角色上 | 0 |
| `ctx.sessions` | core | 补端口 | 会话存储（session-store） | 现仅有调用期 list/get 投影；需完整只读 API 与提案式写入 | 6 |
| `ctx.sessionPersistence` | seam | 补端口 | 会话存储（session-store） | DSH 的合同是存储后端（create/open/stat/list/flush，句柄追加与读取，单写者）。落点见第 9 节 | 3 |
| `ctx.sessionProjections` | core | 补端口 | canonical 投影 | 只读投影接口 | 2 |
| `ctx.sessionProjectionCache` | core | 补端口 | canonical 投影缓存 | 随 sessionProjections | 0 |
| `ctx.sessionQuery` | seam | 补端口 | 会话检索 | seam，后端可替换 | 3 |
| `ctx.sessionReferenceResolver` | core | 补端口 | 会话引用 | 把当前界面的会话快照作为上下文 | 0 |
| `ctx.sessionTitle` | seam | 补端口 | 会话标题生成 | seam，实现可替换 | 1 |
| `ctx.planMode` | core | 新建 | 计划（magi-plan） | 已核对：Magi 的 PlanStore 是用户可见的阶段计划（最多 12 项），没有“计划模式”及其退出流程 | 0 |
| `ctx.goals` | core | 补端口 | 目标（Goal） | Magi 原生拥有；现被明确拒绝，需放开读写 | 1 |
| `ctx.schedule` | core | 补端口 | 插件作业调度 | Magi 现仅调度插件命令；DSH 是定时向目标会话排入消息 | 0 |
| `ctx.userQuestions` | seam | 已有能力 | ask_user_question | seam | 1 |
| `ctx.commands` | core | 已有能力 | 命令贡献 | 命令注册与执行 | 14 |
| `ctx.skills` | seam | 补端口 | 技能（skill-runtime） | 现仅接入全局文本技能；其他调用策略、资源根、provider、agent 作用域未接 | 9 |
| `ctx.compaction` | seam | 补端口 | 上下文压缩（context-runtime） | seam，后端可替换；Magi 现为单一权威，放开需设计 | 0 |
| `ctx.tokenMeter` | core | 补端口 | 用量权威（usage-authority） | 现只能观察已提交用量 | 0 |
| `ctx.toolResultPruner` | core | 补端口 | 工具结果投影 | 对应 tool.result 的 advise | 0 |
| `ctx.systemPrompt` | core | 补端口 | 提示装配 | 现只能追加；按决议开放整体改写，并与缓存谱系对接 | 4 |
| `ctx.tools` | core | 补端口 | 工具注册与执行 | 注册已有；pre-execute 的 allow/ask、execute 环绕、restrict 需补 | 34 |
| `ctx.approval` | seam | 补端口 | 审批与安全规则（safety-gate） | 按决议完全放行：插件可决定审批，审批事实仍由 daemon 记录来源 | 0 |
| `ctx.permissionPresets` | core | 补端口 | 访问档位（Restricted / FullAccess） | 对应 workspace-write / danger-full-access 预设 | 0 |
| `ctx.llm` | seam | 已有能力 | 模型数据源（modelSources）与模型调用 | 数据源注册与模型调用均已有；`stream`、`prepareCall` 按合约完整实现 | 9 |
| `ctx.deepseekLlmApiExtensions` | seam | 补端口 | 模型请求拦截 | 请求体扩展需要模型请求前的拦截端口 | 0 |
| `ctx.sessionLogDeepSeek` | core | 不提供 | — | DeepSeek 会话日志上传策略，绑定其账号体系 | 0 |

### 二、执行环境（工具背后的可替换后端）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.fs` | seam | 补端口 | 文件工具 | seam，local/sandbox/ssh 后端可替换 | 2 |
| `ctx.shell` | seam | 补端口 | shell_exec | seam | 0 |
| `ctx.shellEnv` | core | 补端口 | shell 环境变量事实 | 插件声明 DSH_* 环境事实 | 0 |
| `ctx.subprocess` | seam | 补端口 | 进程管理（magi-process） | seam | 1 |
| `ctx.terminals` | seam | 补端口 | 用户终端（PTY，terminal_runtime）与进程工具 | 已核对：Magi 有会话作用域的 PTY 终端；seam 的后端提供方未向插件开放 | 0 |
| `ctx.sandbox` | seam | 补端口 | 隔离与安全规则 | seam | 0 |
| `ctx.sandboxPolicy` | core | 补端口 | 隔离策略 | 随 sandbox | 0 |
| `ctx.jobs` | seam | 补端口 | 后台进程与作业 | seam | 0 |
| `ctx.ptcRuntime` | seam | 新建 | — | PTC（代码执行式工具调用）模式，Magi 无 | 0 |
| `ctx.ssh` | core | 新建 | — | Magi 无 SSH 后端 | 0 |
| `ctx.lsp` | seam | 新建 | — | Magi 无 LSP 工具 | 0 |
| `ctx.web` | seam | 已有能力 | web_search / web_fetch | 联网提供方选择 | 0 |
| `ctx.spillStore` | seam | 新建 | 工具结果预算与历史压缩 | 已核对：Magi 用预算与历史压缩处理超长输出，没有“落盘并返回引用”的机制 | 0 |
| `ctx.workingDirectory` | seam | 补端口 | 会话工作目录 | 现仅有调用期 cwd 投影 | 1 |
| `ctx.worktrees` | seam | 补端口 | Git 工作树（magi-git）与任务租约 | seam | 0 |
| `ctx.attachments` | seam | 补端口 | 私有图片服务 | 图片读写已有；文件附件等其余语义补齐后才算完整 | 3 |
| `ctx.fileReferences` | seam | 补端口 | 上下文文件引用 | seam | 0 |
| `ctx.workflowEngine` | seam | 新建 | — | DSH：模型编写的 JS 脚本编排并发子代理。Magi 有子代理派发与计划，没有脚本编排引擎；本样本中 0 个插件使用 | 0 |

### 三、外部能力（供应方可替换）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.mcpResources` | seam | 已有能力 | MCP 客户端 | 上游 MCP 客户端作为普通依赖运行，工具调用经 Magi 工具入口 | 0 |
| `ctx.mcpManager` | core | 补端口 | MCP 服务器配置与状态 | Magi 的 MCP 管理在设置中，未向插件开放 | 0 |
| `ctx.browserUse` | seam | 补端口 | 内置浏览器（browser-authority） | DSH 是 provider 注册；Magi 浏览器由权威状态拥有 | 0 |
| `ctx.computerUse` | seam | 新建 | — | Magi 无 computer use | 0 |
| `ctx.cuaAccess` | core | 新建 | — | 随 computerUse | 0 |
| `ctx.cuaPrograms` | core | 新建 | — | 随 computerUse | 0 |
| `ctx.officeKit` | core | 新建 | — | Office 渲染、转换与重算 | 0 |
| `ctx.speechToText` | seam | 新建 | — | 语音转写 | 1 |
| `ctx.speechController` | core | 新建 | — | 随 speechToText | 0 |
| `ctx.forge` | seam | 新建 | — | 代码托管变更请求；Magi 有 Git 能力和 Super-agent 设计稿，尚无对应服务 | 0 |
| `ctx.forgeSession` | service | 新建 | — | 随 forge | 0 |
| `ctx.forgePanel` | service | 宿主服务 | — | 随 forge 的界面远程服务 | 0 |
| `ctx.webhookRuntime` | core | 新建 | — | 外部事件投递 | 0 |

### 四、凭据、设置与存储

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.credentials` | seam | 新建 | — | DSH 凭据引用 seam；建成前不提供，必需它的插件启动失败并说明 | 9 |
| `ctx.authorization` | seam | 新建 | — | 凭据获取流程注册，随 credentials | 0 |
| `ctx.deepseekAccount` | service | 不提供 | — | 绑定 DeepSeek 账号体系 | 0 |
| `ctx.settings` | core | 已有能力 | 插件设置（CAS） | 设置读取与 CAS 写入 | 14 |
| `ctx.configEditor` | core | 已有能力 | 插件设置写入 | 随 settings | 0 |
| `ctx.storage` | seam | 补端口 | 插件资源存储 | Magi 有带版本的资源；DSH 是通用后端 seam（JSON / SQLite） | 0 |
| `ctx.storageDomain` | core | 补端口 | 插件资源存储 | 随 storage | 4 |
| `ctx.workspaceRegistry` | core | 补端口 | 工作区（magi-workspace） | 只读投影 | 4 |

### 五、宿主、客户端与 API 控制器（Magi 自有产品服务：经 DSH 合约完整提供，或不提供）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.connection` | core | 宿主服务 | daemon 已认证连接 | 按合约完整提供 | 14 |
| `ctx.connection.authentication` | seam | 宿主服务 | — | 宿主认证，Magi 自有 | 0 |
| `ctx.webServer` | core | 宿主服务 | 插件私有路由 | 路由经 daemon 转交；其余成员逐项核对后完整提供 | 28 |
| `ctx.webApp` | core | 宿主服务 | — | DSH Web GUI 地址 | 0 |
| `ctx.clientModules` | core | 宿主服务 | 视图加载 | 插件客户端模块打包与服务 | 0 |
| `ctx.typert` | core | 宿主服务 | 插件私有 Remote 路由 | JSON unary 已有；流与 Host 对象查找补齐后才算完整 | 1 |
| `ctx.typertGateway` | core | 宿主服务 | 同上 | 随 typert | 0 |
| `ctx.remoteAccess` | core | 宿主服务 | — | 远程访问，Magi 自有 | 0 |
| `ctx.hmr` | core | 不提供 | — | 操作 DSH profile 文件与模块热替换；不提供（决策 13） | 1 |
| `ctx.pluginManager` | core | 宿主服务 | 插件生命周期 | 在 Magi 插件生命周期上完整实现；安装、启停、更新、卸载经用户确认 | 5 |
| `ctx.pluginMarket` | core | 宿主服务 | 插件中心 | Magi 自有 | 0 |
| `ctx.pluginRegistryProbe` | core | 宿主服务 | — | DSH 插件管理界面使用 | 0 |
| `ctx.profileContext` | core | 不提供 | — | DSH profile 身份；不提供（决策 13） | 4 |
| `ctx.headlessStartup` | core | 不提供 | — | DSH 无头运行选项 | 0 |
| `ctx.sessionController` | core | 宿主服务 | Magi API | 会话命令与冷读，Magi 自有 API | 2 |
| `ctx.sessionStatus` | core | 宿主服务 | Magi 界面 | 已读与未读标记 | 0 |
| `ctx.sessionFileReferences` | core | 宿主服务 | Magi API | 文件引用的远程服务 | 0 |
| `ctx.sessionSkillCatalog` | core | 宿主服务 | Magi API | 技能目录的远程服务 | 0 |
| `ctx.jobController` | core | 宿主服务 | Magi API | 后台作业观察流 | 0 |
| `ctx.credentialsController` | core | 宿主服务 | — | 随 credentials | 0 |
| `ctx.settingsController` | core | 宿主服务 | Magi 设置 API | 设置的远程服务 | 0 |
| `ctx.workspaceFiles` | core | 宿主服务 | Magi API | 工作区文件读取与变更流 | 0 |
| `ctx.workspaceChanges` | core | 宿主服务 | Magi 变更视图 | 工作区变更摘要 | 0 |
| `ctx.terminalController` | core | 宿主服务 | Magi 终端面板 | 用户终端的远程服务，Magi 自有 | 0 |
| `ctx.workspaceController` | core | 宿主服务 | Magi API | 工作区命令 | 0 |
| `ctx.directoryPicker` | seam | 宿主服务 | 桌面目录选择 | seam，Magi 由 Electron 宿主承担 | 0 |
| `ctx.directoryPickerController` | core | 宿主服务 | 桌面目录选择 | 随 directoryPicker | 0 |
| `ctx.fileUploads` | core | 宿主服务 | — | 客户端文件上传 | 0 |

### 六、遥测、反馈与实验服务（Magi 不提供；DSH 插件以可选注入使用，自行跳过）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.otel` | service | 不提供 | — | 遥测 | 0 |
| `ctx.productAnalytics` | service | 不提供 | — | 产品统计 | 0 |
| `ctx.productTelemetry` | service | 不提供 | — | 产品遥测 | 0 |
| `ctx.sessionTelemetry` | seam | 不提供 | — | 会话遥测 seam | 0 |
| `ctx.messageFeedback` | core | 不提供 | — | 消息反馈，写入 DSH canonical 日志 | 0 |
| `ctx.sessionFeedback` | core | 不提供 | — | 会话级反馈 | 0 |
| `ctx.actionCards` | service | 不提供 | — | 实验：行动卡片 | 0 |
| `ctx.pluginRecommendations` | service | 不提供 | — | 实验：插件推荐 | 0 |
| `ctx.deepseekChatImport` | service | 不提供 | — | 导入 DeepSeek 聊天导出 | 0 |
| `ctx.cotTranslation` | service | 不提供 | — | 实验：推理翻译 | 0 |
| `ctx.translator` | service | 不提供 | — | 实验：文本翻译 | 0 |
| `ctx.liveshareShares` | service | 不提供 | — | 实验：实时共享 | 0 |
| `ctx.claudeCodeMods` | core | 不提供 | — | 实验：Claude Code 模组 | 0 |
| `ctx.inspector` | core | 不提供 | — | DSH 调试观察 | 0 |
| `ctx.dynamicCordisRunner` | core | 不提供 | — | DSH 开发调试 | 0 |
| `ctx.cordisInspect` | core | 不提供 | — | DSH 开发调试 | 0 |

## 7. 公共能力分组

第 6 节的 113 行是 DSH 的服务清单，不是 Magi 要新增的 113 个服务。Magi 按真实任务形成少数几项公共能力；Node 插件宿主按 DSH 合约实现这些服务，背后调用这些能力：

| 公共能力 | 涵盖的 DSH 接缝 | 依据 |
| --- | --- | --- |
| 会话读取与事件 | `sessions`、`agents`（句柄）、`sessionProjections`、`sessionQuery`、`workspaceRegistry`、`session/event`、`agent/created`、`goals` 读取、`planMode` | 样本被阻塞的首要原因 |
| 工具管线 | `tools` 的 pre-execute、post-execute、result、guard、execute 环绕、restrict | 样本被阻塞第二类 |
| 提供方端口 | `approval`、`permissionPresets`、`sandbox`、`llm` 请求环绕、`systemPrompt` 装配、`compaction`、`tokenMeter`、`toolResultPruner`、`fs`/`shell`/`subprocess`/`jobs`/`terminals` 后端、`web`、`browserUse`、`mcpManager` | dsh-purge 与后续接缝；遵守 3.1 |
| 凭据与持久业务数据 | `credentials`、`authorization`、`storage`、`storageDomain`、`spillStore` | 样本阻塞第三类 |
| 调度与后台任务 | `schedule`、`jobs`、`goals` 写入、`subagents`、`agentTeams` | 为事件驱动与后台工作预留 |
| 能力组合 | `agentPresets`、`skills` 提供方、`commands`、预设绑定 | 合同见 3.3 |
| 可替换后端 | `agentLoop`、`sessionPersistence` | 第 9 节 |
| 按需新建 | `computerUse`、`officeKit`、`speechToText`、`forge`、`webhookRuntime`、`ssh`、`lsp`、`ptcRuntime`、`workflowEngine` | 样本使用极少，出现真实需求再排期 |

项目级 Super-agent 设计（`docs/magi-project-super-agent-and-upstream-maintenance-design.md`）计划让插件参与持续观察外部事件、主动工作和交付跟进，这支持“会话事件、凭据、持久业务数据、调度、执行”这几项能力的方向；但它是规划，不是已交付，也不是本表的需求来源，需求来源是第 5 节的真实插件。


## 8. 界面槽位对齐

DSH 的界面是**槽位系统**：宿主和各界面包声明槽位，插件的客户端部分把组件注册进去。DSH 发行的网页客户端共声明 **122 个槽位**（来自其 `slot-catalog` 生成清单）：对话区 47、侧栏 29、设置 14、插件管理 9、代码托管 5、外壳 4、工具 3、其余 11。

### 8.1 范围（决策 7）

**范围内：设置页、主页面、右栏标签页。** 范围外的槽位（主对话的输入区、对话区、工具调用视图、会话头部，左栏，外壳浮层，以及它们的替换类）暂不支持，先对齐核心接缝，后续有条件再评估。

**Magi 的设置页就是槽位的承载位置。** 每个插件在设置里有独立页面：左侧导航里一个入口，页面顶部是 Magi 原生页头（图标、名称、版本、启用开关、卸载），正文由插件注册的组件渲染，例如 WorkBuddy 的账号池、更多设置、活动记录都是它自己的组件。DSH 的 `settings.section`、`plugins.bundle.config`、`settings.plugins.tab` 与全部 `plugins.*` 槽位都渲染在这里，沿现状，不新增位置。

### 8.2 Magi 现状

槽位表（`scripts/compat/runtime/client/slot-context.mjs`，核对的是 DSH 0.2.2-rc.1）登记了 18 个：
- **6 个挂载**：主页面 `main`、设置 `settings.section`、`settings.plugins.tab`、`plugins.tab`、`plugins.bundle.config`、右栏 `sidebar.right.pane.tab`。
- **5 个别名（现状：组件被接收，但不按合约渲染，由别的槽位或清单数据代替）**：`settings.section.icon`、`plugins.row.config`（与 `plugins.bundle.config` 是同一个配置组件）、`sidebar.panellist`、`sidebar.right.pane.tab.title`、`sidebar.right.tab.guide.entry`（右栏标签的名称与导航）。
- **3 个接收但内容不显示（现状：静默丢弃，没有告诉用户）**：`sidebar.footer.action`、`conversation.input.left`、`conversation.input.right`。
- **2 个拒绝安装**：`conversation.chat.node`、`conversation.input.dock`。
- **2 个明确未接入**：`conversation.session.header.actions`、`conversation.chat.turnTail`。其余槽位没有登记。

**关键事实：安装时只要插件注册了既不挂载、也不是别名或忽略的槽位，安装就会直接失败**（提示“该插件需要尚未接入的界面位置”）。DSH 当前 master 的槽位与 rc.1 有出入，下表以 master 为准。

**目标规则（取代别名与静默丢弃）**：范围内的槽位，插件注册的每个组件都按 DSH 合约在对应位置渲染；同一个组件注册到两个范围内槽位时，渲染位置相同的只渲染一次，不同组件各自渲染。范围外的槽位按 8.4 接收、不渲染，并在安装预览与详情里明示。不再有“别名”“接收但静默不显示”。

### 8.3 实测（样本 95 个包，对注册调用做静态匹配）

- 45 个包注册了界面槽位。其中 25 个全部落在“挂载、别名或忽略”里，**20 个（44%）至少注册了一个会让安装失败的槽位**。另有 6 个因使用“接收但不显示”的槽位，能安装但丢失对应内容。
- 导致安装失败的槽位（插件数）：`shell.overlay` 6，`plugins.item` 5，`conversation.input.dock` 4，`tool.call.toolview` 4，`conversation.view` 3，`conversation.chat.node` 2，`plugins.detail.section` 2，其余各 1。
- 按位置归类（受影响插件数，可重叠）：插件管理页 8、对话区 7、输入区 6、外壳浮层 6、工具视图 4、设置页 1。
- **其中属于范围内的只有插件管理页（`plugins.*`）和设置页**：登记并渲染后，样本里有 5 个只因这类槽位被拒的插件即可通过。其余都在范围外。
- 合并后端：79 个已扫描插件中，后端无阻塞 50 个；这 50 个里 7 个会被范围外的槽位拒绝，其中 3 个只是因为 `plugins.*`。按当前规则，两项都通过的是 43 个（54%）；若把 `plugins.*` 补上，是 46 个。这是安装准入，不是业务验收。

### 8.4 范围外槽位与无页面插件的规则（已定：允许安装并明示缺失）

安装、运行和动态注册**采用同一条规则**：范围外槽位的注册被接收但不渲染，插件照常安装与运行。**安装预览与插件详情必须列出“该插件的 N 项界面功能在 Magi 中不可用”及具体槽位**，让用户在安装前就知道缺什么；其余功能（后端、设置页、主页面、右栏）正常。这类插件不算“完整运行”，缺失要如实展示。

**没有可显示的页面，也不能因此否定仍然可用的工具或模型业务。** 只提供工具、模型数据源、命令或后台能力的插件，没有任何挂载槽位时照常安装运行，只是没有设置页入口。

这改变了现有行为：当前 `client-assets.mjs` 对未登记槽位直接抛错，对“没有任何挂载槽位”也抛错（“不交付空白设置页”）。`client-assets.mjs` 属于第 3.4 节的删除清单；槽位在运行期注册时判定范围，依赖旧行为的验收与文案同步更新。

按样本，这样后端无阻塞的 50 个插件都能通过界面准入；其中受影响的 7 个会带着“界面功能缺失”的说明安装。这仍是安装准入，不是业务验收。

### 8.5 范围内槽位的组件合同

同一前缀的槽位并不都是“把正文渲染进页面”：它们收到的参数、需要的交互和对应的 Magi 承载方式各不相同（配置正文要 `form`，详情动作要 `subject`，设置关闭按钮属于 DSH 设置外壳，整块右栏要 `width`）。下表按槽位逐项确定，数据取自 DSH 的 `slot-catalog`：“宿主提供的参数”是 owner 传给组件的属性，“标准套件”是组件可调用的 hook 个数（根作用域 10 个，会话作用域 19 个，见 8.6）。

“范围”列：**已挂载**＝现有；**待登记**＝范围内，尚未登记，需要按合约渲染；**范围外**＝DSH 的外壳或会话区，Magi 自有，不渲染，计入缺失说明；**按需**＝范围内但样本中没有插件使用，出现需求再接入，接入前按范围外明示。

| 槽位 | 内容性质（宿主提供的参数） | 标准套件 | Magi 承载方式 | 范围 | 样本 |
| --- | --- | --- | --- | --- | --- |
| `settings.section` | 设置页正文；owner 提供 close() | 10 | 该插件的设置页正文 | 已挂载 | 12 |
| `settings.section.icon` | 设置导航图标（size、active） | 10 | 渲染为该插件设置导航入口的图标 | 待登记 | 1 |
| `settings.plugins.tab` | 插件分类里的标签页内容（children） | 10 | 并入该插件的设置页 | 已挂载 | 5 |
| `settings.general.item` | 常规设置条目（children） | 10 | Magi 偏好页自有 | 范围外 | 0 |
| `settings.header` | 设置外壳页头（children） | 10 | Magi 外壳自有 | 范围外 | 0 |
| `settings.close` | 设置外壳关闭按钮（children） | 10 | Magi 外壳自有 | 范围外 | 0 |
| `settings.trigger` | 设置入口按钮（wide） | 10 | Magi 外壳自有 | 范围外 | 0 |
| `settings.launcher` | 设置入口区（openSettings、openOnboarding 等） | 10 | Magi 外壳自有 | 范围外 | 0 |
| `settings.launcher.notifications` | 设置入口通知点（active、openAccount） | 10 | Magi 外壳自有 | 范围外 | 0 |
| `settings.action` | 设置外壳动作（children） | 10 | Magi 外壳自有 | 范围外 | 0 |
| `settings.onboarding` | DSH 新手引导步骤（stepId、complete、openSection） | 10 | Magi 无此流程 | 范围外 | 0 |
| `settings.models.footer` | DSH 模型设置页脚 | 10 | Magi 模型页自有；插件模型走 modelSources | 范围外 | 0 |
| `settings.models.provider-card` | DSH 模型提供商卡片（provider、configured） | 10 | 同上 | 范围外 | 1 |
| `settings.models.sign-in` | DSH 模型登录 | 10 | 同上 | 范围外 | 0 |
| `plugins.bundle.config` | 插件配置正文（view、form） | 10 | 该插件设置页正文；form 由 Magi 的设置 CAS 提供 | 已挂载 | 5 |
| `plugins.row.config` | 插件行配置（view、form） | 10 | 该插件设置页正文；与 bundle.config 是同一组件时只渲染一次 | 待登记 | 7 |
| `plugins.tab` | 插件管理标签页（install、update、openPackage） | 10 | 该插件的设置页；install、update 经 Magi 插件操作桥 | 已挂载 | 3 |
| `plugins.item` | 插件行内容（view、form） | 10 | 设置页正文的摘要区 | 待登记 | 5 |
| `plugins.detail.section` | 详情分区（subject） | 10 | 设置页正文的附加分区；subject 为插件身份与状态 | 待登记 | 2 |
| `plugins.detail.badge` | 详情徽标（subject） | 10 | 设置页正文顶部的徽标行 | 待登记 | 1 |
| `plugins.detail.actions` | 详情动作（subject） | 10 | 设置页正文顶部的动作行；页头的启用与卸载仍是 Magi 原生 | 待登记 | 1 |
| `plugins.add.actions` | DSH“添加插件”对话框动作（onDismiss） | 10 | Magi 安装入口自有 | 范围外 | 0 |
| `plugins.bundle.activation` | DSH 包激活对话（packageName、onOpenDetails） | 10 | Magi 安装即启用 | 范围外 | 0 |
| `main` | 主页面（key） | 10 | 主页面 | 已挂载 | 4 |
| `main.conversation` | 主区会话（会话作用域） | 17 | 属于会话区 | 范围外 | 0 |
| `rightbar` | 整块右栏（width、viewportWidth、canShow） | 10 | Magi 右栏自有，不支持整块替换，只支持标签页 | 范围外 | 0 |
| `rightbar.session` | 整块会话右栏（同上） | 19 | 同上 | 范围外 | 0 |
| `sidebar.right.pane.tab` | 右栏标签页正文（key；会话作用域） | 19 | 右栏标签页；需要 8.6 的标准套件 | 已挂载 | 5 |
| `sidebar.right.pane.tab.title` | 标签标题 | 19 | 渲染为该右栏标签的标题 | 待登记 | 4 |
| `sidebar.right.tab.guide` | DSH 右栏引导页正文 | 19 | Magi 右栏没有引导页 | 范围外 | 0 |
| `sidebar.right.tab.guide.entry` | 引导页卡片（entryId、title） | 19 | 同上 | 范围外 | 2 |
| `sidebar.right.tab.guide.actions` | 引导页更多菜单 | 10 | 同上 | 范围外 | 0 |
| `sidebar.right.tab.document` | DSH 文档预览正文（content、scrollportRef 等） | 19 | 按 Magi 右栏文件预览逐个对应 | 按需 | 0 |
| `sidebar.right.tab.document.action` | 文档预览工具栏控件 | 19 | 同上 | 按需 | 0 |
| `sidebar.right.tab.document.actions` | 文档预览头部操作 | 19 | 同上 | 按需 | 0 |
| `sidebar.right.tab.document.office.pdf` | Office 转换后的 PDF 呈现 | 19 | 同上 | 按需 | 0 |
| `sidebar.right.tab.document.unpreviewable` | 无法预览时的补充操作 | 19 | 同上 | 按需 | 0 |
| `sidebar.right.tab.files.actions` | 文件页目录操作 | 19 | 按 Magi 文件树逐个对应 | 按需 | 0 |
| `sidebar.right.tab.menu.item` | 标签操作菜单项（tab、dismiss） | 19 | 由 Magi 原生菜单渲染数据项 | 按需 | 0 |
| `deliverables.file.actions` | 文件操作（会话事件坐标） | 19 | 按需 | 按需 | 0 |
| `deliverables.review.file.actions` | 变更文件评审页的文件操作 | 19 | 按需 | 按需 | 0 |

### 8.6 标准套件（组件可调用的 hook）

DSH 在槽位组件里提供一套标准 hook：根作用域（设置、主页面、插件管理）10 个，会话作用域（右栏标签页）在此基础上再加 9 个。页面宿主要为组件提供它们才能运行。**标准套件是槽位合约的一部分：某个作用域（根或会话）的槽位，只有在该作用域的套件完整提供后才算交付，不做只实现一部分就上线的作用域。** 实现顺序按样本用量；依赖 Magi 尚无能力的 hook，按第 4 节先补能力。

| hook | 作用域 | 样本使用（95 个包） | Magi 提供方式 |
| --- | --- | --- | --- |
| `useSession` | 会话 | 6 | 当前会话快照（依赖第 7 节“会话读取与事件”） |
| `useSessions` | 根 | 5 | 会话目录投影（已有会话目录读取能力） |
| `useProjection` | 会话 | 4 | 会话投影（依赖“会话读取与事件”） |
| `inputActions` | 会话 | 4 | 现有输入桥（`composer:text` 授权，已有） |
| `useChat`、`useInput` | 会话 | 各 2 | 依赖“会话读取与事件”，按需 |
| `useWorkspaces` | 根 | 1 | 工作区投影（依赖 `workspaceRegistry`） |
| `useSessionStatus` | 根 | 1 | 会话状态投影 |
| 其余 11 个：`useRemoteAccess`、`useResource`、`useContextMenu`、`usePanelInfo`、`useSessionRetainInfo`、`useSessionAccess`、`useWorkspaceAccess`、`useConversation`、`useAccess`、`useTrajectory`，以及 `sessionId` 属性 | — | 0（`sessionId` 是属性，随会话提供） | 随所属作用域完整实现；依赖尚无能力的先补能力 |

统计是对 hook 名称做静态匹配，可能把同名的普通变量算进去，只作优先级参考。

### 8.7 槽位清单

122 个槽位逐项列出。“Magi 现状”是平台重构前的行为：挂载 = 已渲染；别名 = 组件被接收但不按合约渲染；接收不显示 = 能安装但内容丢失；拒绝 = 会拒绝安装；空 = 未登记，安装会失败。“范围”是目标：待登记 = 按合约渲染；暂不支持 = 范围外，按 8.4 明示缺失。“样本”为 95 个包中注册该槽位的包数。

| 槽位 | 基数 | 作用域 | Magi 现状 | 范围 | 样本 |
| --- | --- | --- | --- | --- | --- |
| `contextMenu.content` | keyed | root |  | 暂不支持 | 0 |
| `contextMenu.icon` | keyed | root |  | 暂不支持 | 0 |
| `conversation.approval.detail` | single | session |  | 暂不支持 | 0 |
| `conversation.chat.assistant-actions` | list | session |  | 暂不支持 | 1 |
| `conversation.chat.commandview` | keyed | session |  | 暂不支持 | 1 |
| `conversation.chat.flow` | single | session |  | 暂不支持 | 0 |
| `conversation.chat.markdown.fence` | hoc | session |  | 暂不支持 | 0 |
| `conversation.chat.message.images` | single | session |  | 暂不支持 | 0 |
| `conversation.chat.node` | keyed | session | 拒绝 | 暂不支持 | 2 |
| `conversation.chat.reasoning.body` | single | session |  | 暂不支持 | 0 |
| `conversation.chat.text-interaction` | keyed | session |  | 暂不支持 | 0 |
| `conversation.chat.turnTail` | list | session |  | 暂不支持 | 1 |
| `conversation.chat.user.before` | chain | session |  | 暂不支持 | 1 |
| `conversation.chat.user.images` | single | session |  | 暂不支持 | 0 |
| `conversation.composer` | chain | session |  | 暂不支持 | 1 |
| `conversation.composer.bar` | single | session-maybe |  | 暂不支持 | 0 |
| `conversation.composer.dock` | list | session |  | 暂不支持 | 1 |
| `conversation.header` | single | session-maybe |  | 暂不支持 | 0 |
| `conversation.header.leading` | single | root |  | 暂不支持 | 0 |
| `conversation.hero.agentPreset` | single | session-maybe |  | 暂不支持 | 0 |
| `conversation.hero.brand.mark` | single | root |  | 暂不支持 | 0 |
| `conversation.hero.frame` | hoc | root |  | 暂不支持 | 0 |
| `conversation.hero.options` | list | session |  | 暂不支持 | 0 |
| `conversation.hero.workspace` | single | root |  | 暂不支持 | 0 |
| `conversation.hero.workspace.directoryFlow` | single | root |  | 暂不支持 | 0 |
| `conversation.input.activity` | single | session |  | 暂不支持 | 0 |
| `conversation.input.attachments` | single | session-maybe |  | 暂不支持 | 0 |
| `conversation.input.dock` | list | session | 拒绝 | 暂不支持 | 4 |
| `conversation.input.left` | list | session | 接收不显示 | 暂不支持 | 2 |
| `conversation.input.model` | single | session |  | 暂不支持 | 0 |
| `conversation.input.model.label` | single | session |  | 暂不支持 | 0 |
| `conversation.input.model.options` | list | session |  | 暂不支持 | 0 |
| `conversation.input.model.pane` | keyed | session |  | 暂不支持 | 0 |
| `conversation.input.overlay` | list | session |  | 暂不支持 | 0 |
| `conversation.input.permission` | single | session |  | 暂不支持 | 0 |
| `conversation.input.plan` | single | session |  | 暂不支持 | 0 |
| `conversation.input.right` | list | session | 接收不显示 | 暂不支持 | 3 |
| `conversation.plan-review.actions` | list | session |  | 暂不支持 | 0 |
| `conversation.session` | single | session |  | 暂不支持 | 0 |
| `conversation.session.header` | single | session |  | 暂不支持 | 1 |
| `conversation.session.header.actions` | list | session |  | 暂不支持 | 0 |
| `conversation.session.header.actions.subagent.footer` | list | session |  | 暂不支持 | 0 |
| `conversation.session.header.corner` | single | session |  | 暂不支持 | 0 |
| `conversation.session.header.lineage` | single | session |  | 暂不支持 | 0 |
| `conversation.session.header.utilities` | list | session |  | 暂不支持 | 1 |
| `conversation.session.header.views.actions` | list | root |  | 暂不支持 | 0 |
| `conversation.trajectory.images` | single | session |  | 暂不支持 | 0 |
| `conversation.user.text.node` | keyed | session |  | 暂不支持 | 0 |
| `conversation.view` | list | session |  | 暂不支持 | 3 |
| `deliverables.file.actions` | list | session |  | 按需 | 0 |
| `deliverables.review.file.actions` | list | session |  | 按需 | 0 |
| `forge.change-request.card.actions` | list | session |  | 暂不支持 | 0 |
| `forge.changes.file.menu` | list | session |  | 暂不支持 | 0 |
| `forge.changes.toolbar` | list | session |  | 暂不支持 | 0 |
| `forge.diff.comment.actions` | list | session |  | 暂不支持 | 0 |
| `forge.diff.line.annotation` | keyed | session |  | 暂不支持 | 0 |
| `main` | keyed | root | 挂载 | 已挂载 | 4 |
| `main.conversation` | single | session-maybe |  | 范围外 | 0 |
| `plugins.add.actions` | list | root |  | 范围外 | 0 |
| `plugins.bundle.activation` | keyed | root |  | 范围外 | 0 |
| `plugins.bundle.config` | keyed | root | 挂载 | 已挂载 | 5 |
| `plugins.detail.actions` | list | root |  | 待登记 | 1 |
| `plugins.detail.badge` | list | root |  | 待登记 | 1 |
| `plugins.detail.section` | list | root |  | 待登记 | 2 |
| `plugins.item` | list | root |  | 待登记 | 5 |
| `plugins.row.config` | keyed | root | 别名 | 待登记 | 7 |
| `plugins.tab` | list | root | 挂载 | 已挂载 | 3 |
| `rightbar` | single | root |  | 范围外 | 0 |
| `rightbar.session` | single | session |  | 范围外 | 0 |
| `root` | single | root |  | 暂不支持 | 0 |
| `settings.action` | list | root |  | 范围外 | 0 |
| `settings.close` | single | root |  | 范围外 | 0 |
| `settings.general.item` | list | root |  | 范围外 | 0 |
| `settings.header` | single | root |  | 范围外 | 0 |
| `settings.launcher` | single | root |  | 范围外 | 0 |
| `settings.launcher.notifications` | single | root |  | 范围外 | 0 |
| `settings.models.footer` | list | root |  | 范围外 | 0 |
| `settings.models.provider-card` | keyed | root |  | 范围外 | 1 |
| `settings.models.sign-in` | single | root |  | 范围外 | 0 |
| `settings.onboarding` | list | root |  | 范围外 | 0 |
| `settings.plugins.tab` | list | root | 挂载 | 已挂载 | 5 |
| `settings.section` | list | root | 挂载 | 已挂载 | 12 |
| `settings.section.icon` | list | root | 别名 | 待登记 | 1 |
| `settings.trigger` | single | root |  | 范围外 | 0 |
| `shell.bottom` | single | root |  | 暂不支持 | 0 |
| `shell.leading` | single | root |  | 暂不支持 | 0 |
| `shell.overlay` | list | root |  | 暂不支持 | 6 |
| `shell.quota-notice` | chain | root |  | 暂不支持 | 0 |
| `sidebar` | single | root |  | 暂不支持 | 0 |
| `sidebar.brand.mark` | single | root |  | 暂不支持 | 0 |
| `sidebar.brand.name` | single | root |  | 暂不支持 | 0 |
| `sidebar.chat.conversation` | single | session |  | 暂不支持 | 0 |
| `sidebar.footer.action` | list | root | 接收不显示 | 暂不支持 | 4 |
| `sidebar.panellist` | list | root | 别名 | 暂不支持 | 6 |
| `sidebar.panellist.actions` | list | root |  | 暂不支持 | 0 |
| `sidebar.right.pane.tab` | keyed | session | 挂载 | 已挂载 | 5 |
| `sidebar.right.pane.tab.title` | keyed | session | 别名 | 待登记 | 4 |
| `sidebar.right.tab.document` | keyed | session |  | 按需 | 0 |
| `sidebar.right.tab.document.action` | keyed | session |  | 按需 | 0 |
| `sidebar.right.tab.document.actions` | list | session |  | 按需 | 0 |
| `sidebar.right.tab.document.office.pdf` | keyed | session |  | 按需 | 0 |
| `sidebar.right.tab.document.unpreviewable` | list | session |  | 按需 | 0 |
| `sidebar.right.tab.files.actions` | list | session |  | 按需 | 0 |
| `sidebar.right.tab.guide` | chain | session |  | 范围外 | 0 |
| `sidebar.right.tab.guide.actions` | list | root |  | 范围外 | 0 |
| `sidebar.right.tab.guide.entry` | keyed | session | 别名 | 暂不支持 | 2 |
| `sidebar.right.tab.menu.item` | list | session |  | 按需 | 0 |
| `sidebar.session.action` | hoc | root |  | 暂不支持 | 0 |
| `sidebar.session.row.hover` | list | root |  | 暂不支持 | 0 |
| `sidebar.session.row.leading` | list | root |  | 暂不支持 | 0 |
| `sidebar.session.row.trailing` | list | root |  | 暂不支持 | 0 |
| `sidebar.settings` | single | root |  | 暂不支持 | 0 |
| `sidebar.toggle.badge` | single | root |  | 暂不支持 | 0 |
| `sidebar.workspaces` | single | root |  | 暂不支持 | 0 |
| `sidebar.workspaces.directoryFlow` | single | root |  | 暂不支持 | 0 |
| `sidebar.workspaces.session.menu.item` | list | root |  | 暂不支持 | 0 |
| `sidebar.workspaces.session.row.action` | list | root |  | 暂不支持 | 0 |
| `tool.call.images` | single | session |  | 暂不支持 | 0 |
| `tool.call.toolview` | keyed | session |  | 暂不支持 | 4 |
| `tool.view.cordis` | keyed | session |  | 暂不支持 | 0 |
| `workspace.picker.directoryFlow` | single | root |  | 暂不支持 | 0 |
| `workspace.toast` | keyed | root |  | 暂不支持 | 0 |

### 8.8 后续评估时的起点（暂不实施）

如果后续要做范围外的位置，插件界面在隔离 iframe 里，组件无法插进宿主的某个按钮旁边，可按四种承载形态分：A 整块（常驻 iframe，高度上报并设上限，用于输入区上下的整行条、对话 View、工具调用视图）；B 小控件条（每插件每位置一个小 iframe，固定行高，放不下进“…”菜单，用于输入区左右、侧栏底部）；C 数据贡献（插件只提供图标、文字、菜单项，由 Magi 原生渲染，用于会话行装饰和菜单行）；D 替换（替换消息渲染、输入框、工作区浏览区，需要只替换内容不替换外框、加载失败回退原生、保留“显示原文”）。样本里使用最多的是 `shell.overlay` 6、`input.dock` 4、`tool.call.toolview` 4、`footer.action` 4。具体设计要按 UX/UI 规范再出方案。


## 9. 执行循环与会话存储后端

两者在 DSH 中都是可替换接缝。架构位置现在确定，实施可以后排；**不用镜像代替真实后端**：镜像只服务备份或导出，如果有这个需求要单独立项，不作为存储接缝的过渡方案，也不算存储接缝完成。

**`ctx.agentLoop` ↔ Magi 外部执行器接口。** DSH 的 agent-loop 负责创建或恢复 Agent、驱动模型请求、流式响应、工具执行与会话历史。Magi 已预留外部执行器接口：执行器声明能力（`ExecutorCapabilities`：输入模态、steer、resume、工具模式、审批模式、取消方式、**上下文归谁**、后台、观察），协议里已有 `magi_owned` 与 `executor_owned`。要完成的是：插件执行器的**注册、选择、运行、取消、恢复**五项，其中选择与失败行为遵守 3.1，运行时的租约、取消、用量与回执入口仍由 daemon 提供。待审计：现有 `executor_owned` 的完成条件、历史重放与审批交互是否足以承载 DSH 循环的语义。

**`ctx.sessionPersistence` ↔ 会话存储后端。** DSH 的合同是**存储后端**：`create`、`open(id,'write'|'read')`、`stat`、`list`、`flush`（持久化屏障），写句柄单写者所有权，追加式连续事件日志，崩溃后返回物理有效的日志、语义修复由读取方负责。目标是**真实支持读写、单写者和持久化屏障**，并由 daemon 定义事务成功条件（例如一个轮次的提交以后端 `flush` 确认为准；失败明确报错，不静默换后端；启动时后端不可用则该后端里的会话不可打开，而不是悄悄改用别处）。**前置工作是审计 `magi-session-store` 的持久化模型能否表达为这份事件日志合同**，审计结果决定这一项的范围，在此之前不承诺具体实现。

样本里没有插件依赖这两项，所以它们排在后面；但接口和合同位置现在就按本节确定，避免之后与 3.1 的提供方合同冲突。

## 10. 推进顺序

先重构插件平台，再逐项补能力。在翻译结构上逐个接缝推进，每一项都会在翻译里再长出补丁，所以平台重构排在所有接缝之前。界面只做决策 7 的范围。

0. **合同先行（已确认）**：第 3 节的提供方合同、运行时语义和对象行为清单，以及能力组合最小合同。已在 3.5 定稿。
1. **插件平台重构**（第 3.4 节）：
   - Node 插件宿主原生运行 DSH 合约，所有已启用插件在同一个 Cordis 宿主进程中（决策 12），配置来自 Magi 插件设置（决策 14）；
   - 安装只做校验与放置，贡献全部在运行时注册；
   - 现有已接通的能力（工具、命令、设置、模型数据源与模型调用、路由、技能、联网、插件生命周期、连接、图片）在新宿主中按合约完整重写；
   - SDK 2 降为内部协议（决策 10），`plugin-authoring.md` 与 `extension-protocol-v1.md` 改写为内部协议文档或移除；
   - 按删除清单删掉翻译层、垫片与 Rust 中的 DSH 兼容代码；
   - 验收：样本包真实安装激活，能激活的包数不低于重构前。
2. **会话读取与事件**：`sessions`、`agents`（句柄，含实例代次）、`session/event`、`session/created`、`agent/created`、`sessionProjections`、`sessionQuery`、`workspaceRegistry`，以及 8.6 中这些服务支撑的标准套件 hook。
   - **现状（2026-11-17）**：daemon 的观察钩子已改为运行期按需订阅，`session.item` 投递完整的已提交条目，`turn.accepted/end` 带 `turnSeq`；这部分是正式能力。曾按旧结构在兼容层写过一个事件桥，其中失败原因、取消原因、工具结果消息 id、`seq`、`stream` 是在宿主里合成的，违反第 2 节，已丢弃；`session/event` 在平台重构后于新宿主中实现，兼容层仍按原状拒绝注册它。
   - **需要先在内核补齐的事实**：会话内单调的事件序号；条目所属的模型步骤，以及步骤开始与结束；轮次结束原因（取消原因、失败的结构化事实）；助手消息的流记录与用量；工具结果的消息身份；子代理会话与主会话的归属关系。
3. **工具管线**：`tools/pre-execute`、`post-execute`、`result`、`tools.guard`，以及 `execute` 环绕。
4. **凭据与持久业务数据**：`credentials` 与 `authorization`、`storage` 与 `storageDomain`，以及技能提供方 `skills.registerProvider`。
5. **提供方端口**（dsh-purge 依赖）：`system-prompt/assemble` 整体改写、`llm/stream` 环绕、`agent/pre-step` 环绕、对象行为清单里的消息投影转换、`approval` 链；均遵守 3.1。
6. **范围内的界面**：按 8.2 的目标规则渲染范围内槽位（含 `plugins.*`、设置导航图标、右栏标签标题），按作用域完整提供 8.6 的标准套件；范围外槽位与无页面插件按 8.4 处理。
7. **能力组合的落地**：预设与会话绑定的实现（合同已在第 0 项定好）。
8. **执行循环与会话存储后端**：按第 9 节，先审计，再做真实后端。
9. **按需新建**：第 7 节最后一行列出的能力，按插件出现的实际需求逐个排期。

## 11. 与项目指令的关系，及未验证的边界

**已定事项**：原列为待定的宿主进程形态、profile 相关服务、插件配置归属，已按建议确定为决策 12–14（2026-11-17）。

**与项目指令的关系：**
- 项目根目录 `AGENTS.md` 的“功能开发”一节仍指向旧设计文档的两个锚点（翻译功能的产品合同、每轮推进与完成门槛）。本文替代它们，但项目指令文件需要用户确认后再改，尚未改动。
- 对既有不变量的影响：按决策 1，DSH 放行的事项不再保留“扩展只能阻止、不能批准”“提示只能追加”这类限制，由第 3.1 节的提供方合同取代；以下仍沿用：插件不直接写 canonical 事实、一个事实一个写入口、调用与实例可失效、超时与队列有界、撤权优先、未支持的合约明确拒绝。具体字句由后续接口章节定稿。
- 第 3 节的运行时合同已确认（3.5）；与 DSH 语义有出入处以 DSH 为准，每个接缝落地时核对。

**未验证：**
- 本表未逐项审计 Magi 源码（已核对 4 项：`planMode`、`terminals`、`spillStore`、`terminalController`）。
- 样本是搜索结果前 100 个，静态扫描，不代表生态整体，也没有任何业务验收；扫描器的“未登记”有一部分只是规则表没有收录。
- hook 与槽位的使用数是静态匹配，可能漏掉动态拼出的名字，也可能把同名变量算进去。
- 平台重构的工作量与样本的真实激活结果尚未测量。

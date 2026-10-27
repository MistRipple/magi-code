# 模型推理能力与选择交互

推理能力由 daemon 解析；Web 只消费 `supportedEfforts`、`defaultEffort`、`transport` 和 `source`。运行时、连接测试、用量身份与模型选择器使用同一解析结果。

能力优先级为服务商模型目录显式声明、已知模型内置能力。未知模型返回空档位和 `source: unknown`，请求不携带强度参数。`/models` 通常只返回模型名称，不能据此声称所有服务商都支持自动发现档位；布尔 `reasoning: true` 也不能推导具体档位。

服务商目录支持 `reasoningCapabilities` 对象或模型条目上的 `supportedEfforts` / `supportedReasoningEfforts` / `supported_reasoning_efforts` 数组及对应默认值。同时支持 Codex 目录的 `models[].slug`、`supported_reasoning_levels[].effort` 和 `default_reasoning_level`。数组元素可以是字符串或包含 `reasoningEffort` / `reasoning_effort` / `effort` 的对象。无效声明不作为能力事实。普通模型列表缺少显式能力时，daemon 在同一 `/models` 地址附带自身 `client_version` 请求补充元数据；可选查询失败仍保留普通列表。精确模型 id 优先，代理别名只按同一服务明确提供的相同 `display_name` 关联能力；重名且能力冲突时拒绝关联，不解码别名，不修改实际请求的模型 id。缓存未覆盖的模型在读取档位时自动加载目录；UI 不让旧“未知”投影压过新读取结果。能力缓存以基础地址、协议和凭据摘要分区，保存于 daemon 设置；刷新目录替换该连接的目录声明，不跨连接或账号复用。不提供用户侧兼容性规则编辑；启动时清理历史 `modelReasoningOverrides`，写入入口拒绝废弃字段。

内置规则只覆盖已知名称及日期快照，不把任意带后缀的代理别名或未来版本当作已知模型。OpenAI 规则参考 [GPT-5.6](https://developers.openai.com/api/docs/models/gpt-5.6)、[GPT-6](https://developers.openai.com/api/docs/guides/latest-model)、[GPT-5.2](https://developers.openai.com/api/docs/models/gpt-5.2)。同名模型经网关转发时可能存在差异，服务商声明优先。

支持的强度枚举为 `none`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`、`ultra`；枚举全集不表示每个模型都支持全部档位。JSON `null` 表示使用模型默认，保存后仍保留 `null`，执行时才按能力解析默认档位；字符串 `none` 表示明确关闭推理，二者不可混同。新会话默认使用 `null`，已有明确四档配置继续保留。不属于目标模型能力的旧值在保存时恢复 `null` 并说明原因。尚无能力声明的历史值保留以兼容配置，但运行时不会发送它。

OpenAI Chat 写入 `reasoning_effort`，Responses 写入 `reasoning.effort`。Anthropic 按能力选择 `output_config.effort` 或旧式 `thinking.budget_tokens`；旧式预算档位是 Magi 的预算预设。没有可调能力时不生成推理参数。GPT Web 的模型与推理由网页管理，本地选择器只展示该事实。

交互入口首先展示当前模型和强度，离散滑块只包含该模型支持的档位；点击模型名进入可搜索的模型列表，选择后回到强度面板。面板只保留当前强度、模型切换和可用档位，不展示恢复默认按钮或常驻用量说明；未显式选档时仍沿用服务端模型默认值。未知、不支持调整及单一档位不显示无意义滑块。切换模型保留仍合法的当前档位，否则使用目标模型默认。草稿刷新目录不覆盖已选择的模型与强度；已有会话沿原设置入口持久化。

设置页的辅助模型、识图模型和独立引擎共用动态档位控件；选项来自 daemon，包含“模型默认”。读取中、读取失败（可重试）、能力未知和明确不支持分开表达。强度进入草稿变更检测、保存、连接测试和重载，`null` 与 `medium` 必须区分。修改模型或连接后重新解析能力，旧档位非法时恢复“模型默认”。主模型设置只管理连接，具体模型与强度归会话；图片生成配置不展示推理档位。子代理引擎只需配置一个默认推理强度；辅助/识图模型仍沿用各自默认行为。

`agent_spawn.reasoning_effort` 是可选的按次选择，主代理从 daemon 注入的角色模型能力中选择实际档位，不额外调用模型判断档位，也不自动按轮升级。角色显式绑定引擎时使用绑定模型，未绑定时继承父任务当前模型。省略或 `null` 使用目标配置默认强度；继承模型以父任务当前强度为默认。`none` 必须属于目标模型的合法档位。显式非法值、未知能力或不支持调整的模型选择强度均在创建子任务前拒绝，不能静默换档或换模型。未知能力的默认请求不发送强度参数。

预检沿唯一模型解析入口生成 `AgentModelSelection`，在 TaskExecutorBinding 中与子任务一同原子注册、持久化。快照固定模型、引擎身份与绑定代际、连接摘要、能力/传输方式、上下文窗口、请求和实际强度；不保存 API Key。排队、重试、恢复和工具后续轮次均使用此快照。运行期设置快照继续负责凭据与原权限；重启恢复从原连接取凭据并核对连接摘要，原引擎移除或连接/账号变化明确失败。角色重新绑定、引擎模型/默认强度改变或能力目录刷新不能悄悄改变既有任务。新 activation 重新进行派发选择，旧任务事实不被改写。

同一快照产生实际 HTTP 请求和用量模型身份；不同强度分别归因。派发回执、事件及代理卡片展示实际模型和强度。提示词提醒选择足够完成任务的强度，高档通常增加延迟与 token；该提示不保证每次选择都最优，也不声称固定节省比例。GPT Web 不参与本轮变更与验证。

定向验证：`cargo test -p magi-bridge-client --lib reasoning`、`cargo test -p magi-bridge-client --lib declared_alias`、`cargo test -p magi-api --lib routes::settings::tests`、Web check 与相关 golden；`npm run test:model-reasoning-live` 使用隔离状态、真实 daemon/App Server/Chromium 和本地模型服务，验证真实 agent_spawn/agent_wait、绑定/继承/默认/非法/未知能力、实际请求、持久化快照，以及切模、滑块、默认状态保留、设置保存及重启恢复。该验收不代表真实外部服务商或发行包验收，也不测试 GPT Web 网站。

本轮验收（2026-10-26）：daemon 编译检查、模型能力/派发/用量/配置清理定向 Rust 回归、设置 API 回归、代理读模型回归、Web check、设置/会话配置/工具卡片/i18n golden 与协议生成检查均通过。隔离真实 daemon + App Server + Electron Chromium 验收通过绑定/继承/默认、显式 none、未知模型不携带强度、非法档位零创建、实际请求参数、按强度用量归因、代理卡片与详情、动态滑块、设置保存/测试、能力读取失败重试及重启恢复。另在子代理等待回答时强制终止测试 daemon，修改角色绑定及引擎模型/默认强度，重启后回答原问题并继续原 Task；原模型/强度保持不变、提问不重放、历史代理投影保持原模型。模型响应使用本地确定性服务，验证执行合同，不代表真实模型自主选档质量、外部服务商网络或发行包已验收。

接口格式修复验收（2026-10-26）：此前只验证自建 `supportedEfforts` 数据，未覆盖实际代理的版本协商目录，不能据此声称真实接口识别完成。本轮补齐 Codex `slug` / `supported_reasoning_levels[].effort` / `default_reasoning_level`、同源展示名别名关联和按需目录加载。实际已配置服务在隔离 daemon 中分别以 Anthropic Messages 与 OpenAI Responses 查询，各 24 个模型，未知项均为 0；GLM 5.3 三档、GPT 5.6 Sol 六档、Kiro Claude 空档位与服务声明一致。这项真实服务验收仅调用目录，无付费推理。75 项设置回归、19 项 reasoning 回归、Web check/构建、61 项 composer 场景、协议检查，以及改用版本协商目录的真实 daemon/Chromium 完整验收通过。

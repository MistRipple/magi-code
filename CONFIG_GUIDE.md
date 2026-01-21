# MultiCLI 配置说明

## 📁 配置结构

MultiCLI 采用系统目录配置结构，所有配置存储在用户目录：

### 配置位置：~/.multicli/

**位置**: 用户目录 `~/.multicli/`

**内容**: 所有配置文件

```
~/.multicli/
├── llm.json             # 所有 LLM 配置（augment, orchestrator, workers, compressor）
├── claude.json          # Claude Worker 画像
├── codex.json           # Codex Worker 画像
├── gemini.json          # Gemini Worker 画像
├── categories.json      # 任务分类配置
├── mcp.json            # MCP 服务器配置（预留）
├── skills.json         # 自定义技能配置（预留）
└── config.json         # 全局配置
```

---

## 🎯 配置职责分离

### LLM 配置（~/.multicli/llm.json）

**为什么在系统目录**:
- ✅ 跨项目共享（所有项目使用相同的 LLM 配置）
- ✅ 独立于 VS Code workspace
- ✅ 易于备份和版本控制
- ✅ 可以手动编辑 JSON 文件
- ✅ 支持环境变量（通过 process.env）

**包含内容**:
- Augment 账号配置
- 编排者模型配置
- Worker 代理模型配置
- 上下文压缩模型配置

### 画像和工具配置（~/.multicli/）

**包含内容**:
- Worker 画像（role, focus, constraints）
- 任务分类配置
- MCP 服务器配置
- 自定义技能配置
- 全局系统配置

### 项目目录（.multicli/）

**仅存储会话相关数据**:
- 当前项目的会话记录
- 临时文件
- 项目特定的快照

---

## 📝 配置文件详解

### 1. llm.json（LLM 配置）

**位置**: `~/.multicli/llm.json`

**结构**:

```json
{
  "augment": {
    "email": "user@example.com",
    "apiKey": "aug_..."
  },
  "orchestrator": {
    "baseUrl": "https://api.anthropic.com",
    "apiKey": "sk-ant-...",
    "model": "claude-3-5-sonnet-20241022",
    "provider": "anthropic",
    "enabled": true
  },
  "workers": {
    "claude": {
      "baseUrl": "https://api.anthropic.com",
      "apiKey": "sk-ant-...",
      "model": "claude-3-5-sonnet-20241022",
      "provider": "anthropic",
      "enabled": true
    },
    "codex": {
      "baseUrl": "https://api.openai.com/v1",
      "apiKey": "sk-...",
      "model": "gpt-4-turbo-preview",
      "provider": "openai",
      "enabled": true
    },
    "gemini": {
      "baseUrl": "https://api.openai.com/v1",
      "apiKey": "sk-...",
      "model": "gpt-4-turbo-preview",
      "provider": "openai",
      "enabled": true
    }
  },
  "compressor": {
    "enabled": false,
    "baseUrl": "https://api.anthropic.com",
    "apiKey": "sk-ant-...",
    "model": "claude-3-haiku-20240307",
    "provider": "anthropic"
  }
}
```

**字段说明**:

- `baseUrl`: API 端点 URL（支持代理）
- `apiKey`: API 密钥（可以直接填写或使用环境变量）
- `model`: 模型名称
- `provider`: 提供商类型（`openai` 或 `anthropic`）
- `enabled`: 是否启用

### 2. claude.json（Worker 画像）

**位置**: `~/.multicli/claude.json`

**结构**:

```json
{
  "name": "Claude",
  "guidance": {
    "role": "全栈开发专家",
    "focus": [
      "代码质量和最佳实践",
      "TypeScript/JavaScript 开发",
      "系统架构设计"
    ],
    "constraints": [
      "遵循项目现有代码风格",
      "优先考虑可维护性",
      "提供清晰的代码注释"
    ]
  }
}
```

### 3. codex.json（Worker 画像）

**位置**: `~/.multicli/codex.json`

**结构**:

```json
{
  "name": "Codex",
  "guidance": {
    "role": "代码生成专家",
    "focus": [
      "快速原型开发",
      "算法实现",
      "代码重构"
    ],
    "constraints": [
      "生成简洁高效的代码",
      "遵循语言惯例",
      "提供使用示例"
    ]
  }
}
```

### 4. gemini.json（Worker 画像）

**位置**: `~/.multicli/gemini.json`

**结构**:

```json
{
  "name": "Gemini",
  "guidance": {
    "role": "多模态专家",
    "focus": [
      "图像理解",
      "多模态交互",
      "创意生成"
    ],
    "constraints": [
      "充分利用多模态能力",
      "提供详细的视觉描述",
      "注重用户体验"
    ]
  }
}
```

---

## 🔧 配置示例

### 1. 使用代理

编辑 `~/.multicli/llm.json`:

```json
{
  "workers": {
    "claude": {
      "baseUrl": "http://localhost:8080/v1",
      "apiKey": "sk-ant-...",
      "model": "claude-3-5-sonnet-20241022",
      "provider": "anthropic",
      "enabled": true
    }
  }
}
```

### 2. 禁用某个 Worker

编辑 `~/.multicli/llm.json`:

```json
{
  "workers": {
    "gemini": {
      "baseUrl": "https://api.openai.com/v1",
      "apiKey": "sk-...",
      "model": "gpt-4-turbo-preview",
      "provider": "openai",
      "enabled": false
    }
  }
}
```

### 3. 使用不同的模型

编辑 `~/.multicli/llm.json`:

```json
{
  "orchestrator": {
    "baseUrl": "https://api.anthropic.com",
    "apiKey": "sk-ant-...",
    "model": "claude-3-opus-20240229",
    "provider": "anthropic",
    "enabled": true
  }
}
```

---

## 🔐 环境变量

### 设置环境变量

**macOS/Linux**:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
```

**Windows (PowerShell)**:

```powershell
$env:ANTHROPIC_API_KEY="sk-ant-..."
$env:OPENAI_API_KEY="sk-..."
```

### 在配置中使用环境变量

系统会自动从环境变量读取 API Key（如果配置文件中为空）：

```json
{
  "orchestrator": {
    "baseUrl": "https://api.anthropic.com",
    "apiKey": "",  // 留空，系统会自动从 ANTHROPIC_API_KEY 读取
    "model": "claude-3-5-sonnet-20241022",
    "provider": "anthropic",
    "enabled": true
  }
}
```

**优先级**:
1. 配置文件中的 `apiKey`（如果不为空）
2. 环境变量 `ANTHROPIC_API_KEY` 或 `OPENAI_API_KEY`

---

## 📊 配置验证

系统会自动验证配置的完整性：

### 必需字段

- `baseUrl`: API 端点 URL
- `apiKey`: API 密钥（或环境变量）
- `model`: 模型名称
- `provider`: 提供商类型（`openai` 或 `anthropic`）

### 验证失败

如果配置验证失败，系统会在日志中输出错误信息：

```
[LLM] Configuration validation failed for orchestrator
  - orchestrator: API key is missing
  - orchestrator: Model is missing
```

---

## 🚀 快速开始

### 1. 设置环境变量

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
```

### 2. 创建配置文件

系统会在首次运行时自动创建默认配置文件：

```bash
~/.multicli/llm.json
~/.multicli/claude.json
~/.multicli/codex.json
~/.multicli/gemini.json
```

### 3. 编辑配置（可选）

编辑 `~/.multicli/llm.json` 修改 LLM 配置：

```json
{
  "orchestrator": {
    "baseUrl": "https://api.anthropic.com",
    "apiKey": "",
    "model": "claude-3-5-sonnet-20241022",
    "provider": "anthropic",
    "enabled": true
  },
  "workers": {
    "claude": {
      "baseUrl": "https://api.anthropic.com",
      "apiKey": "",
      "model": "claude-3-5-sonnet-20241022",
      "provider": "anthropic",
      "enabled": true
    }
  }
}
```

### 4. 编辑 Worker 画像（可选）

编辑 `~/.multicli/claude.json`:

```json
{
  "name": "Claude",
  "guidance": {
    "role": "你的角色定位",
    "focus": ["专注领域1", "专注领域2"],
    "constraints": ["约束1", "约束2"]
  }
}
```

### 5. 重启 VS Code

重新加载 VS Code 窗口以应用配置。

---

## 🔍 故障排查

### API Key 未找到

**问题**: 日志显示 "API key is missing"

**解决**:
1. 检查环境变量是否设置：`echo $ANTHROPIC_API_KEY`
2. 检查 `~/.multicli/llm.json` 中的 `apiKey` 字段
3. 重启 VS Code 以重新加载环境变量

### 配置文件未找到

**问题**: 系统无法找到配置文件

**解决**:
1. 检查 `~/.multicli/` 目录是否存在
2. 运行插件，系统会自动创建默认配置
3. 手动创建配置文件（参考上面的结构）

### 配置未生效

**问题**: 修改配置后没有生效

**解决**:
1. 重新加载 VS Code 窗口（Cmd/Ctrl + Shift + P → "Reload Window"）
2. 检查配置文件语法是否正确（JSON 格式）
3. 查看 VS Code 输出面板的日志

### 代理连接失败

**问题**: 使用代理时连接失败

**解决**:
1. 检查代理服务是否运行
2. 检查 `baseUrl` 是否正确
3. 检查代理是否支持目标 API 格式

---

## 📚 相关文档

- [PHASES_0_TO_6_SUMMARY.md](./PHASES_0_TO_6_SUMMARY.md) - 项目进度总结
- [REFACTOR_CLI_TO_LLM.md](./REFACTOR_CLI_TO_LLM.md) - 重构方案

---

## 🔄 配置迁移

如果你之前使用 VS Code settings.json 配置，需要迁移到 `~/.multicli/llm.json`：

### 迁移步骤

1. **备份旧配置**:
   - 复制 `.vscode/settings.json` 中的 `multicli` 配置

2. **创建新配置**:
   - 在 `~/.multicli/llm.json` 中按照上面的结构填写

3. **删除旧配置**:
   - 从 `.vscode/settings.json` 中删除 `multicli` 配置

4. **重启 VS Code**:
   - 重新加载窗口以应用新配置

---

**最后更新**: 2024年
**状态**: ✅ 配置系统已完成
**配置位置**: `~/.multicli/` (系统目录)

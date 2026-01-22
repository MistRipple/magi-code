# Claude Skills 工具支持实现文档

## 概述

本文档描述了 MultiCLI 项目中 Claude Skills（内置工具）支持的完整实现。该系统允许 Claude 使用 Anthropic 提供的服务器端工具（如 web_search、web_fetch）以及客户端工具（如 text_editor、computer_use）。

## 功能特性

### 已实现功能
- ✅ 内置工具配置管理（启用/禁用）
- ✅ 服务器端工具支持（web_search、web_fetch）
- ✅ 客户端工具框架（text_editor、computer_use）
- ✅ 自定义工具管理（添加/删除）
- ✅ 可视化配置界面
- ✅ 配置持久化存储

### 待实现功能
- ⏳ LLM 客户端工具集成
- ⏳ tool_use 响应处理
- ⏳ tool_result 返回机制
- ⏳ 客户端工具实现（text_editor、computer_use）

## 架构设计

### 系统架构

```
┌─────────────────────────────────────────────────────────────┐
│                        Frontend (Webview)                    │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Skills List  │  │ Custom Tools │  │ Toggle UI    │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                            ↕ Message Passing
┌─────────────────────────────────────────────────────────────┐
│                   Backend (Extension Host)                   │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Config Mgr   │  │ Skills Mgr   │  │ Message Hdlr │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                            ↕ API Request
┌─────────────────────────────────────────────────────────────┐
│                      Claude API (Anthropic)                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Web Search   │  │ Web Fetch    │  │ Tool Use     │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
```

## 实现细节

### 1. 工具管理器 (src/tools/skills-manager.ts)

#### SkillsManager 类

**核心功能**:
- 管理内置工具和自定义工具
- 提供工具定义给 LLM 客户端
- 执行客户端工具
- 区分服务器端和客户端工具

**内置工具定义**:

```typescript
export enum BuiltInTool {
  WEB_SEARCH = 'web_search_20250305',      // 服务器端
  WEB_FETCH = 'web_fetch_20250305',        // 服务器端
  TEXT_EDITOR = 'text_editor_20250124',    // 客户端
  COMPUTER_USE = 'computer_use_20241022'   // 客户端
}
```

**关键方法**:

```typescript
class SkillsManager {
  // 获取所有启用的工具定义
  getEnabledTools(): ToolDefinition[]

  // 检查工具类型
  isServerSideTool(toolName: string): boolean
  isClientSideTool(toolName: string): boolean

  // 工具管理
  enableBuiltInTool(tool: BuiltInTool): void
  disableBuiltInTool(tool: BuiltInTool): void
  addCustomTool(tool: ToolDefinition): void
  removeCustomTool(toolName: string): void

  // 执行客户端工具
  async executeClientTool(toolUse: ToolUseRequest): Promise<ToolResult>
}
```

### 2. 配置管理 (src/llm/config.ts)

#### 配置文件位置
```
~/.multicli/skills.json
```

#### 配置结构
```json
{
  "builtInTools": {
    "web_search_20250305": {
      "enabled": true,
      "description": "搜索网络以获取最新信息"
    },
    "web_fetch_20250305": {
      "enabled": true,
      "description": "获取网页内容"
    },
    "text_editor_20250124": {
      "enabled": false,
      "description": "编辑文本文件（需要客户端实现）"
    },
    "computer_use_20241022": {
      "enabled": false,
      "description": "控制计算机（需要客户端实现）"
    }
  },
  "customTools": [
    {
      "name": "my_custom_tool",
      "description": "My custom tool description",
      "input_schema": {
        "type": "object",
        "properties": {
          "param1": {
            "type": "string",
            "description": "Parameter description"
          }
        },
        "required": ["param1"]
      }
    }
  ]
}
```

#### 配置方法
```typescript
class LLMConfigLoader {
  static loadSkillsConfig(): any
  static saveSkillsConfig(config: any): void
}
```

### 3. 类型定义 (src/types.ts)

#### 消息类型

**前端 → 后端**:
```typescript
| { type: 'loadSkillsConfig' }
| { type: 'saveSkillsConfig'; config: any }
| { type: 'toggleBuiltInTool'; tool: string; enabled: boolean }
| { type: 'addCustomTool'; tool: any }
| { type: 'removeCustomTool'; toolName: string }
```

**后端 → 前端**:
```typescript
| { type: 'skillsConfigLoaded'; config: any }
| { type: 'skillsConfigSaved' }
| { type: 'builtInToolToggled'; tool: string; enabled: boolean }
| { type: 'customToolAdded'; tool: any }
| { type: 'customToolRemoved'; toolName: string }
```

### 4. 后端消息处理 (src/ui/webview-provider.ts)

#### 消息处理器

```typescript
// 加载配置
private async handleLoadSkillsConfig(): Promise<void>

// 保存配置
private async handleSaveSkillsConfig(config: any): Promise<void>

// 切换工具状态
private async handleToggleBuiltInTool(tool: string, enabled: boolean): Promise<void>

// 管理自定义工具
private async handleAddCustomTool(tool: any): Promise<void>
private async handleRemoveCustomTool(toolName: string): Promise<void>
```

### 5. 前端 UI (src/ui/webview/index.html)

#### UI 组件

**1. Skills 工具列表**
- 显示 4 个内置工具
- 每个工具显示：
  - 图标（不同颜色区分类型）
  - 名称和类型标签（Server/Client）
  - 描述
  - 启用/禁用开关

**2. 自定义工具列表**
- 显示用户添加的自定义工具
- 支持编辑和删除操作

**3. 工具类型标识**
- Server: 蓝色标签 - 服务器端执行
- Client: 粉色标签 - 客户端执行

#### JavaScript 功能

```javascript
// 初始化
function initSkillsConfig()

// 渲染
function renderSkillsToolList()
function renderCustomToolList()

// 事件处理
// - 切换工具开关
// - 添加/删除自定义工具
```

#### CSS 样式

**关键样式类**:
- `.skills-tool-list` - 工具列表容器
- `.skills-tool-item` - 单个工具项
- `.skills-tool-icon` - 工具图标（4 种颜色）
- `.skills-tool-toggle` - 开关按钮
- `.skills-tool-type` - 类型标签

## 工具说明

### 服务器端工具（Server-side Tools）

#### 1. Web Search (web_search_20250305)
- **功能**: 搜索网络以获取最新信息
- **执行位置**: Anthropic 服务器
- **参数**:
  ```typescript
  {
    query: string  // 搜索查询
  }
  ```
- **使用场景**: 需要最新信息、实时数据、新闻等

#### 2. Web Fetch (web_fetch_20250305)
- **功能**: 获取并分析网页内容
- **执行位置**: Anthropic 服务器
- **参数**:
  ```typescript
  {
    url: string      // 要获取的 URL
    prompt?: string  // 可选的分析提示
  }
  ```
- **使用场景**: 分析网页内容、提取信息等

### 客户端工具（Client-side Tools）

#### 3. Text Editor (text_editor_20250124)
- **功能**: 编辑文本文件
- **执行位置**: 客户端（VS Code）
- **状态**: 框架已实现，具体功能待开发
- **参数**:
  ```typescript
  {
    command: 'view' | 'create' | 'str_replace' | 'insert' | 'undo_edit'
    path: string
    // ... 其他参数根据命令而定
  }
  ```
- **使用场景**: 文件编辑、代码修改等

#### 4. Computer Use (computer_use_20241022)
- **功能**: 控制计算机（截图、鼠标、键盘等）
- **执行位置**: 客户端（系统级）
- **状态**: 框架已实现，具体功能待开发
- **安全考虑**: 需要额外权限和安全措施
- **参数**:
  ```typescript
  {
    action: 'key' | 'type' | 'mouse_move' | 'left_click' | 'screenshot' | ...
    // ... 其他参数根据动作而定
  }
  ```
- **使用场景**: 自动化操作、UI 测试等

## 使用指南

### 配置内置工具

1. 打开设置面板
2. 切换到"工具"标签
3. 在"Claude Skills（内置工具）"部分：
   - 点击工具右侧的开关启用/禁用
   - Server 类型工具可直接使用
   - Client 类型工具需要额外实现

### 添加自定义工具

1. 在"自定义工具"部分点击"+ 添加工具"
2. 填写工具定义（JSON 格式）：
   ```json
   {
     "name": "my_tool",
     "description": "Tool description",
     "input_schema": {
       "type": "object",
       "properties": {
         "param": {
           "type": "string",
           "description": "Parameter description"
         }
       },
       "required": ["param"]
     }
   }
   ```
3. 保存后工具将添加到列表

## 集成到 LLM 调用流程

### 待实现步骤

#### 1. 在 LLM 客户端中添加工具支持

```typescript
// src/llm/clients/anthropic-client.ts
import { SkillsManager } from '../../tools/skills-manager';

class AnthropicClient {
  private skillsManager: SkillsManager;

  async sendMessage(message: string) {
    // 获取启用的工具
    const tools = this.skillsManager.getEnabledTools();

    // 发送请求时包含工具定义
    const response = await this.client.messages.create({
      model: this.config.model,
      max_tokens: 4096,
      tools: tools,  // 添加工具定义
      messages: [{ role: 'user', content: message }]
    });

    // 处理响应
    return this.handleResponse(response);
  }
}
```

#### 2. 处理 tool_use 响应

```typescript
private async handleResponse(response: any) {
  // 检查是否有 tool_use
  if (response.stop_reason === 'tool_use') {
    const toolUseBlocks = response.content.filter(
      (block: any) => block.type === 'tool_use'
    );

    // 执行工具
    const toolResults = await this.executeTools(toolUseBlocks);

    // 继续对话，返回工具结果
    return this.continueWithToolResults(toolResults);
  }

  // 正常响应
  return response.content;
}
```

#### 3. 执行工具并返回结果

```typescript
private async executeTools(toolUseBlocks: any[]) {
  const results = [];

  for (const toolUse of toolUseBlocks) {
    const { id, name, input } = toolUse;

    // 检查工具类型
    if (this.skillsManager.isServerSideTool(name)) {
      // 服务器端工具已由 Claude 执行，无需处理
      continue;
    }

    if (this.skillsManager.isClientSideTool(name)) {
      // 执行客户端工具
      const result = await this.skillsManager.executeClientTool({
        type: 'tool_use',
        id,
        name,
        input
      });

      results.push(result);
    }
  }

  return results;
}
```

## 工具执行流程

### 服务器端工具流程

```
用户输入 → LLM 请求（包含 tools）→ Claude 决定使用工具
                                        ↓
                                   Claude 执行工具
                                        ↓
                                   返回工具结果
                                        ↓
                                   生成最终响应
```

### 客户端工具流程

```
用户输入 → LLM 请求（包含 tools）→ Claude 决定使用工具
                                        ↓
                                   返回 tool_use 请求
                                        ↓
                              客户端执行工具
                                        ↓
                              返回 tool_result
                                        ↓
                              继续 LLM 对话
                                        ↓
                              生成最终响应
```

## 文件清单

### 新增文件
- `src/tools/skills-manager.ts` (350 行) - Skills 管理器

### 修改文件
- `src/llm/config.ts` (+36 行) - Skills 配置管理
- `src/types.ts` (+10 行) - 消息类型定义
- `src/ui/webview-provider.ts` (+207 行) - 消息处理器
- `src/ui/webview/index.html` (+233 行 JS + 44 行 CSS) - 前端 UI

### 总计
- 新增代码: ~530 行
- 修改文件: 4 个
- 新增文件: 1 个

## 配置示例

### 启用所有服务器端工具

```json
{
  "builtInTools": {
    "web_search_20250305": { "enabled": true },
    "web_fetch_20250305": { "enabled": true },
    "text_editor_20250124": { "enabled": false },
    "computer_use_20241022": { "enabled": false }
  },
  "customTools": []
}
```

### 添加自定义工具

```json
{
  "builtInTools": { ... },
  "customTools": [
    {
      "name": "database_query",
      "description": "Query the database",
      "input_schema": {
        "type": "object",
        "properties": {
          "query": {
            "type": "string",
            "description": "SQL query to execute"
          }
        },
        "required": ["query"]
      }
    }
  ]
}
```

## 安全考虑

### 服务器端工具
- ✅ 由 Anthropic 执行，安全可控
- ✅ 有使用限制和配额
- ✅ 不需要额外权限

### 客户端工具
- ⚠️ 在本地执行，需要谨慎
- ⚠️ text_editor 需要文件系统权限
- ⚠️ computer_use 需要系统级权限
- 🔒 建议：
  - 实现权限确认机制
  - 限制可操作的文件/目录
  - 记录所有操作日志
  - 提供撤销功能

## 测试建议

### 单元测试
- [ ] SkillsManager 工具管理
- [ ] 配置加载和保存
- [ ] 工具类型判断
- [ ] 客户端工具执行

### 集成测试
- [ ] 完整的工具调用流程
- [ ] 服务器端工具使用
- [ ] 客户端工具执行
- [ ] 错误处理

### UI 测试
- [ ] 工具列表渲染
- [ ] 开关切换
- [ ] 自定义工具管理
- [ ] 配置保存

## 下一步工作

### 高优先级
1. **LLM 客户端集成** - 在 API 请求中添加工具支持
2. **tool_use 响应处理** - 处理 Claude 的工具使用请求
3. **tool_result 返回** - 将工具执行结果返回给 Claude

### 中优先级
4. **Text Editor 实现** - 实现文本编辑器工具
5. **错误处理优化** - 完善工具执行错误处理
6. **工具使用统计** - 记录工具使用情况

### 低优先级
7. **Computer Use 实现** - 实现计算机控制工具（需要额外安全措施）
8. **自定义工具对话框** - 添加可视化的自定义工具编辑器
9. **工具使用历史** - 显示工具调用历史记录

## 参考资料

- [Claude Tool Use Documentation](https://docs.anthropic.com/en/docs/build-with-claude/tool-use)
- [Anthropic API Reference](https://docs.anthropic.com/en/api)
- [MCP Protocol](https://modelcontextprotocol.io)

## 总结

Claude Skills 工具支持的基础框架已经完成：
- ✅ 工具管理器实现
- ✅ 配置系统完善
- ✅ 前端 UI 完整
- ✅ 消息处理就绪

下一步需要将工具集成到 LLM 调用流程中，实现完整的工具使用功能。

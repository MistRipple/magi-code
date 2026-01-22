# MCP 工具系统实现文档

## 概述

本文档描述了 MultiCLI 项目中 MCP (Model Context Protocol) 工具管理系统的完整实现。该系统允许用户通过 JSON 配置添加、管理和使用 MCP 服务器，并支持动态刷新工具列表。

## 功能特性

### 核心功能
- ✅ 通过 JSON 配置添加 MCP 服务器
- ✅ 查看 MCP 服务器工具列表
- ✅ 动态刷新工具列表
- ✅ 完整的 CRUD 操作（创建、读取、更新、删除）
- ✅ 服务器连接/断开管理
- ✅ 可视化 UI 界面
- ✅ 实时状态反馈

### 技术特性
- 基于 @modelcontextprotocol/sdk 的标准实现
- Stdio 传输协议支持
- 类型安全的消息传递
- 错误处理和用户反馈
- 配置持久化存储

## 架构设计

### 系统架构

```
┌─────────────────────────────────────────────────────────────┐
│                        Frontend (Webview)                    │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Server List  │  │ Modal Dialog │  │ Tool Display │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                            ↕ Message Passing
┌─────────────────────────────────────────────────────────────┐
│                   Backend (Extension Host)                   │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Config Mgr   │  │ MCP Manager  │  │ Message Hdlr │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
                            ↕ Stdio Transport
┌─────────────────────────────────────────────────────────────┐
│                      MCP Servers (External)                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │ Filesystem   │  │ Database     │  │ Custom Tools │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
└─────────────────────────────────────────────────────────────┘
```

### 数据流

```
用户操作 → 前端 UI → postMessage → 后端处理器 → MCP Manager → MCP Server
                                                          ↓
用户反馈 ← 前端 UI ← postMessage ← 后端响应 ← MCP Manager ← Tool List
```

## 实现细节

### 1. 配置管理 (src/llm/config.ts)

#### 配置文件位置
```
~/.multicli/mcp.json
```

#### 配置结构
```typescript
{
  "servers": [
    {
      "id": "unique-id",
      "name": "Server Name",
      "type": "stdio",
      "command": "npx",
      "args": ["@modelcontextprotocol/server-filesystem", "/path"],
      "env": { "KEY": "value" },
      "enabled": true
    }
  ]
}
```

#### 核心方法
- `loadMCPConfig()`: 加载配置文件
- `saveMCPConfig(servers)`: 保存配置文件
- `addMCPServer(server)`: 添加服务器
- `updateMCPServer(id, updates)`: 更新服务器
- `deleteMCPServer(id)`: 删除服务器

### 2. MCP 客户端管理 (src/tools/mcp-manager.ts)

#### MCPManager 类

**职责**:
- 管理 MCP 服务器连接
- 维护工具列表缓存
- 处理工具调用

**核心方法**:

```typescript
class MCPManager {
  // 连接到 MCP 服务器
  async connectServer(config: MCPServerConfig): Promise<void>

  // 断开服务器连接
  async disconnectServer(serverId: string): Promise<void>

  // 获取服务器工具列表
  getServerTools(serverId: string): MCPToolInfo[]

  // 刷新工具列表
  async refreshServerTools(serverId: string): Promise<MCPToolInfo[]>

  // 调用工具
  async callTool(serverId: string, toolName: string, args: any): Promise<any>

  // 获取所有工具
  getAllTools(): MCPToolInfo[]
}
```

**连接流程**:
1. 验证配置有效性
2. 创建 StdioClientTransport
3. 创建 MCP Client
4. 连接到服务器
5. 获取工具列表
6. 缓存客户端和工具

### 3. 类型定义 (src/types.ts)

#### 消息类型

**前端 → 后端**:
```typescript
| { type: 'loadMCPServers' }
| { type: 'addMCPServer'; server: any }
| { type: 'updateMCPServer'; serverId: string; updates: any }
| { type: 'deleteMCPServer'; serverId: string }
| { type: 'connectMCPServer'; serverId: string }
| { type: 'disconnectMCPServer'; serverId: string }
| { type: 'refreshMCPTools'; serverId: string }
| { type: 'getMCPServerTools'; serverId: string }
```

**后端 → 前端**:
```typescript
| { type: 'mcpServersLoaded'; servers: any[] }
| { type: 'mcpServerAdded'; server: any }
| { type: 'mcpServerUpdated'; serverId: string }
| { type: 'mcpServerDeleted'; serverId: string }
| { type: 'mcpServerConnected'; serverId: string; toolCount: number }
| { type: 'mcpServerDisconnected'; serverId: string }
| { type: 'mcpServerConnectionFailed'; serverId: string; error: string }
| { type: 'mcpToolsRefreshed'; serverId: string; tools: any[] }
| { type: 'mcpServerTools'; serverId: string; tools: any[] }
```

### 4. 后端消息处理 (src/ui/webview-provider.ts)

#### 消息路由

在 `handleMessage` 方法中添加了 8 个 case 分支，每个对应一个 MCP 操作。

#### 处理器实现示例

```typescript
private async handleConnectMCPServer(serverId: string): Promise<void> {
  try {
    // 1. 加载配置
    const servers = LLMConfigLoader.loadMCPConfig();
    const server = servers.find((s: any) => s.id === serverId);

    // 2. 验证服务器
    if (!server) throw new Error(`服务器不存在: ${serverId}`);
    if (!server.enabled) throw new Error('服务器未启用');

    // 3. 连接服务器
    const manager = await this.getMCPManager();
    await manager.connectServer(server);

    // 4. 获取工具列表
    const tools = manager.getServerTools(serverId);

    // 5. 发送成功响应
    this.postMessage({
      type: 'mcpServerConnected',
      serverId,
      toolCount: tools.length
    });

    // 6. 显示成功提示
    this.postMessage({
      type: 'toast',
      message: `服务器已连接，发现 ${tools.length} 个工具`,
      toastType: 'success'
    });
  } catch (error: any) {
    // 错误处理
    this.postMessage({
      type: 'mcpServerConnectionFailed',
      serverId,
      error: error.message
    });
  }
}
```

### 5. 前端 UI (src/ui/webview/index.html)

#### 主要组件

**1. 服务器列表**
```javascript
function renderMCPServerList() {
  // 渲染服务器卡片
  // 每个卡片包含：
  // - 服务器名称和命令
  // - 操作按钮（连接、查看工具、刷新、编辑、删除）
  // - 可展开的工具列表区域
}
```

**2. 模态对话框**
```javascript
function showMCPDialog(server = null) {
  // 显示添加/编辑对话框
  // 包含表单字段：
  // - 服务器名称
  // - 命令
  // - 参数（JSON 数组）
  // - 环境变量（JSON 对象）
  // - 启用状态
}
```

**3. 工具显示**
```javascript
function renderMCPTools(serverId, tools) {
  // 渲染工具列表
  // 每个工具显示：
  // - 工具图标
  // - 工具名称
  // - 工具描述
}
```

#### 事件处理

```javascript
function handleMCPAction(action, serverId) {
  switch (action) {
    case 'connect':
      vscode.postMessage({ type: 'connectMCPServer', serverId });
      break;
    case 'tools':
      toggleMCPTools(serverId);
      break;
    case 'refresh':
      vscode.postMessage({ type: 'refreshMCPTools', serverId });
      break;
    case 'edit':
      const server = mcpServers.find(s => s.id === serverId);
      showMCPDialog(server);
      break;
    case 'delete':
      if (confirm('确定要删除此服务器吗？')) {
        vscode.postMessage({ type: 'deleteMCPServer', serverId });
      }
      break;
  }
}
```

#### 消息监听

```javascript
window.addEventListener('message', event => {
  const msg = event.data;

  if (msg.type === 'mcpServersLoaded') {
    mcpServers = msg.servers || [];
    renderMCPServerList();
  }
  else if (msg.type === 'mcpServerConnected') {
    // 更新 UI 显示连接状态
  }
  else if (msg.type === 'mcpToolsRefreshed') {
    renderMCPTools(msg.serverId, msg.tools);
  }
  // ... 其他消息处理
});
```

### 6. CSS 样式

#### 关键样式类

**服务器列表**:
- `.mcp-server-list`: 服务器列表容器
- `.mcp-server-item`: 单个服务器卡片
- `.mcp-server-header`: 服务器头部（名称和操作）
- `.mcp-server-actions`: 操作按钮组
- `.mcp-action-btn`: 操作按钮

**工具显示**:
- `.mcp-server-tools`: 工具列表容器
- `.mcp-tool-item`: 单个工具项
- `.mcp-tool-icon`: 工具图标
- `.mcp-tool-info`: 工具信息

**模态对话框**:
- `.modal-overlay`: 遮罩层
- `.modal-dialog`: 对话框容器
- `.modal-header`: 对话框头部
- `.modal-body`: 对话框内容
- `.modal-footer`: 对话框底部
- `.form-field`: 表单字段

## 使用指南

### 添加 MCP 服务器

1. 打开设置面板
2. 切换到"工具"标签
3. 点击"添加服务器"按钮
4. 填写服务器信息：
   - 名称：例如 "filesystem"
   - 命令：例如 "npx"
   - 参数：例如 `["@modelcontextprotocol/server-filesystem", "/path"]`
   - 环境变量：例如 `{"KEY": "value"}`
   - 勾选"启用此服务器"
5. 点击"保存"

### 连接服务器

1. 在服务器列表中找到目标服务器
2. 点击"连接"按钮（链接图标）
3. 等待连接成功提示
4. 连接成功后可以查看工具列表

### 查看工具列表

1. 点击"查看工具"按钮（工具图标）
2. 工具列表会展开显示
3. 每个工具显示名称和描述

### 刷新工具列表

1. 点击"刷新工具列表"按钮（刷新图标）
2. 系统会重新获取最新的工具列表
3. 工具列表自动更新

### 编辑服务器

1. 点击"编辑"按钮（铅笔图标）
2. 在对话框中修改配置
3. 点击"保存"

### 删除服务器

1. 点击"删除"按钮（垃圾桶图标）
2. 确认删除操作
3. 服务器配置将被永久删除

## 配置示例

### Filesystem Server

```json
{
  "id": "filesystem-1",
  "name": "Filesystem",
  "type": "stdio",
  "command": "npx",
  "args": [
    "@modelcontextprotocol/server-filesystem",
    "/Users/username/allowed-directory"
  ],
  "env": {},
  "enabled": true
}
```

### Database Server

```json
{
  "id": "database-1",
  "name": "PostgreSQL",
  "type": "stdio",
  "command": "npx",
  "args": [
    "@modelcontextprotocol/server-postgres",
    "postgresql://localhost/mydb"
  ],
  "env": {
    "PGPASSWORD": "secret"
  },
  "enabled": true
}
```

### Custom Server

```json
{
  "id": "custom-1",
  "name": "My Custom Server",
  "type": "stdio",
  "command": "node",
  "args": [
    "/path/to/my-mcp-server/index.js"
  ],
  "env": {
    "API_KEY": "your-api-key"
  },
  "enabled": true
}
```

## 错误处理

### 常见错误

1. **服务器连接失败**
   - 检查命令和参数是否正确
   - 确认 MCP 服务器已安装
   - 查看环境变量配置

2. **工具列表为空**
   - 确认服务器已成功连接
   - 尝试刷新工具列表
   - 检查服务器日志

3. **配置保存失败**
   - 检查 JSON 格式是否正确
   - 确认有写入权限
   - 查看错误提示信息

### 调试技巧

1. 查看 VS Code 开发者工具控制台
2. 检查 ~/.multicli/mcp.json 配置文件
3. 使用"重新检测"按钮刷新状态
4. 查看 Toast 通知中的错误信息

## 技术栈

- **前端**: HTML + CSS + JavaScript (Vanilla)
- **后端**: TypeScript + Node.js
- **MCP SDK**: @modelcontextprotocol/sdk
- **通信**: VS Code Webview Message Passing
- **存储**: JSON 文件 (~/.multicli/mcp.json)

## 文件清单

### 新增文件
- `src/tools/mcp-manager.ts` (230 行) - MCP 客户端管理器

### 修改文件
- `src/llm/config.ts` (+81 行) - 配置管理方法
- `src/types.ts` (+16 行) - 消息类型定义
- `src/ui/webview-provider.ts` (+276 行) - 消息处理器
- `src/ui/webview/index.html` (+373 行) - 前端 UI 和样式

### 总计
- 新增代码: ~746 行
- 修改文件: 4 个
- 新增文件: 1 个

## 测试建议

### 单元测试
- [ ] 配置加载和保存
- [ ] MCP 客户端连接
- [ ] 工具列表获取
- [ ] 错误处理

### 集成测试
- [ ] 完整的添加-连接-查看流程
- [ ] 配置更新和删除
- [ ] 工具刷新功能
- [ ] 多服务器管理

### UI 测试
- [ ] 服务器列表渲染
- [ ] 模态对话框交互
- [ ] 工具列表展开/折叠
- [ ] 错误提示显示

## 未来改进

### 功能增强
- [ ] 支持 SSE 传输协议
- [ ] 服务器健康检查
- [ ] 工具使用统计
- [ ] 批量操作支持
- [ ] 配置导入/导出

### 性能优化
- [ ] 工具列表缓存策略
- [ ] 连接池管理
- [ ] 懒加载工具详情
- [ ] 虚拟滚动优化

### 用户体验
- [ ] 搜索和过滤功能
- [ ] 服务器分组管理
- [ ] 快捷键支持
- [ ] 拖拽排序
- [ ] 主题定制

## 总结

MCP 工具系统已完整实现，提供了：
- ✅ 完整的 CRUD 功能
- ✅ 可视化管理界面
- ✅ 动态工具发现
- ✅ 类型安全的实现
- ✅ 良好的错误处理
- ✅ 用户友好的交互

系统已通过编译验证，可以投入使用。

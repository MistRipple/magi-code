# 三层数据流调试测试指南

## 已添加的调试日志

### ✅ 第一层：CLI 原始数据
**文件**: `src/ui/webview-provider.ts:677-683`
**日志标识**: `[DEBUG-LAYER-1]`
**记录内容**:
- 消息类型
- 内容长度
- 内容预览（前 200 字符）
- 是否包含 JSON

### ✅ 第二层：后端处理
**文件**: `src/normalizer/orchestrator-normalizer.ts:39-67`
**日志标识**: `[DEBUG-LAYER-2]`
**记录内容**:
- 进入 normalizer 时的原始内容
- parseContentToBlocks 返回的 blocks 信息
- 每个 block 的类型和内容预览

**文件**: `src/utils/content-parser.ts:263-286`
**日志标识**: `[content-parser]`
**记录内容**:
- 发现的裸露 JSON 数量
- 每个 JSON 的位置和预览
- 移除后的内容长度

### ✅ 第三层：前端渲染
**文件**: `src/ui/webview/index.html:2795-2806`
**日志标识**: `[DEBUG-LAYER-3]`
**记录内容**:
- 前端接收到的消息信息
- blocks 数量和类型
- 第一个 block 的预览

**文件**: `src/ui/webview/index.html:4453-4473`
**日志标识**: `[DEBUG-LAYER-3]`
**记录内容**:
- 开始渲染时的 blocks 信息
- 每个 block 的渲染详情

---

## 测试步骤

### 1. 重启 VSCode 扩展
```
Cmd+Shift+P → "Reload Window"
或
Cmd+R
```

### 2. 打开开发者工具
```
帮助 → 切换开发人员工具
或
Cmd+Option+I
```

### 3. 清空控制台
点击控制台的清空按钮（🚫 图标）

### 4. 发送测试消息
```
"做一个登录功能，包含前后端的，使用python和vue"
```

### 5. 观察控制台日志

---

## 日志分析指南

### 场景 A：正常情况（JSON 被成功移除）

**预期日志顺序**:
```
[DEBUG-LAYER-1] 原始 Orchestrator 消息: { type: 'direct_response', contentLength: 500, hasJson: true, ... }
[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口: { messageId: 'msg-orch-xxx', contentLength: 500, ... }
[content-parser] 发现裸露 JSON: 1 个
[content-parser] JSON 1: { startIndex: 100, endIndex: 300, length: 200, preview: '{"constraints":...' }
[content-parser] 移除 JSON 后的内容长度: 300
[DEBUG-LAYER-2] parseContentToBlocks 返回: { blocksCount: 1, blockTypes: ['text'], ... }
[DEBUG-LAYER-3] 前端接收 standardMessage: { blocksCount: 1, blockTypes: ['text'], ... }
[DEBUG-LAYER-3] renderParsedBlocks 开始渲染: { blocksCount: 1, blockTypes: ['text'] }
[DEBUG-LAYER-3] 渲染 block: { type: 'text', contentLength: 300, isMarkdown: false }
```

**结论**: ✅ 一切正常，JSON 被成功移除

---

### 场景 B：没有 [content-parser] 日志

**实际日志**:
```
[DEBUG-LAYER-1] 原始 Orchestrator 消息: { type: 'direct_response', contentLength: 500, hasJson: true, ... }
[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口: { messageId: 'msg-orch-xxx', contentLength: 500, ... }
[DEBUG-LAYER-2] parseContentToBlocks 返回: { blocksCount: 2, blockTypes: ['text', 'code'], ... }
[DEBUG-LAYER-3] 前端接收 standardMessage: { blocksCount: 2, blockTypes: ['text', 'code'], ... }
```

**问题**: extractEmbeddedJson 没有找到 JSON

**可能原因**:
1. JSON 格式不符合预期（不是标准的 `{` 或 `[` 开头）
2. JSON 在代码块中（被跳过）
3. JSON 不是有效的 JSON（JSON.parse 失败）

**下一步**:
- 查看 `[DEBUG-LAYER-1]` 的 `contentPreview`，确认 JSON 的实际格式
- 检查 JSON 是否在 ``` 代码块中
- 手动测试 JSON.parse 是否能解析该 JSON

---

### 场景 C：有 [content-parser] 日志但 JSON 还在显示

**实际日志**:
```
[DEBUG-LAYER-1] 原始 Orchestrator 消息: { type: 'direct_response', contentLength: 500, hasJson: true, ... }
[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口: { messageId: 'msg-orch-xxx', contentLength: 500, ... }
[content-parser] 发现裸露 JSON: 1 个
[content-parser] JSON 1: { startIndex: 100, endIndex: 300, length: 200, preview: '{"constraints":...' }
[content-parser] 移除 JSON 后的内容长度: 300
[DEBUG-LAYER-2] parseContentToBlocks 返回: { blocksCount: 2, blockTypes: ['text', 'code'], ... }
```

**问题**: JSON 被检测到但移除后仍然有 code block

**可能原因**:
1. 移除逻辑的索引计算错误
2. 移除后的内容又被 parseTextContent 识别为 JSON
3. 有多个 JSON，只移除了部分

**下一步**:
- 比较 `contentLength: 500` 和 `移除 JSON 后的内容长度: 300`，确认移除是否生效
- 检查 `blockTypes` 中是否有 code 类型
- 查看 `[DEBUG-LAYER-3]` 的 block 详情，确认 code block 的内容

---

### 场景 D：前端显示的内容与 blocks 不符

**实际日志**:
```
[DEBUG-LAYER-2] parseContentToBlocks 返回: { blocksCount: 1, blockTypes: ['text'], ... }
[DEBUG-LAYER-3] 前端接收 standardMessage: { blocksCount: 1, blockTypes: ['text'], ... }
[DEBUG-LAYER-3] renderParsedBlocks 开始渲染: { blocksCount: 1, blockTypes: ['text'] }
[DEBUG-LAYER-3] 渲染 block: { type: 'text', contentLength: 300, isMarkdown: false }
```

但前端仍然显示 JSON 代码块

**问题**: 前端渲染逻辑有问题

**可能原因**:
1. 前端使用了旧的 content 而不是 blocks
2. 前端缓存了旧消息
3. 前端的 JSON 检测逻辑没有完全移除

**下一步**:
- 检查前端是否使用了 `message.content` 而不是 `message.blocks`
- 清空浏览器缓存并重新加载
- 检查 renderParsedBlocks 的 switch 语句

---

### 场景 E：没有任何日志

**问题**: parseContentToBlocks 没有被调用

**可能原因**:
1. normalizeOrchestratorMessage 没有执行
2. uiMessage.content 为空
3. 事件监听器没有触发

**下一步**:
- 检查是否有 `[DEBUG-LAYER-1]` 日志
- 如果没有，说明 orchestrator:ui_message 事件没有触发
- 检查 Orchestrator 是否正常工作

---

## 关键检查点

### ✅ 检查点 1：原始内容是否包含 JSON
查看 `[DEBUG-LAYER-1]` 的 `hasJson` 字段和 `contentPreview`

### ✅ 检查点 2：JSON 是否被检测到
查看是否有 `[content-parser] 发现裸露 JSON` 日志

### ✅ 检查点 3：JSON 是否被移除
比较移除前后的内容长度

### ✅ 检查点 4：blocks 类型是否正确
查看 `[DEBUG-LAYER-2]` 的 `blockTypes`，应该只有 `['text']`，不应该有 `'code'`

### ✅ 检查点 5：前端是否正确渲染
查看 `[DEBUG-LAYER-3]` 的渲染日志，确认渲染的 block 类型

---

## 提供给开发者的信息

测试后，请提供以下信息：

1. **完整的控制台日志**（从发送消息到渲染完成）
2. **前端显示的截图**（显示是否还有 JSON 代码块）
3. **具体的场景**（A、B、C、D 或 E）

根据这些信息，可以精确定位问题出在哪一层。

---

## 编译状态

✅ 已编译通过
✅ 所有调试日志已添加
✅ 准备测试

---

**下一步**: 用户测试并提供日志输出

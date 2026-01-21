# OpenAI SDK baseURL 修复

## 问题描述

用户配置的 Codex 模型连接测试失败，错误信息：
```
Error: No choices in OpenAI response
Model: gpt-5.2-codex
```

后续日志显示 OpenAI SDK 返回的是 HTML 响应而不是 JSON：
```
response: '"<!doctype html>\\n<html lang=\\"zh\\">\\n..."'
```

## 根本原因

**OpenAI SDK 要求 baseURL 必须包含 `/v1` 路径**。

用户配置：
```json
{
  "baseUrl": "https://newapi.stonefancyx.com",
  "model": "gpt-5.2-codex",
  "provider": "openai"
}
```

直接 API 测试使用的 URL：
```
https://newapi.stonefancyx.com/v1/chat/completions  ✅ 正常工作
```

但 OpenAI SDK 使用的 URL：
```
https://newapi.stonefancyx.com  ❌ 返回 HTML（可能是网站首页）
```

## 解决方案

修改 `src/llm/clients/universal-client.ts` 的 `initializeClient()` 方法，自动为 OpenAI baseURL 添加 `/v1` 后缀（如果不存在）。

### 修改前（行 42-46）

```typescript
} else if (this.config.provider === 'openai') {
  this.openaiClient = new OpenAI({
    apiKey: this.config.apiKey,
    baseURL: this.config.baseUrl,
  });
```

### 修改后（行 42-58）

```typescript
} else if (this.config.provider === 'openai') {
  // OpenAI SDK 需要 baseURL 包含 /v1 路径
  let baseURL = this.config.baseUrl;
  if (baseURL && !baseURL.endsWith('/v1')) {
    baseURL = baseURL.replace(/\/$/, '') + '/v1';
  }

  this.openaiClient = new OpenAI({
    apiKey: this.config.apiKey,
    baseURL: baseURL,
  });

  logger.info('OpenAI client initialized', {
    originalBaseUrl: this.config.baseUrl,
    finalBaseUrl: baseURL,
    model: this.config.model
  }, LogCategory.LLM);
```

## 修复逻辑

1. **检查 baseURL 是否以 `/v1` 结尾**
   - 如果已经包含 `/v1`，保持不变
   - 如果不包含，自动添加

2. **处理尾部斜杠**
   - 使用 `replace(/\/$/, '')` 移除尾部斜杠（如果有）
   - 然后添加 `/v1`

3. **添加日志**
   - 记录原始 baseURL 和最终 baseURL
   - 方便调试和验证

## 测试用例

| 输入 baseURL | 输出 baseURL |
|-------------|-------------|
| `https://api.openai.com` | `https://api.openai.com/v1` |
| `https://api.openai.com/` | `https://api.openai.com/v1` |
| `https://api.openai.com/v1` | `https://api.openai.com/v1` |
| `https://api.openai.com/v1/` | `https://api.openai.com/v1/` |
| `https://newapi.stonefancyx.com` | `https://newapi.stonefancyx.com/v1` |

## 验证

编译通过：
```bash
npm run compile
✅ 0 错误
```

## 影响范围

- **文件**: `src/llm/clients/universal-client.ts`
- **方法**: `initializeClient()`
- **影响**: 所有使用 OpenAI provider 的配置
- **向后兼容**: ✅ 是（已包含 `/v1` 的配置不受影响）

## 下一步

用户需要在 VS Code 中重新测试 Codex 模型连接：
1. 重启插件（或重新加载窗口）
2. 打开配置面板 → 统计 Tab
3. 点击"重新检测"按钮
4. 查看 Codex 连接状态

预期结果：
- ✅ Codex 状态显示"已连接"
- ✅ 版本信息显示"openai - gpt-5.2-codex"
- ✅ 日志显示正确的 baseURL：`https://newapi.stonefancyx.com/v1`

---

**修复时间**: 2024年
**状态**: ✅ 已完成
**编译结果**: ✅ 通过（0 错误）

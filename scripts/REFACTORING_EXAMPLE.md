# 测试脚本重构示例

## 重构目标

使用统一的 `test-utils.js` 工具库替代重复的测试代码，提高可维护性和一致性。

## 示例: test-orchestrator-workers-e2e.js

### 重构前 (162 行)

**问题**:
- 重复定义颜色常量 (~10 行)
- 手动管理 passed/failed 计数器
- 手动计算测试时长
- 手动格式化输出
- 不统一的测试结果展示

```javascript
// 颜色输出
const colors = {
  reset: '\x1b[0m', red: '\x1b[31m', green: '\x1b[32m',
  yellow: '\x1b[33m', blue: '\x1b[34m', magenta: '\x1b[35m', cyan: '\x1b[36m',
};
const log = (msg, color = 'reset') => console.log(`${colors[color]}${msg}${colors.reset}`);
const logSection = (title) => {
  console.log(`\n${'='.repeat(70)}`);
  console.log(`  ${title}`);
  console.log(`${'='.repeat(70)}`);
};

async function main() {
  console.log('🚀 画像系统单元测试\n');
  let passed = 0, failed = 0;
  const startTime = Date.now();

  // 测试逻辑
  const profiles = profileLoader.getAllProfiles();
  log(`加载画像数量: ${profiles.size}`, profiles.size >= 3 ? 'green' : 'red');
  if (profiles.size >= 3) passed++; else failed++;

  // ... 更多测试 ...

  // 手动输出汇总
  const duration = ((Date.now() - startTime) / 1000).toFixed(2);
  const total = passed + failed;
  log(`⏱️  耗时: ${duration}s`, 'blue');
  log(`✅ 通过: ${passed}/${total} (${((passed/total)*100).toFixed(1)}%)`, passed === total ? 'green' : 'yellow');
  if (failed > 0) log(`❌ 失败: ${failed}/${total}`, 'red');
}
```

### 重构后 (148 行, -14 行 / -8.6%)

**改进**:
- ✅ 导入 TestRunner，无需重复定义颜色和日志函数
- ✅ 自动追踪测试结果
- ✅ 自动计算时长
- ✅ 统一的输出格式
- ✅ 失败测试自动高亮显示

```javascript
const { TestRunner } = require('./test-utils');

async function main() {
  const runner = new TestRunner('画像系统单元测试');

  try {
    // 1. 测试 ProfileLoader
    runner.logSection('1. ProfileLoader 测试');
    const profileLoader = new ProfileLoader(workspaceRoot);
    await profileLoader.load();

    const profiles = profileLoader.getAllProfiles();
    runner.logTest(
      '加载画像数量',
      profiles.size >= 3,
      `加载了 ${profiles.size} 个画像`
    );

    // ... 更多测试 ...

    // 自动汇总并返回退出码
    process.exit(runner.finish());

  } catch (error) {
    runner.log(`\n❌ 测试失败: ${error.message}`, 'red');
    console.error(error);
    process.exit(1);
  }
}
```

## 输出对比

### 重构前输出
```
🚀 画像系统单元测试

================================================================================
  1. ProfileLoader 测试
================================================================================
加载画像数量: 3
  claude: guidance=✅, guidance.role=✅
...
⏱️  耗时: 0.03s
✅ 通过: 8/9 (88.9%)
❌ 失败: 1/9
```

### 重构后输出
```
================================================================================
  1. ProfileLoader 测试
================================================================================
✅ 加载画像数量
   加载了 3 个画像
✅ claude 画像结构
   guidance=✅, guidance.role=✅
...

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  测试结果汇总
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
⏱️  耗时: 0.03s
✅ 通过: 8/9 (88.9%)
❌ 失败: 1/9

失败的测试:
  ❌ 任务分析: "分析 src/orchestrator 目录的代码结构..."
     类型: architecture, 复杂度: 2
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

**关键改进**:
1. ✅ 每个测试都有清晰的 ✅/❌ 标识
2. ✅ 测试名称和详情分行显示，更易读
3. ✅ 失败测试自动列出在结果汇总中
4. ✅ 使用双线分隔符（━）区分汇总区域

## API 使用指南

### TestRunner 核心方法

```javascript
const { TestRunner } = require('./test-utils');

// 1. 创建运行器
const runner = new TestRunner('测试套件名称');

// 2. 添加章节标题
runner.logSection('第一阶段: 初始化');

// 3. 记录测试结果
runner.logTest(
  '测试名称',          // 显示名称
  true/false,          // 是否通过
  '可选的详细信息'     // 显示在测试下方
);

// 4. 输出彩色日志
runner.log('消息内容', 'green');  // 支持: green, red, yellow, blue, cyan, magenta

// 5. 完成测试并返回退出码
process.exit(runner.finish());  // 返回 0 (全部通过) 或 1 (有失败)
```

### waitFor 辅助函数

```javascript
const { waitFor } = require('./test-utils');

// 等待异步条件满足
const success = await waitFor(
  async () => {
    // 返回布尔值的检查函数
    return someAsyncCondition();
  },
  5000,   // 超时时间 (毫秒，默认 5000)
  100     // 检查间隔 (毫秒，默认 100)
);

runner.logTest('异步条件满足', success);
```

## 下一步计划

根据 `docs/测试架构演进计划.md`，接下来需要重构:

1. ✅ `test-orchestrator-workers-e2e.js` (已完成)
2. ⏳ `e2e-architecture-test.js` (部分完成，需继续)
3. ⏳ `test-architecture-optimization.js` (部分完成，需继续)
4. ⏳ `test-orchestrator-flow.js` (未开始)

**预期效果**:
- 消除 ~150 行重复代码
- 所有测试输出格式统一
- 更容易添加新测试
- 更好的失败诊断信息

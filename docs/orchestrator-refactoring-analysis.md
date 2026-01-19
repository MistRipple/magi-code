# OrchestratorAgent 深度分析与重构方案

> **核心原则**: 结合项目现有的 Worker 画像系统特色，不盲目照搬外部设计

## 一、项目核心特色理解

### 1.1 MultiCLI 独特架构

MultiCLI 采用 **"引导而非限制"** 的设计理念，与 oh-my-opencode 的 **"委派+验证"** 模式有本质区别：

| 维度 | MultiCLI (本项目) | oh-my-opencode |
|------|------------------|----------------|
| **核心理念** | 引导式编排 (Prompt 引导行为) | 委派式编排 (工具限制权限) |
| **Worker 选择** | 画像驱动 + 分类匹配 + 执行统计 | 硬编码 Agent 列表 |
| **CLI 进程** | 复用成熟 CLI 完整能力 | 自建 Agent 系统 |
| **协作模式** | 主导者/协作者角色切换 | 主 Agent + 子 Agent |
| **配置管理** | ~/.multicli/ 用户级配置 | .opencode/ 项目级配置 |

### 1.2 项目核心组件关系

```
┌─────────────────────────────────────────────────────────────────┐
│                      ProfileLoader (画像系统)                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │WorkerProfile│  │ Categories  │  │ CategoryRules│              │
│  │  (能力画像)  │  │ (任务分类)   │  │ (分类规则)   │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
└───────────────────────────┬─────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│                    OrchestratorAgent (编排核心)                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │ PolicyEngine│  │ CLISelector │  │GuidanceInjector│            │
│  │  (策略引擎)  │  │ (CLI选择器) │  │ (引导注入器)  │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
└───────────────────────────┬─────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│                      WorkerPool (执行层)                          │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │ Claude Worker│  │ Codex Worker│  │Gemini Worker│              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
└─────────────────────────────────────────────────────────────────┘
```

### 1.3 画像系统核心设计

```typescript
// WorkerProfile - 核心数据结构
interface WorkerProfile {
  profile: { strengths, weaknesses };      // 能力画像
  preferences: { preferredCategories };     // 任务偏好
  guidance: { role, focus, constraints };   // 行为引导
  collaboration: { asLeader, asCollaborator }; // 协作规则
}

// CategoryConfig - 任务分类
interface CategoryConfig {
  keywords: string[];        // 匹配关键词
  defaultWorker: CLIType;    // 默认 Worker
  riskLevel: RiskLevel;      // 风险等级
}
```

### 1.4 当前执行流程

1. **Phase 0**: Intent Gate - 意图门控（AI 决策）
2. **Phase 1**: Task Analysis - 任务分析（画像驱动）
3. **Phase 2**: Plan Review - 计划评审
4. **Phase 3**: Dispatch - 任务分发（CLISelector + GuidanceInjector）
5. **Phase 4**: Monitor - 监控执行
6. **Phase 4.5**: Integration - 功能集成联调
7. **Phase 5**: Verification - 编译/Lint/测试验证
8. **Phase 6**: Summary - 结果汇总

---

## 二、核心问题分析（结合项目特色）

### 2.1 架构层面问题

| 问题 | 严重程度 | 影响 | 与画像系统的关系 |
|------|----------|------|-----------------|
| **巨型单文件** (4832行) | 🔴 高 | 难以维护、测试、扩展 | 画像系统已独立，但编排核心未拆分 |
| **职责混杂** | 🔴 高 | 意图/分析/评审/执行/验证混在一起 | PolicyEngine 已部分分离，但未彻底 |
| **评审与画像脱节** | 🔴 高 | 评审未利用 Worker 能力画像 | strengths/weaknesses 未用于评审决策 |
| **状态管理散乱** | 🟡 中 | 15+ 个 Map/Set 状态变量 | - |
| **回调地狱** | 🟡 中 | 6 种回调类型混合使用 | - |

### 2.2 评审机制问题

**当前评审机制与画像系统脱节**：

```typescript
// 问题1: Plan Review 未利用画像信息
private async reviewPlan(plan, formattedPlan): Promise<PlanReview> {
  // ❌ 未检查子任务分配是否符合 Worker 的 strengths
  // ❌ 未利用 CategoryConfig.riskLevel 调整评审严格度
  // ❌ 评审失败直接抛错，无基于画像的修复建议
}

// 问题2: SubTask Review 未感知 Worker 能力
private async runSubTaskReviews(subTask, result, config): Promise<ReviewDecision> {
  // ❌ 判断是否需要 peerReview 仅基于文件扩展名和关键词
  // ❌ 未考虑执行 Worker 的 weaknesses（可能需要更严格评审）
  // ❌ 未利用 collaboration.asCollaborator 规则指导互检
}

// 问题3: 评审者选择太简单
private selectPeerReviewer(subTask: SubTask): CLIType {
  // ❌ 仅排除执行者后取第一个
  // ❌ 应该选择 strengths 匹配当前任务分类的 Worker
}
```

### 2.3 应该保留和增强的项目特色

| 已有优势 | 当前实现 | 增强方向 |
|----------|----------|----------|
| **画像驱动选择** | CLISelector + ProfileLoader | ✅ 保持，增强评审集成 |
| **引导式 Prompt** | GuidanceInjector | ✅ 保持，增加评审引导 |
| **分类风险映射** | CategoryConfig.riskLevel | ⚡ 增强，连接评审策略 |
| **执行统计降级** | ExecutionStats + PolicyEngine | ✅ 保持 |
| **协作角色切换** | asLeader/asCollaborator | ⚡ 增强，用于互检匹配 |

### 2.4 可借鉴但需适配的外部设计

| oh-my-opencode 设计 | 适配方式 |
|---------------------|----------|
| SUBAGENTS LIE 强制验证 | 转化为"基于 Worker weaknesses 的针对性验证" |
| 3 次失败恢复机制 | 结合 RecoveryHandler，增加画像感知 |
| Hook 系统 | 可引入，但不替代画像系统 |
| 后台任务并发控制 | 增强 WorkerPool，基于 Worker 能力分配 |

---

## 三、重构方案（结合画像系统）

> **核心原则**: 以画像系统为基础，增强评审机制，而非照搬外部设计

### 3.1 评审机制重构：画像驱动评审 ⭐

```typescript
// 新设计：ProfileAwareReviewer - 画像感知的评审器
export class ProfileAwareReviewer {
  constructor(
    private profileLoader: ProfileLoader,
    private policyEngine: PolicyEngine
  ) {}

  /**
   * 计划评审：检查任务分配是否符合 Worker 能力
   */
  async reviewPlan(plan: ExecutionPlan): Promise<PlanReviewResult> {
    const issues: PlanIssue[] = [];

    for (const task of plan.subTasks) {
      const worker = task.assignedWorker;
      const profile = this.profileLoader.getProfile(worker);
      const category = this.inferCategory(task);

      // 1. 检查是否分配给了擅长该分类的 Worker
      if (!profile.preferences.preferredCategories.includes(category)) {
        issues.push({
          type: 'suboptimal_assignment',
          taskId: task.id,
          message: `任务分类 "${category}" 不在 ${worker} 的擅长领域`,
          suggestion: this.findBetterWorker(category),
        });
      }

      // 2. 检查任务是否涉及 Worker 的弱项
      const weaknessHit = profile.profile.weaknesses.find(w =>
        task.description.toLowerCase().includes(w.toLowerCase())
      );
      if (weaknessHit) {
        issues.push({
          type: 'weakness_match',
          taskId: task.id,
          message: `任务涉及 ${worker} 的弱项: "${weaknessHit}"`,
          reviewLevel: 'strict', // 需要更严格的评审
        });
      }
    }

    return { issues, approved: issues.filter(i => i.type === 'critical').length === 0 };
  }

  /**
   * 互检评审者选择：基于能力画像匹配
   */
  selectPeerReviewer(task: SubTask, executor: CLIType): CLIType {
    const category = this.inferCategory(task);
    const allProfiles = this.profileLoader.getAllProfiles();

    // 选择擅长该分类且不是执行者的 Worker
    const candidates = [...allProfiles.entries()]
      .filter(([cli]) => cli !== executor)
      .filter(([_, profile]) => profile.preferences.preferredCategories.includes(category))
      .sort((a, b) => {
        // 优先选择该分类是第一优先的 Worker
        const aIndex = a[1].preferences.preferredCategories.indexOf(category);
        const bIndex = b[1].preferences.preferredCategories.indexOf(category);
        return aIndex - bIndex;
      });

    return candidates[0]?.[0] || (executor === 'claude' ? 'codex' : 'claude');
  }

  /**
   * 评审严格度：基于分类风险 + Worker 弱项
   */
  determineReviewLevel(task: SubTask, executor: CLIType): ReviewLevel {
    const category = this.profileLoader.getCategory(this.inferCategory(task));
    const profile = this.profileLoader.getProfile(executor);

    // 基础严格度来自分类风险
    let level: ReviewLevel = category?.riskLevel === 'high' ? 'strict'
                           : category?.riskLevel === 'medium' ? 'standard'
                           : 'light';

    // 如果任务涉及 Worker 弱项，提升严格度
    const involvesWeakness = profile.profile.weaknesses.some(w =>
      task.description.toLowerCase().includes(w.toLowerCase())
    );
    if (involvesWeakness && level !== 'strict') {
      level = level === 'light' ? 'standard' : 'strict';
    }

    return level;
  }
}
```

### 3.2 增强 GuidanceInjector：评审引导注入

```typescript
// 扩展 GuidanceInjector，支持评审上下文
export class EnhancedGuidanceInjector extends GuidanceInjector {
  /**
   * 构建自检引导 Prompt
   */
  buildSelfCheckGuidance(profile: WorkerProfile, task: SubTask): string {
    const sections: string[] = [];

    // 1. 基于 Worker 弱项的重点检查
    if (profile.profile.weaknesses.length > 0) {
      sections.push(`## 重点自检（你的相对弱项）`);
      sections.push(`请特别检查以下方面，因为这些是你需要额外注意的领域：`);
      profile.profile.weaknesses.forEach(w => {
        sections.push(`- ${w}`);
      });
    }

    // 2. 基于协作规则的输出检查
    sections.push(`## 协作规范检查`);
    profile.collaboration.asCollaborator.forEach(rule => {
      sections.push(`- [ ] ${rule}`);
    });

    return sections.join('\n');
  }

  /**
   * 构建互检引导 Prompt
   */
  buildPeerReviewGuidance(
    reviewerProfile: WorkerProfile,
    executorProfile: WorkerProfile,
    task: SubTask
  ): string {
    const sections: string[] = [];

    // 1. 利用评审者的专长
    sections.push(`## 你的专长检查视角`);
    sections.push(`作为 ${reviewerProfile.name}，请重点从以下专长领域审查：`);
    reviewerProfile.profile.strengths.forEach(s => {
      sections.push(`- ${s}`);
    });

    // 2. 针对执行者弱项的检查
    sections.push(`\n## 执行者弱项重点审查`);
    sections.push(`执行者 ${executorProfile.name} 在以下方面相对较弱，请重点检查：`);
    executorProfile.profile.weaknesses.forEach(w => {
      sections.push(`- ${w}`);
    });

    return sections.join('\n');
  }
}
```

### 3.3 文件拆分方案（保持画像系统核心地位）

```
src/orchestrator/
├── profile/                         # ✅ 保持现有结构
│   ├── profile-loader.ts
│   ├── guidance-injector.ts         # ⚡ 扩展评审引导
│   └── types.ts
├── review/                          # 🆕 新增评审模块
│   ├── profile-aware-reviewer.ts    # 画像感知评审器
│   ├── review-policy.ts             # 评审策略（基于画像）
│   ├── review-recovery.ts           # 评审失败恢复
│   └── index.ts
├── policy-engine.ts                 # ⚡ 增强，集成评审策略
├── orchestrator-agent.ts            # ⚡ 精简，委托给子模块
└── worker-pool.ts                   # ✅ 保持
```

### 3.4 评审失败恢复（结合画像系统）

```typescript
// 画像感知的失败恢复
class ProfileAwareRecoveryHandler {
  constructor(
    private profileLoader: ProfileLoader,
    private recoveryHandler: RecoveryHandler
  ) {}

  async handleReviewFailure(
    task: SubTask,
    failure: ReviewFailure,
    executor: CLIType
  ): Promise<RecoveryAction> {
    const profile = this.profileLoader.getProfile(executor);
    const category = this.inferCategory(task);

    // 1. 检查失败是否与 Worker 弱项相关
    const isWeaknessRelated = profile.profile.weaknesses.some(w =>
      failure.message.toLowerCase().includes(w.toLowerCase()) ||
      task.description.toLowerCase().includes(w.toLowerCase())
    );

    if (isWeaknessRelated) {
      // 弱项相关失败：考虑换 Worker 重试
      const betterWorker = this.findWorkerWithStrength(category);
      if (betterWorker && betterWorker !== executor) {
        return {
          action: 'reassign',
          newWorker: betterWorker,
          reason: `任务涉及 ${executor} 的弱项，转交给更擅长的 ${betterWorker}`,
        };
      }
    }

    // 2. 非弱项相关失败：使用原有恢复逻辑
    return this.recoveryHandler.handleFailure(task, failure);
  }
}
```

---

## 四、实施路径

### 4.1 Phase 1: 评审机制画像集成（最高优先级）

**目标**: 让评审机制感知 Worker 画像

**具体改动**:

1. **新增 `ProfileAwareReviewer`** (约 200 行)
   - 计划评审时检查任务分配合理性
   - 基于 Worker 弱项决定评审严格度
   - 智能选择互检评审者

2. **扩展 `GuidanceInjector`** (约 50 行)
   - 添加 `buildSelfCheckGuidance()` 方法
   - 添加 `buildPeerReviewGuidance()` 方法

3. **修改 `OrchestratorAgent`** (精简约 300 行)
   - 评审逻辑委托给 `ProfileAwareReviewer`
   - 保持接口兼容

### 4.2 Phase 2: 评审配置扩展

**扩展 WorkerProfile 类型**:

```typescript
// types.ts 扩展
interface WorkerProfile {
  // ... 现有字段 ...

  // 🆕 评审相关配置
  review?: {
    /** 作为被评审者时需要重点检查的方面 */
    focusAreasWhenReviewed: string[];
    /** 作为评审者时的专长视角 */
    reviewStrengths: string[];
    /** 需要更严格评审的任务类型 */
    strictReviewCategories: string[];
  };
}
```

**扩展 CategoryConfig**:

```typescript
// types.ts 扩展
interface CategoryConfig {
  // ... 现有字段 ...

  // 🆕 评审策略
  reviewPolicy?: {
    /** 是否强制互检 */
    requirePeerReview: boolean;
    /** 推荐的评审 Worker */
    preferredReviewer?: CLIType;
    /** 评审重点 */
    reviewFocus: string[];
  };
}
```

### 4.3 渐进式实施计划

| 阶段 | 时间 | 内容 | 风险 |
|------|------|------|------|
| 1.1 | Week 1 | 创建 ProfileAwareReviewer 原型 | 低 |
| 1.2 | Week 2 | 集成到 OrchestratorAgent | 中 |
| 2.1 | Week 3 | 扩展 WorkerProfile 评审配置 | 低 |
| 2.2 | Week 4 | 扩展 CategoryConfig 评审策略 | 低 |
| 3.1 | Week 5-6 | 评审失败画像感知恢复 | 中 |

---

## 五、总结

### 5.1 核心改进点（结合项目特色）

| 改进项 | 当前问题 | 改进方案 | 与画像系统的关系 |
|--------|----------|----------|------------------|
| **评审者选择** | 简单排除后取第一个 | 基于 strengths 匹配 | 利用 preferredCategories |
| **评审严格度** | 仅基于文件扩展名 | 分类风险 + Worker 弱项 | 利用 riskLevel + weaknesses |
| **自检引导** | 通用 Prompt | 基于 Worker 弱项定制 | 利用 weaknesses |
| **互检引导** | 通用 Prompt | 利用评审者专长视角 | 利用 strengths |
| **失败恢复** | 简单重试 | 弱项相关则换 Worker | 利用 weaknesses + strengths |

### 5.2 不照搬 oh-my-opencode 的原因

| oh-my-opencode 设计 | 为什么不直接采用 | 本项目的替代方案 |
|---------------------|------------------|------------------|
| 固定 Agent 列表 (oracle/librarian/explore) | 我们有动态画像系统 | 基于 ProfileLoader 动态选择 |
| SUBAGENTS LIE 强制验证 | 太绝对，忽略 Worker 特性 | 基于 weaknesses 的针对性验证 |
| 委派 + 工具限制 | 与"引导而非限制"理念冲突 | 通过 Prompt 引导评审行为 |
| Hook 系统替代核心逻辑 | 画像系统才是我们的核心 | Hook 作为补充，不替代画像 |

### 5.3 下一步行动

1. **立即**: 创建 `ProfileAwareReviewer` 原型，验证画像驱动评审可行性
2. **短期**: 扩展 `GuidanceInjector` 支持评审引导注入
3. **中期**: 扩展画像配置，添加评审相关字段
4. **长期**: 考虑引入轻量 Hook 系统作为画像系统的补充
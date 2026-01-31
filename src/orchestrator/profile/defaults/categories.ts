/**
 * Worker Profile System - 默认任务分类配置
 */

import { CategoriesConfig } from '../types';

export const DEFAULT_CATEGORIES_CONFIG: CategoriesConfig = {
  version: '1.0',

  categories: {
    // 架构设计类
    architecture: {
      displayName: '架构设计',
      description: '系统架构、模块设计、接口定义',
      keywords: [
        '架构|设计|模块|重构',
        '接口|契约|API 设计',
        '拆分|解耦|抽象',
      ],
      defaultWorker: 'claude',
      priority: 'high',
      riskLevel: 'high',
    },

    // 后端开发类
    backend: {
      displayName: '后端开发',
      description: 'API 实现、数据库、服务端逻辑',
      keywords: [
        '后端|API|服务|接口实现',
        '数据库|SQL|ORM',
        '鉴权|认证|授权',
      ],
      defaultWorker: 'claude',
      priority: 'medium',
      riskLevel: 'medium',
    },

    // 前端开发类
    frontend: {
      displayName: '前端开发',
      description: 'UI 组件、页面、样式、交互',
      keywords: [
        '前端|UI|组件|页面',
        '样式|CSS|布局',
        '交互|动画|响应式',
      ],
      defaultWorker: 'gemini',
      priority: 'medium',
      riskLevel: 'low',
    },

    // 数据分析类
    data_analysis: {
      displayName: '数据分析',
      description: '数据处理、脚本、统计、可视化',
      keywords: [
        '数据|分析|统计|可视化',
        '脚本|ETL|清洗',
        '报表|指标|图表',
      ],
      defaultWorker: 'codex',
      priority: 'medium',
      riskLevel: 'low',
    },

    // 功能实现类
    implement: {
      displayName: '功能实现',
      description: '实现新功能、编写业务逻辑',
      keywords: [
        '实现|开发|编写',
        '功能|特性|feature',
        '业务逻辑|逻辑实现',
      ],
      defaultWorker: 'codex',
      priority: 'medium',
      riskLevel: 'medium',
    },

    // 代码重构类
    refactor: {
      displayName: '代码重构',
      description: '优化代码结构、提升可维护性',
      keywords: [
        '重构|优化|改进',
        '提取|抽象|简化',
        '可维护性|可读性',
      ],
      defaultWorker: 'claude',
      priority: 'medium',
      riskLevel: 'medium',
    },

    // Bug 修复类
    bugfix: {
      displayName: 'Bug 修复',
      description: '问题修复、错误处理',
      keywords: [
        '修复|bug|fix|错误',
        '问题|异常|崩溃',
      ],
      defaultWorker: 'codex',
      priority: 'high',
      riskLevel: 'medium',
    },

    // 问题排查类
    debug: {
      displayName: '问题排查',
      description: '调试、问题定位、日志分析',
      keywords: [
        '调试|debug|排查',
        '定位|分析|追踪',
        '日志|堆栈|错误信息',
      ],
      defaultWorker: 'claude',
      priority: 'high',
      riskLevel: 'low',
    },

    // 测试类
    test: {
      displayName: '测试编写',
      description: '单元测试、集成测试',
      keywords: [
        '测试|test|单元测试',
        '覆盖率|mock|断言',
      ],
      defaultWorker: 'codex',
      priority: 'medium',
      riskLevel: 'low',
    },

    // 文档类 (注意：类型定义使用 'document'，但这里保持 'docs' 作为内部标识)
    document: {
      displayName: '文档编写',
      description: 'README、注释、API 文档',
      keywords: [
        '文档|README|注释',
        '说明|指南|教程',
      ],
      defaultWorker: 'gemini',
      priority: 'low',
      riskLevel: 'low',
    },

    // 代码审查类
    review: {
      displayName: '代码审查',
      description: '代码审查、质量检查',
      keywords: [
        '审查|review|检查',
        '质量|规范|最佳实践',
      ],
      defaultWorker: 'claude',
      priority: 'medium',
      riskLevel: 'low',
    },

    // 通用任务类
    general: {
      displayName: '通用任务',
      description: '其他未分类任务',
      keywords: [
        '通用|其他|杂项',
      ],
      defaultWorker: 'claude',
      priority: 'low',
      riskLevel: 'low',
    },

    // 集成联调类
    integration: {
      displayName: '集成联调',
      description: '跨模块集成、接口对接',
      keywords: [
        '集成|联调|对接',
        '跨模块|跨端',
      ],
      defaultWorker: 'claude',
      priority: 'high',
      riskLevel: 'high',
    },

    // 简单任务类
    simple: {
      displayName: '简单任务',
      description: '小修改、格式调整',
      keywords: [
        '简单|快速|小改',
        '格式|命名|注释',
      ],
      defaultWorker: 'codex',
      priority: 'low',
      riskLevel: 'low',
    },
  },

  rules: {
    categoryPriority: [
      'architecture',
      'debug',
      'bugfix',
      'refactor',
      'data_analysis',
      'backend',
      'frontend',
      'implement',
      'test',
      'review',
      'document',
      'general',
    ],
    defaultCategory: 'general',
    riskMapping: {
      high: 'fullPath',
      medium: 'standardPath',
      low: 'lightPath',
    },
  },
};

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

    // Bug 修复类
    bugfix: {
      displayName: 'Bug 修复',
      description: '问题修复、错误处理',
      keywords: [
        '修复|bug|fix|错误',
        '问题|异常|崩溃',
        '调试|排查',
      ],
      defaultWorker: 'codex',
      priority: 'high',
      riskLevel: 'medium',
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

    // 文档类
    docs: {
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
      'integration',
      'bugfix',
      'backend',
      'frontend',
      'test',
      'docs',
      'simple',
    ],
    defaultCategory: 'simple',
    riskMapping: {
      high: 'fullPath',
      medium: 'standardPath',
      low: 'lightPath',
    },
  },
};


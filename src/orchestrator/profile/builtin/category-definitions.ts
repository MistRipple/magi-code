/**
 * Category definitions (built-in, non-configurable)
 */

import { CategoryDefinition } from '../types';

export const CATEGORY_DEFINITIONS: Record<string, CategoryDefinition> = {
  architecture: {
    displayName: 'Architecture Design',
    description: 'System architecture, module design, interface definition',
    keywords: [
      '架构|设计|模块|重构',
      '接口|契约|API 设计',
      '拆分|解耦|抽象',
    ],
    guidance: {
      focus: ['Scalability and module decoupling', 'Interface contract design', 'Impact scope analysis'],
      constraints: ['Avoid over-engineering', 'Confirm before large-scale modifications'],
    },
    priority: 'high',
    riskLevel: 'high',
  },

  backend: {
    displayName: 'Backend Development',
    description: 'API implementation, database, server-side logic',
    keywords: [
      '后端|API|服务|接口实现',
      '数据库|SQL|ORM',
      '鉴权|认证|授权',
    ],
    guidance: {
      focus: ['API design standards', 'Security considerations', 'Error handling'],
      constraints: ['Follow RESTful conventions', 'Ensure proper data validation'],
    },
    priority: 'medium',
    riskLevel: 'medium',
  },

  frontend: {
    displayName: 'Frontend Development',
    description: 'UI components, pages, styles, interactions',
    keywords: [
      '前端|UI|组件|页面',
      '样式|CSS|布局',
      '交互|动画|响应式',
    ],
    guidance: {
      focus: ['User experience', 'Interaction details', 'Responsive design'],
      constraints: ['Maintain design consistency', 'Ensure accessibility'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  data_analysis: {
    displayName: 'Data Analysis',
    description: 'Data processing, scripting, statistics, visualization',
    keywords: [
      '数据分析|统计分析|指标分析|报表分析|analytics',
      '可视化|报表|指标|图表|dashboard',
      'etl|数据清洗|清洗脚本',
    ],
    guidance: {
      focus: ['Data accuracy', 'Clear visualizations', 'Well-defined conclusions'],
      constraints: ['Respect data privacy', 'Annotate data sources'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  implement: {
    displayName: 'Feature Implementation',
    description: 'Implement new features, write business logic',
    keywords: [
      '实现|开发|编写',
      '功能|特性|feature',
      '业务逻辑|逻辑实现',
    ],
    guidance: {
      focus: ['Requirement comprehension', 'Code conventions', 'Edge case handling'],
      constraints: ['Avoid over-engineering', 'Keep it simple'],
    },
    priority: 'medium',
    riskLevel: 'medium',
  },

  refactor: {
    displayName: 'Code Refactoring',
    description: 'Optimize code structure, improve maintainability',
    keywords: [
      '重构|优化|改进',
      '提取|抽象|简化',
      '可维护性|可读性',
    ],
    guidance: {
      focus: ['Maintainability improvement', 'Code reuse', 'Eliminate duplication'],
      constraints: ['Preserve existing behavior', 'Proceed incrementally'],
    },
    priority: 'medium',
    riskLevel: 'medium',
  },

  bugfix: {
    displayName: 'Bug Fix',
    description: 'Fix defects, handle errors',
    keywords: [
      '修复|bug|fix|错误',
      '问题|异常|崩溃',
    ],
    guidance: {
      focus: ['Pinpoint the root cause', 'Minimize change scope', 'Prevent regressions'],
      constraints: ['Do not expand the change scope', 'Preserve existing logic'],
    },
    priority: 'high',
    riskLevel: 'medium',
  },

  debug: {
    displayName: 'Debugging',
    description: 'Debug, problem investigation, log analysis',
    keywords: [
      '调试|debug|排查',
      '定位|分析|追踪',
      '日志|堆栈|错误信息',
    ],
    guidance: {
      focus: ['Root cause analysis', 'Reproduction path', 'Log analysis'],
      constraints: ['Do not rush into changes', 'Understand first, act second'],
    },
    priority: 'high',
    riskLevel: 'low',
  },

  test: {
    displayName: 'Test Writing',
    description: 'Unit tests, integration tests',
    keywords: [
      '测试|test|单元测试',
      '覆盖率|mock|断言',
    ],
    guidance: {
      focus: ['Edge cases', 'Error paths', 'Test maintainability'],
      constraints: ['Test independence', 'Avoid excessive mocking'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  review: {
    displayName: 'Code Review',
    description: 'Code review, quality inspection',
    keywords: [
      '审查|review|检查',
      '质量|规范|最佳实践',
    ],
    guidance: {
      focus: ['Code quality', 'Potential issues', 'Best practices'],
      constraints: ['Provide constructive feedback', 'Distinguish severity levels'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  document: {
    displayName: 'Documentation',
    description: 'README, comments, API documentation',
    keywords: [
      '文档|README|注释',
      '说明|指南|教程',
    ],
    guidance: {
      focus: ['Clarity and readability', 'Complete examples', 'Keep up to date'],
      constraints: ['Avoid redundancy', 'Write for the target audience'],
    },
    priority: 'low',
    riskLevel: 'low',
  },

  integration: {
    displayName: 'Integration',
    description: 'Cross-module integration, interface alignment',
    keywords: [
      '集成|联调|对接',
      '跨模块|跨端',
    ],
    guidance: {
      focus: ['Interface consistency', 'Error handling', 'Integration testing'],
      constraints: ['Confirm interface contracts', 'Prepare rollback plans'],
    },
    priority: 'high',
    riskLevel: 'high',
  },

  simple: {
    displayName: 'Simple Task',
    description: 'Minor changes, formatting adjustments',
    keywords: [
      '简单|快速|小改',
      '格式|命名|注释',
    ],
    guidance: {
      focus: ['Complete quickly', 'Maintain consistency'],
      constraints: ['Do not expand scope'],
    },
    priority: 'low',
    riskLevel: 'low',
  },

  general: {
    displayName: 'General Task',
    description: 'Other uncategorized tasks',
    keywords: [
      '通用|其他|杂项',
    ],
    guidance: {
      focus: ['Understand requirements', 'Implement reasonably'],
      constraints: ['Confirm when uncertain'],
    },
    priority: 'low',
    riskLevel: 'low',
  },
};

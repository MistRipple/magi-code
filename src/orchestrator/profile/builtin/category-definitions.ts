/**
 * Category definitions (built-in, non-configurable)
 */

import { CategoryDefinition } from '../types';

export const CATEGORY_DEFINITIONS: Record<string, CategoryDefinition> = {
  architecture: {
    displayName: '架构设计',
    description: '系统架构、模块设计、接口定义',
    keywords: [
      '架构|设计|模块|重构',
      '接口|契约|API 设计',
      '拆分|解耦|抽象',
    ],
    guidance: {
      focus: ['可扩展性和模块解耦', '接口契约设计', '影响范围分析'],
      constraints: ['避免过度设计', '大规模修改前先确认'],
    },
    priority: 'high',
    riskLevel: 'high',
  },

  backend: {
    displayName: '后端开发',
    description: 'API 实现、数据库、服务端逻辑',
    keywords: [
      '后端|API|服务|接口实现',
      '数据库|SQL|ORM',
      '鉴权|认证|授权',
    ],
    guidance: {
      focus: ['API 设计规范', '安全性考虑', '错误处理'],
      constraints: ['遵循 RESTful 规范', '注意数据验证'],
    },
    priority: 'medium',
    riskLevel: 'medium',
  },

  frontend: {
    displayName: '前端开发',
    description: 'UI 组件、页面、样式、交互',
    keywords: [
      '前端|UI|组件|页面',
      '样式|CSS|布局',
      '交互|动画|响应式',
    ],
    guidance: {
      focus: ['用户体验', '交互细节', '响应式设计'],
      constraints: ['保持设计一致性', '注意可访问性'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  data_analysis: {
    displayName: '数据分析',
    description: '数据处理、脚本、统计、可视化',
    keywords: [
      '数据|分析|统计|可视化',
      '脚本|ETL|清洗',
      '报表|指标|图表',
    ],
    guidance: {
      focus: ['数据准确性', '可视化清晰', '结论明确'],
      constraints: ['注意数据隐私', '标注数据来源'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  implement: {
    displayName: '功能实现',
    description: '实现新功能、编写业务逻辑',
    keywords: [
      '实现|开发|编写',
      '功能|特性|feature',
      '业务逻辑|逻辑实现',
    ],
    guidance: {
      focus: ['需求理解', '代码规范', '边界处理'],
      constraints: ['不过度设计', '保持简洁'],
    },
    priority: 'medium',
    riskLevel: 'medium',
  },

  refactor: {
    displayName: '代码重构',
    description: '优化代码结构、提升可维护性',
    keywords: [
      '重构|优化|改进',
      '提取|抽象|简化',
      '可维护性|可读性',
    ],
    guidance: {
      focus: ['可维护性提升', '代码复用', '消除重复'],
      constraints: ['保持功能不变', '分步进行'],
    },
    priority: 'medium',
    riskLevel: 'medium',
  },

  bugfix: {
    displayName: '缺陷修复',
    description: '问题修复、错误处理',
    keywords: [
      '修复|bug|fix|错误',
      '问题|异常|崩溃',
    ],
    guidance: {
      focus: ['精准定位问题', '最小化修改范围', '防止回归'],
      constraints: ['不扩大改动范围', '保持原有逻辑'],
    },
    priority: 'high',
    riskLevel: 'medium',
  },

  debug: {
    displayName: '问题排查',
    description: '调试、问题定位、日志分析',
    keywords: [
      '调试|debug|排查',
      '定位|分析|追踪',
      '日志|堆栈|错误信息',
    ],
    guidance: {
      focus: ['根因分析', '复现路径', '日志分析'],
      constraints: ['不急于修改', '先理解后行动'],
    },
    priority: 'high',
    riskLevel: 'low',
  },

  test: {
    displayName: '测试编写',
    description: '单元测试、集成测试',
    keywords: [
      '测试|test|单元测试',
      '覆盖率|mock|断言',
    ],
    guidance: {
      focus: ['边界场景', '异常路径', '测试可维护性'],
      constraints: ['测试独立性', '避免过度 mock'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  review: {
    displayName: '代码审查',
    description: '代码审查、质量检查',
    keywords: [
      '审查|review|检查',
      '质量|规范|最佳实践',
    ],
    guidance: {
      focus: ['代码质量', '潜在问题', '最佳实践'],
      constraints: ['建设性反馈', '区分严重程度'],
    },
    priority: 'medium',
    riskLevel: 'low',
  },

  document: {
    displayName: '文档编写',
    description: 'README、注释、API 文档',
    keywords: [
      '文档|README|注释',
      '说明|指南|教程',
    ],
    guidance: {
      focus: ['清晰易懂', '示例完整', '保持更新'],
      constraints: ['避免冗余', '面向目标读者'],
    },
    priority: 'low',
    riskLevel: 'low',
  },

  integration: {
    displayName: '集成联调',
    description: '跨模块集成、接口对接',
    keywords: [
      '集成|联调|对接',
      '跨模块|跨端',
    ],
    guidance: {
      focus: ['接口一致性', '错误处理', '联调测试'],
      constraints: ['确认接口契约', '做好回滚准备'],
    },
    priority: 'high',
    riskLevel: 'high',
  },

  simple: {
    displayName: '简单任务',
    description: '小修改、格式调整',
    keywords: [
      '简单|快速|小改',
      '格式|命名|注释',
    ],
    guidance: {
      focus: ['快速完成', '保持一致'],
      constraints: ['不扩大范围'],
    },
    priority: 'low',
    riskLevel: 'low',
  },

  general: {
    displayName: '通用任务',
    description: '其他未分类任务',
    keywords: [
      '通用|其他|杂项',
    ],
    guidance: {
      focus: ['理解需求', '合理实现'],
      constraints: ['不确定时先确认'],
    },
    priority: 'low',
    riskLevel: 'low',
  },
};

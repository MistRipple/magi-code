/**
 * Domain detector
 *
 * 职责：
 * - 从任务文本中识别 ownership 相关职责域信号
 * - 区分“可作为更具体分类的职责域”与“必须先拆 Assignment 的跨域边界”
 */

import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';

export const OWNERSHIP_DOMAIN_CATEGORIES = [
  'frontend',
  'backend',
  'integration',
  'test',
  'document',
  'data_analysis',
] as const;

export const ASSIGNMENT_OWNERSHIP_BOUNDARY_CATEGORIES = [
  'frontend',
  'backend',
  'integration',
] as const;

export type OwnershipDomain = typeof OWNERSHIP_DOMAIN_CATEGORIES[number];

export interface OwnershipDomainDetection {
  matchedDomains: OwnershipDomain[];
  splitBoundaryDomains: OwnershipDomain[];
}

interface CompiledDomainMatcher {
  domain: OwnershipDomain;
  patterns: RegExp[];
}

const NEGATED_DOMAIN_ALIASES: Record<OwnershipDomain, string[]> = {
  frontend: ['前端', '页面', 'ui', '组件', '样式', 'css', '布局', '交互'],
  backend: ['后端', 'api', '服务', '接口', '数据库', 'sql', 'orm', '鉴权', '认证', '授权'],
  integration: ['集成', '联调', '对接', '跨模块', '跨端'],
  test: ['测试', 'test', '单元测试', 'mock', '断言', '覆盖率'],
  document: ['文档', 'readme', '注释', '说明', '指南', '教程'],
  data_analysis: ['数据', '分析', '统计', '可视化', '报表', '指标', '图表', 'etl', '清洗'],
};

const NEGATION_PREFIX_PATTERN = '(?:不涉及|无需|无须|不需要|不用|不必|不改|不修改|不做|不处理|跳过|排除)';
const NEGATION_GAP_PATTERN = '[^\\n，。,；;:：]{0,8}';

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function stripNegatedDomainHints(text: string): string {
  let sanitized = text;

  for (const aliases of Object.values(NEGATED_DOMAIN_ALIASES)) {
    for (const alias of aliases) {
      const escapedAlias = escapeRegExp(alias);
      sanitized = sanitized
        .replace(new RegExp(`${NEGATION_PREFIX_PATTERN}${NEGATION_GAP_PATTERN}${escapedAlias}${NEGATION_GAP_PATTERN}`, 'ig'), ' ')
        .replace(new RegExp(`${escapedAlias}${NEGATION_GAP_PATTERN}${NEGATION_PREFIX_PATTERN}${NEGATION_GAP_PATTERN}`, 'ig'), ' ');
    }
  }

  return sanitized;
}

function compileDomainMatchers(): CompiledDomainMatcher[] {
  return OWNERSHIP_DOMAIN_CATEGORIES.map((domain) => {
    const definition = CATEGORY_DEFINITIONS[domain];
    if (!definition) {
      throw new Error(`ownership detector 引用了不存在的分类: ${domain}`);
    }
    if (!definition.keywords || definition.keywords.length === 0) {
      throw new Error(`ownership detector 分类关键词为空: ${domain}`);
    }

    return {
      domain,
      patterns: definition.keywords.map((pattern) => {
        try {
          return new RegExp(pattern, 'i');
        } catch (error) {
          throw new Error(`ownership detector 分类 ${domain} 的关键词正则编译失败: "${pattern}" — ${error instanceof Error ? error.message : String(error)}`);
        }
      }),
    };
  });
}

const COMPILED_DOMAIN_MATCHERS = compileDomainMatchers();
const SPLIT_BOUNDARY_DOMAIN_SET = new Set<OwnershipDomain>(ASSIGNMENT_OWNERSHIP_BOUNDARY_CATEGORIES);

export class DomainDetector {
  detectFromTextParts(parts: string[]): OwnershipDomainDetection {
    const text = parts
      .filter((part): part is string => typeof part === 'string' && part.trim().length > 0)
      .join('\n')
      .toLowerCase();
    const sanitizedText = stripNegatedDomainHints(text);

    if (!sanitizedText) {
      return {
        matchedDomains: [],
        splitBoundaryDomains: [],
      };
    }

    const matchedDomains: OwnershipDomain[] = [];
    for (const { domain, patterns } of COMPILED_DOMAIN_MATCHERS) {
      const matched = patterns.some((regex) => regex.test(sanitizedText));
      if (matched) {
        matchedDomains.push(domain);
      }
    }

    return {
      matchedDomains,
      splitBoundaryDomains: matchedDomains.filter((domain) => SPLIT_BOUNDARY_DOMAIN_SET.has(domain)),
    };
  }
}

/**
 * Category resolver (single algorithm)
 *
 * @deprecated 将在 Phase 5 移除。ownership 推断已迁移到 AssignmentCompiler + DomainDetector。
 * 现有调用点（assignment-manager, policy-engine）应在过渡期后改用 AssignmentCompiler.compile()。
 *
 * 设计：
 * - 启动时预编译所有关键词正则，编译失败则立即报错（不做运行时回退）
 * - resolveFromText 是唯一的公开 API
 */

import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';
import { CATEGORY_RULES } from './builtin/category-rules';

/** 预编译的分类匹配器：按优先级排列 */
interface CompiledCategoryMatcher {
  category: string;
  patterns: RegExp[];
}

/** 按 categoryPriority 顺序预编译所有关键词正则 */
function compileMatchers(): CompiledCategoryMatcher[] {
  const matchers: CompiledCategoryMatcher[] = [];

  for (const category of CATEGORY_RULES.categoryPriority) {
    const definition = CATEGORY_DEFINITIONS[category];
    if (!definition) {
      throw new Error(`分类规则引用不存在的分类: ${category}`);
    }
    if (!definition.keywords || definition.keywords.length === 0) {
      throw new Error(`分类关键词为空: ${category}`);
    }

    const patterns = definition.keywords.map(pattern => {
      try {
        return new RegExp(pattern, 'i');
      } catch (e) {
        throw new Error(`分类 ${category} 的关键词正则编译失败: "${pattern}" — ${e instanceof Error ? e.message : String(e)}`);
      }
    });

    matchers.push({ category, patterns });
  }

  return matchers;
}

// 模块加载时预编译，编译失败则阻止启动
const COMPILED_MATCHERS = compileMatchers();

export class CategoryResolver {
  resolveFromText(text: string): string {
    const normalized = text.toLowerCase();

    for (const { category, patterns } of COMPILED_MATCHERS) {
      for (const regex of patterns) {
        if (regex.test(normalized)) {
          return category;
        }
      }
    }

    return CATEGORY_RULES.defaultCategory;
  }
}

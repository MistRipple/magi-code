/**
 * Category resolver (single algorithm)
 */

import { CATEGORY_DEFINITIONS } from './builtin/category-definitions';
import { CATEGORY_RULES } from './builtin/category-rules';

export class CategoryResolver {
  resolveFromText(text: string): string {
    const normalized = text.toLowerCase();

    for (const category of CATEGORY_RULES.categoryPriority) {
      const definition = CATEGORY_DEFINITIONS[category];
      if (!definition) {
        throw new Error(`分类规则引用不存在的分类: ${category}`);
      }

      if (!definition.keywords || definition.keywords.length === 0) {
        throw new Error(`分类关键词为空: ${category}`);
      }

      for (const pattern of definition.keywords) {
        try {
          const regex = new RegExp(pattern, 'i');
          if (regex.test(normalized)) {
            return category;
          }
        } catch {
          if (normalized.includes(pattern.toLowerCase())) {
            return category;
          }
        }
      }
    }

    return CATEGORY_RULES.defaultCategory;
  }

  /**
   * 解析所有匹配的分类（用于多 Worker 协作场景）
   */
  resolveAllFromText(text: string): string[] {
    const normalized = text.toLowerCase();
    const matchedCategories: string[] = [];

    for (const category of CATEGORY_RULES.categoryPriority) {
      const definition = CATEGORY_DEFINITIONS[category];
      if (!definition || !definition.keywords || definition.keywords.length === 0) {
        continue;
      }

      for (const pattern of definition.keywords) {
        let matched = false;
        try {
          const regex = new RegExp(pattern, 'i');
          if (regex.test(normalized)) {
            matched = true;
          }
        } catch {
          if (normalized.includes(pattern.toLowerCase())) {
            matched = true;
          }
        }
        if (matched && !matchedCategories.includes(category)) {
          matchedCategories.push(category);
          break; // 已匹配此分类，继续下一个分类
        }
      }
    }

    // 如果没有匹配任何分类，返回默认分类
    if (matchedCategories.length === 0) {
      return [CATEGORY_RULES.defaultCategory];
    }

    return matchedCategories;
  }
}

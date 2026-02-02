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
}

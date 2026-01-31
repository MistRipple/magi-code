import type { TaskCategory } from '../../types';
import type { CategoryConfig } from './types';
import type { ProfileLoader } from './profile-loader';

export interface CategoryMatchResult {
  category: TaskCategory;
  matchedKeywords: string[];
  categoryConfig: CategoryConfig;
  matchedCategories: TaskCategory[];
}

/**
 * 检查用户请求是否涉及代码操作
 *
 * 判断依据：是否匹配 categories.ts 中定义的任务分类关键词
 * - 匹配任何任务分类关键词 → 涉及代码操作 → 需要 Worker 执行
 * - 不匹配任何关键词 → 不涉及代码操作 → 编排者直接回答
 *
 * @param profileLoader ProfileLoader 实例
 * @param prompt 用户请求
 * @returns 是否涉及代码操作
 */
export function hasCodeOperationIntent(
  profileLoader: ProfileLoader,
  prompt: string
): boolean {
  const categories = profileLoader.getAllCategories();
  const rules = profileLoader.getCategoryRules();

  if (!rules?.categoryPriority?.length) {
    return false;
  }

  const lowerPrompt = prompt.toLowerCase();

  // 遍历所有任务分类，检查是否匹配任何关键词
  for (const categoryName of rules.categoryPriority) {
    const config = categories.get(categoryName);
    if (!config) {
      continue;
    }

    for (const pattern of config.keywords) {
      try {
        const regex = new RegExp(pattern, 'i');
        if (regex.test(lowerPrompt)) {
          return true; // 匹配到任务分类关键词，涉及代码操作
        }
      } catch {
        if (lowerPrompt.includes(pattern.toLowerCase())) {
          return true;
        }
      }
    }
  }

  return false; // 不匹配任何关键词，不涉及代码操作
}

export function matchCategoryWithProfile(
  profileLoader: ProfileLoader,
  prompt: string
): CategoryMatchResult {
  const categories = profileLoader.getAllCategories();
  const rules = profileLoader.getCategoryRules();

  if (!rules?.categoryPriority?.length) {
    throw new Error('任务分类规则缺失：categoryPriority 未配置');
  }
  if (!rules.defaultCategory) {
    throw new Error('任务分类规则缺失：defaultCategory 未配置');
  }

  const lowerPrompt = prompt.toLowerCase();
  const matchedCategories: TaskCategory[] = [];
  const missingCategories: string[] = [];

  let bestMatch: { category: string; score: number; keywords: string[]; config: CategoryConfig } | null = null;

  for (const categoryName of rules.categoryPriority) {
    const config = categories.get(categoryName);
    if (!config) {
      missingCategories.push(categoryName);
      continue;
    }

    let score = 0;
    const matched: string[] = [];

    for (const pattern of config.keywords) {
      try {
        const regex = new RegExp(pattern, 'i');
        if (regex.test(lowerPrompt)) {
          score += 10;
          matched.push(pattern);
        }
      } catch {
        if (lowerPrompt.includes(pattern.toLowerCase())) {
          score += 5;
          matched.push(pattern);
        }
      }
    }

    if (score > 0) {
      matchedCategories.push(categoryName as TaskCategory);
      if (!bestMatch || score > bestMatch.score) {
        bestMatch = { category: categoryName, score, keywords: matched, config };
      }
    }
  }

  if (missingCategories.length) {
    throw new Error(`任务分类配置缺失: ${missingCategories.join(', ')}`);
  }

  if (bestMatch) {
    return {
      category: bestMatch.category as TaskCategory,
      matchedKeywords: bestMatch.keywords,
      categoryConfig: bestMatch.config,
      matchedCategories,
    };
  }

  const defaultCategory = rules.defaultCategory as TaskCategory;
  const defaultConfig = categories.get(defaultCategory);
  if (!defaultConfig) {
    throw new Error(`默认任务分类未配置: ${defaultCategory}`);
  }

  return {
    category: defaultCategory,
    matchedKeywords: [],
    categoryConfig: defaultConfig,
    matchedCategories: [defaultCategory],
  };
}

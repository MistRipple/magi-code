/**
 * Category rules (built-in, non-configurable)
 */

import { CategoryRules } from '../types';

export const CATEGORY_RULES: CategoryRules = {
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
    'integration',
    'simple',
    'general',
  ],
  defaultCategory: 'general',
  riskMapping: { high: 'fullPath', medium: 'standardPath', low: 'lightPath' },
};

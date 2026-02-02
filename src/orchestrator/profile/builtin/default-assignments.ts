/**
 * Default worker assignments (used when config file is missing)
 */

import { WorkerAssignments } from '../types';

export const WORKER_ASSIGNMENTS_VERSION = '2.0';

export const DEFAULT_ASSIGNMENTS: WorkerAssignments['assignments'] = {
  claude: [
    'architecture',
    'refactor',
    'backend',
    'review',
    'debug',
    'integration',
    'general',
  ],
  codex: [
    'bugfix',
    'implement',
    'test',
    'simple',
    'data_analysis',
  ],
  gemini: [
    'frontend',
    'document',
  ],
};

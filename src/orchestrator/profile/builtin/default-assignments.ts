/**
 * Default worker assignments (used when config file is missing)
 */

import { WorkerAssignments } from '../types';

export const WORKER_ASSIGNMENTS_VERSION = '2.0';

export const DEFAULT_ASSIGNMENTS: WorkerAssignments['assignments'] = {
  claude: [
    'architecture',
    'refactor',
    'review',
    'debug',
    'integration',
  ],
  codex: [
    'backend',
    'bugfix',
    'implement',
    'test',
    'simple',
    'general',
  ],
  gemini: [
    'frontend',
    'document',
    'data_analysis',
  ],
};

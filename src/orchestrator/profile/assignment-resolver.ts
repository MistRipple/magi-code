/**
 * Assignment resolver (single source of truth)
 */

import { WorkerSlot } from '../../types/agent-types';
import { WorkerAssignmentLoader } from './worker-assignments';

export class AssignmentResolver {
  constructor(private assignmentLoader: WorkerAssignmentLoader) {}

  resolveWorker(category: string): WorkerSlot {
    return this.assignmentLoader.getWorkerForCategory(category);
  }
}

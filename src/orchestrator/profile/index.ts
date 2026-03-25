/**
 * Worker Profile System - module exports
 */

export * from './types';

export { WORKER_PERSONAS, CATEGORY_DEFINITIONS, CATEGORY_RULES, DEFAULT_ASSIGNMENTS, WORKER_ASSIGNMENTS_VERSION } from './builtin';

export { ProfileLoader } from './profile-loader';
export { GuidanceInjector } from './guidance-injector';
export { PromptBuilder } from './prompt-builder';
export { CategoryResolver } from './category-resolver';
export {
  DomainDetector,
  OWNERSHIP_DOMAIN_CATEGORIES,
  ASSIGNMENT_OWNERSHIP_BOUNDARY_CATEGORIES,
} from './domain-detector';
export {
  OwnershipGuard,
  FALLBACK_ROUTING_CATEGORIES,
  TODO_SPLIT_STRICT_OWNERSHIP_CATEGORIES,
  type DispatchOwnershipAdvisory,
  type DispatchOwnershipAdvisoryInput,
  type SplitTodoOwnershipInput,
} from './ownership-guard';
export { AssignmentResolver } from './assignment-resolver';
export { WorkerAssignmentLoader, WorkerAssignmentStorage } from './worker-assignments';

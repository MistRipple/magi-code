/**
 * Shell 抽象层入口
 */

export type {
  IShellExecutor,
  ProcessRecord,
  ShellProcessEvents,
  ShellEventListener,
} from './types';

export { NodeShellExecutor } from './node-shell-executor';

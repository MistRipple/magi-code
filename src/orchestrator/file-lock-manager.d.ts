import { EventEmitter } from 'events';
export declare class FileLockManager extends EventEmitter {
    acquire(files: string[], priority?: number, abortSignal?: AbortSignal): Promise<() => void>;
    canAcquire(files: string[]): boolean;
    waitForRelease(): Promise<void>;
}

"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.FileLockManager = void 0;
const events_1 = require("events");
const DEFAULT_PRIORITY = 5;
const STARVATION_BOOST_MS = 15000;
class FileLockManager extends events_1.EventEmitter {
    locks = new Map();
    queue = [];
    requestCounter = 0;
    async acquire(files, priority, abortSignal) {
        const normalized = this.normalizeFiles(files);
        if (normalized.length === 0) {
            return () => undefined;
        }
        if (abortSignal?.aborted) {
            const reason = abortSignal.reason;
            throw (reason instanceof Error ? reason : new Error('任务已取消'));
        }
        return new Promise((resolve, reject) => {
            const request = {
                id: String(++this.requestCounter),
                files: normalized,
                priority: priority ?? DEFAULT_PRIORITY,
                enqueuedAt: Date.now(),
                resolve,
                reject,
                abortSignal,
            };
            if (this.tryAcquire(request)) {
                resolve(() => this.release(request));
                return;
            }
            if (abortSignal) {
                const onAbort = () => {
                    this.removeRequest(request.id);
                    const reason = abortSignal.reason;
                    reject(reason instanceof Error ? reason : new Error('任务已取消'));
                };
                request.abortHandler = onAbort;
                abortSignal.addEventListener('abort', onAbort, { once: true });
            }
            this.queue.push(request);
        });
    }
    canAcquire(files) {
        const normalized = this.normalizeFiles(files);
        return normalized.every(file => !this.locks.has(file));
    }
    waitForRelease() {
        return new Promise(resolve => this.once('lock_released', resolve));
    }
    tryAcquire(request) {
        for (const file of request.files) {
            if (this.locks.has(file)) {
                return false;
            }
        }
        for (const file of request.files) {
            this.locks.set(file, request.id);
        }
        return true;
    }
    release(request) {
        for (const file of request.files) {
            const owner = this.locks.get(file);
            if (owner === request.id) {
                this.locks.delete(file);
            }
        }
        this.emit('lock_released');
        this.processQueue();
    }
    processQueue() {
        let progressed = true;
        while (progressed) {
            progressed = false;
            const index = this.findNextRequestIndex();
            if (index === -1) {
                return;
            }
            const request = this.queue.splice(index, 1)[0];
            if (request.abortSignal?.aborted) {
                const reason = request.abortSignal.reason;
                request.reject(reason instanceof Error ? reason : new Error('任务已取消'));
                progressed = true;
                continue;
            }
            if (!this.tryAcquire(request)) {
                this.queue.push(request);
                return;
            }
            if (request.abortHandler && request.abortSignal) {
                request.abortSignal.removeEventListener('abort', request.abortHandler);
            }
            request.resolve(() => this.release(request));
            progressed = true;
        }
    }
    findNextRequestIndex() {
        if (this.queue.length === 0) {
            return -1;
        }
        const now = Date.now();
        let bestIndex = -1;
        let bestPriority = Number.POSITIVE_INFINITY;
        let bestEnqueuedAt = Number.POSITIVE_INFINITY;
        for (let i = 0; i < this.queue.length; i++) {
            const request = this.queue[i];
            if (!this.canAcquire(request.files)) {
                continue;
            }
            const effectivePriority = this.computeEffectivePriority(request, now);
            if (effectivePriority < bestPriority ||
                (effectivePriority === bestPriority && request.enqueuedAt < bestEnqueuedAt)) {
                bestPriority = effectivePriority;
                bestEnqueuedAt = request.enqueuedAt;
                bestIndex = i;
            }
        }
        return bestIndex;
    }
    computeEffectivePriority(request, now) {
        const waitBoost = Math.floor((now - request.enqueuedAt) / STARVATION_BOOST_MS);
        return request.priority - waitBoost;
    }
    removeRequest(requestId) {
        const index = this.queue.findIndex(req => req.id === requestId);
        if (index !== -1) {
            this.queue.splice(index, 1);
        }
    }
    normalizeFiles(files) {
        return Array.from(new Set(files.filter(Boolean))).sort();
    }
}
exports.FileLockManager = FileLockManager;
//# sourceMappingURL=file-lock-manager.js.map

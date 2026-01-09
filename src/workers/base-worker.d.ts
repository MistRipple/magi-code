/**
 * Worker 基类
 * 定义 CLI 执行的抽象接口
 */
import { ChildProcess } from 'child_process';
import { CLIType, SubTask, WorkerResult, WorkerConfig } from '../types';
import { EventEmitter } from '../events';
/** Worker 执行选项 */
export interface WorkerExecuteOptions {
    subTask: SubTask;
    workingDirectory: string;
    timeout?: number;
    onOutput?: (output: string) => void;
}
/**
 * 抽象 Worker 基类
 * 各 CLI Worker 需要继承此类并实现抽象方法
 */
export declare abstract class BaseWorker extends EventEmitter {
    protected config: WorkerConfig;
    protected process: ChildProcess | null;
    protected isRunning: boolean;
    protected outputBuffer: string[];
    constructor(config: WorkerConfig);
    /** CLI 类型 */
    abstract get cliType(): CLIType;
    /** 构建 CLI 命令参数 */
    protected abstract buildArgs(subTask: SubTask): string[];
    /** 解析 CLI 输出 */
    protected abstract parseOutput(output: string): Partial<WorkerResult>;
    /** 执行子任务 */
    execute(options: WorkerExecuteOptions): Promise<WorkerResult>;
    /** 运行 CLI 进程 */
    protected runProcess(args: string[], cwd: string, timeout?: number, onOutput?: (output: string) => void): Promise<string>;
    /** 打断执行 */
    interrupt(): boolean;
    get running(): boolean;
    getOutput(): string[];
    getConfig(): WorkerConfig;
}
//# sourceMappingURL=base-worker.d.ts.map
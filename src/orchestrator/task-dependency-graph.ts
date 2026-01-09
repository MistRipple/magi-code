/**
 * 任务依赖图
 * 管理任务之间的依赖关系，支持拓扑排序和并行分组
 */

/** 任务节点 */
export interface TaskNode {
  /** 任务 ID */
  id: string;
  /** 任务名称 */
  name: string;
  /** 依赖的任务 ID 列表 */
  dependencies: string[];
  /** 被依赖的任务 ID 列表（反向边） */
  dependents: string[];
  /** 任务状态 */
  status: 'pending' | 'ready' | 'running' | 'completed' | 'failed';
  /** 任务数据（可选） */
  data?: unknown;
}

/** 并行执行批次 */
export interface ExecutionBatch {
  /** 批次编号 */
  batchIndex: number;
  /** 该批次可并行执行的任务 ID 列表 */
  taskIds: string[];
}

/** 依赖图分析结果 */
export interface DependencyAnalysis {
  /** 是否有循环依赖 */
  hasCycle: boolean;
  /** 循环依赖涉及的任务（如果有） */
  cycleNodes?: string[];
  /** 拓扑排序结果 */
  topologicalOrder: string[];
  /** 并行执行批次 */
  executionBatches: ExecutionBatch[];
  /** 关键路径（最长依赖链） */
  criticalPath: string[];
}

/**
 * 任务依赖图类
 * 使用邻接表实现有向无环图（DAG）
 */
export class TaskDependencyGraph {
  private nodes: Map<string, TaskNode> = new Map();

  /**
   * 添加任务节点
   */
  addTask(id: string, name: string, data?: unknown): void {
    if (this.nodes.has(id)) {
      console.warn(`[TaskDependencyGraph] 任务 ${id} 已存在，跳过添加`);
      return;
    }

    this.nodes.set(id, {
      id,
      name,
      dependencies: [],
      dependents: [],
      status: 'pending',
      data,
    });
  }

  /**
   * 添加依赖关系
   * @param taskId 任务 ID
   * @param dependsOn 依赖的任务 ID
   */
  addDependency(taskId: string, dependsOn: string): boolean {
    const task = this.nodes.get(taskId);
    const dependency = this.nodes.get(dependsOn);

    if (!task) {
      console.warn(`[TaskDependencyGraph] 任务 ${taskId} 不存在`);
      return false;
    }

    if (!dependency) {
      console.warn(`[TaskDependencyGraph] 依赖任务 ${dependsOn} 不存在`);
      return false;
    }

    // 检查是否会形成循环
    if (this.wouldCreateCycle(taskId, dependsOn)) {
      console.warn(`[TaskDependencyGraph] 添加依赖会形成循环: ${taskId} -> ${dependsOn}`);
      return false;
    }

    // 添加依赖关系
    if (!task.dependencies.includes(dependsOn)) {
      task.dependencies.push(dependsOn);
    }

    // 添加反向边
    if (!dependency.dependents.includes(taskId)) {
      dependency.dependents.push(taskId);
    }

    return true;
  }

  /**
   * 批量添加依赖关系
   */
  addDependencies(taskId: string, dependsOnList: string[]): void {
    for (const dependsOn of dependsOnList) {
      this.addDependency(taskId, dependsOn);
    }
  }

  /**
   * 检查添加依赖是否会形成循环
   */
  private wouldCreateCycle(taskId: string, dependsOn: string): boolean {
    // 如果 dependsOn 可以到达 taskId，则添加 taskId -> dependsOn 会形成循环
    const visited = new Set<string>();
    const stack = [dependsOn];

    while (stack.length > 0) {
      const current = stack.pop()!;
      if (current === taskId) {
        return true; // 发现循环
      }

      if (visited.has(current)) {
        continue;
      }
      visited.add(current);

      const node = this.nodes.get(current);
      if (node) {
        // 检查 current 的依赖（即 current 指向的节点）
        for (const dep of node.dependencies) {
          if (!visited.has(dep)) {
            stack.push(dep);
          }
        }
      }
    }

    return false;
  }

  /**
   * 获取任务节点
   */
  getTask(id: string): TaskNode | undefined {
    return this.nodes.get(id);
  }

  /**
   * 获取所有任务
   */
  getAllTasks(): TaskNode[] {
    return Array.from(this.nodes.values());
  }

  /**
   * 获取任务数量
   */
  get size(): number {
    return this.nodes.size;
  }

  /**
   * 更新任务状态
   */
  updateTaskStatus(id: string, status: TaskNode['status']): void {
    const task = this.nodes.get(id);
    if (task) {
      task.status = status;
    }
  }

  /**
   * 获取就绪的任务（所有依赖都已完成）
   */
  getReadyTasks(): TaskNode[] {
    const ready: TaskNode[] = [];

    for (const node of this.nodes.values()) {
      if (node.status !== 'pending') {
        continue;
      }

      // 检查所有依赖是否已完成
      const allDepsCompleted = node.dependencies.every(depId => {
        const dep = this.nodes.get(depId);
        return dep && dep.status === 'completed';
      });

      if (allDepsCompleted) {
        ready.push(node);
      }
    }

    return ready;
  }

  /**
   * 分析依赖图
   * 返回拓扑排序结果和并行执行批次
   */
  analyze(): DependencyAnalysis {
    // 使用 Kahn 算法进行拓扑排序
    const inDegree = new Map<string, number>();
    const topologicalOrder: string[] = [];
    const executionBatches: ExecutionBatch[] = [];

    // 初始化入度
    for (const [id, node] of this.nodes) {
      inDegree.set(id, node.dependencies.length);
    }

    // 找出所有入度为 0 的节点（第一批可执行的任务）
    let currentBatch: string[] = [];
    for (const [id, degree] of inDegree) {
      if (degree === 0) {
        currentBatch.push(id);
      }
    }

    let batchIndex = 0;
    const processed = new Set<string>();

    while (currentBatch.length > 0) {
      // 记录当前批次
      executionBatches.push({
        batchIndex,
        taskIds: [...currentBatch],
      });

      // 将当前批次加入拓扑排序结果
      topologicalOrder.push(...currentBatch);

      // 准备下一批次
      const nextBatch: string[] = [];

      for (const taskId of currentBatch) {
        processed.add(taskId);
        const node = this.nodes.get(taskId);
        if (!node) continue;

        // 减少所有依赖此任务的节点的入度
        for (const dependentId of node.dependents) {
          const newDegree = (inDegree.get(dependentId) || 0) - 1;
          inDegree.set(dependentId, newDegree);

          // 如果入度变为 0，加入下一批次
          if (newDegree === 0 && !processed.has(dependentId)) {
            nextBatch.push(dependentId);
          }
        }
      }

      currentBatch = nextBatch;
      batchIndex++;
    }

    // 检查是否有循环依赖
    const hasCycle = processed.size < this.nodes.size;
    let cycleNodes: string[] | undefined;

    if (hasCycle) {
      cycleNodes = Array.from(this.nodes.keys()).filter(id => !processed.has(id));
    }

    // 计算关键路径
    const criticalPath = this.findCriticalPath();

    return {
      hasCycle,
      cycleNodes,
      topologicalOrder,
      executionBatches,
      criticalPath,
    };
  }

  /**
   * 找出关键路径（最长依赖链）
   */
  private findCriticalPath(): string[] {
    const distances = new Map<string, number>();
    const predecessors = new Map<string, string | null>();

    // 初始化
    for (const id of this.nodes.keys()) {
      distances.set(id, 0);
      predecessors.set(id, null);
    }

    // 获取拓扑排序
    const analysis = this.getTopologicalOrder();
    if (!analysis) return [];

    // 计算最长路径
    for (const taskId of analysis) {
      const node = this.nodes.get(taskId);
      if (!node) continue;

      for (const dependentId of node.dependents) {
        const currentDist = distances.get(taskId) || 0;
        const dependentDist = distances.get(dependentId) || 0;

        if (currentDist + 1 > dependentDist) {
          distances.set(dependentId, currentDist + 1);
          predecessors.set(dependentId, taskId);
        }
      }
    }

    // 找出最远的节点
    let maxDist = 0;
    let endNode = '';
    for (const [id, dist] of distances) {
      if (dist > maxDist) {
        maxDist = dist;
        endNode = id;
      }
    }

    // 回溯构建关键路径
    const criticalPath: string[] = [];
    let current: string | null = endNode;
    while (current) {
      criticalPath.unshift(current);
      current = predecessors.get(current) || null;
    }

    return criticalPath;
  }

  /**
   * 获取简单的拓扑排序（不包含批次信息）
   */
  private getTopologicalOrder(): string[] | null {
    const inDegree = new Map<string, number>();
    const result: string[] = [];

    for (const [id, node] of this.nodes) {
      inDegree.set(id, node.dependencies.length);
    }

    const queue: string[] = [];
    for (const [id, degree] of inDegree) {
      if (degree === 0) {
        queue.push(id);
      }
    }

    while (queue.length > 0) {
      const taskId = queue.shift()!;
      result.push(taskId);

      const node = this.nodes.get(taskId);
      if (!node) continue;

      for (const dependentId of node.dependents) {
        const newDegree = (inDegree.get(dependentId) || 0) - 1;
        inDegree.set(dependentId, newDegree);
        if (newDegree === 0) {
          queue.push(dependentId);
        }
      }
    }

    return result.length === this.nodes.size ? result : null;
  }

  /**
   * 清空依赖图
   */
  clear(): void {
    this.nodes.clear();
  }

  /**
   * 移除任务
   */
  removeTask(id: string): boolean {
    const task = this.nodes.get(id);
    if (!task) return false;

    // 移除其他任务对此任务的依赖
    for (const depId of task.dependencies) {
      const dep = this.nodes.get(depId);
      if (dep) {
        dep.dependents = dep.dependents.filter(d => d !== id);
      }
    }

    // 移除依赖此任务的其他任务的依赖关系
    for (const dependentId of task.dependents) {
      const dependent = this.nodes.get(dependentId);
      if (dependent) {
        dependent.dependencies = dependent.dependencies.filter(d => d !== id);
      }
    }

    this.nodes.delete(id);
    return true;
  }

  /**
   * 获取依赖图的可视化描述
   */
  toMermaid(): string {
    const lines: string[] = ['graph TD'];

    for (const [id, node] of this.nodes) {
      const label = node.name.replace(/"/g, "'");
      lines.push(`  ${id}["${label}"]`);

      for (const depId of node.dependencies) {
        lines.push(`  ${depId} --> ${id}`);
      }
    }

    return lines.join('\n');
  }
}
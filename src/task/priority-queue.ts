/**
 * PriorityQueue - 优先级队列
 *
 * 使用最小堆实现，优先级数字越小越优先（1 最高优先级）
 */

export interface PriorityItem {
  id: string;
  priority: number;
}

/**
 * 优先级队列
 * 用于管理待执行的 Task
 */
export class PriorityQueue<T extends PriorityItem> {
  private heap: T[] = [];
  private indexMap: Map<string, number> = new Map();

  /**
   * 入队
   */
  enqueue(item: T): void {
    this.heap.push(item);
    this.indexMap.set(item.id, this.heap.length - 1);
    this.bubbleUp(this.heap.length - 1);
  }

  /**
   * 出队（返回优先级最高的项）
   */
  dequeue(): T | null {
    if (this.heap.length === 0) return null;
    if (this.heap.length === 1) {
      const item = this.heap.pop()!;
      this.indexMap.delete(item.id);
      return item;
    }

    const top = this.heap[0];
    const last = this.heap.pop()!;
    this.heap[0] = last;
    this.indexMap.delete(top.id);
    this.indexMap.set(last.id, 0);
    this.bubbleDown(0);

    return top;
  }

  /**
   * 查看队首（不移除）
   */
  peek(): T | null {
    return this.heap.length > 0 ? this.heap[0] : null;
  }

  /**
   * 移除指定项
   */
  remove(id: string): boolean {
    const index = this.indexMap.get(id);
    if (index === undefined) return false;

    if (index === this.heap.length - 1) {
      this.heap.pop();
      this.indexMap.delete(id);
      return true;
    }

    const last = this.heap.pop()!;
    const removed = this.heap[index];
    this.heap[index] = last;
    this.indexMap.delete(removed.id);
    this.indexMap.set(last.id, index);

    // 可能需要向上或向下调整
    if (last.priority < removed.priority) {
      this.bubbleUp(index);
    } else if (last.priority > removed.priority) {
      this.bubbleDown(index);
    }

    return true;
  }

  /**
   * 更新优先级
   */
  updatePriority(id: string, priority: number): void {
    const index = this.indexMap.get(id);
    if (index === undefined) return;

    const oldPriority = this.heap[index].priority;
    this.heap[index].priority = priority;

    if (priority < oldPriority) {
      this.bubbleUp(index);
    } else if (priority > oldPriority) {
      this.bubbleDown(index);
    }
  }

  /**
   * 获取队列大小
   */
  size(): number {
    return this.heap.length;
  }

  /**
   * 清空队列
   */
  clear(): void {
    this.heap = [];
    this.indexMap.clear();
  }

  /**
   * 检查是否包含指定项
   */
  has(id: string): boolean {
    return this.indexMap.has(id);
  }

  /**
   * 获取所有项（不保证顺序）
   */
  toArray(): T[] {
    return [...this.heap];
  }

  /**
   * 向上调整（用于插入）
   */
  private bubbleUp(index: number): void {
    while (index > 0) {
      const parentIndex = Math.floor((index - 1) / 2);
      if (this.heap[index].priority >= this.heap[parentIndex].priority) {
        break;
      }

      this.swap(index, parentIndex);
      index = parentIndex;
    }
  }

  /**
   * 向下调整（用于删除）
   */
  private bubbleDown(index: number): void {
    while (true) {
      const leftChild = 2 * index + 1;
      const rightChild = 2 * index + 2;
      let smallest = index;

      if (
        leftChild < this.heap.length &&
        this.heap[leftChild].priority < this.heap[smallest].priority
      ) {
        smallest = leftChild;
      }

      if (
        rightChild < this.heap.length &&
        this.heap[rightChild].priority < this.heap[smallest].priority
      ) {
        smallest = rightChild;
      }

      if (smallest === index) break;

      this.swap(index, smallest);
      index = smallest;
    }
  }

  /**
   * 交换两个元素
   */
  private swap(i: number, j: number): void {
    const temp = this.heap[i];
    this.heap[i] = this.heap[j];
    this.heap[j] = temp;

    this.indexMap.set(this.heap[i].id, i);
    this.indexMap.set(this.heap[j].id, j);
  }
}

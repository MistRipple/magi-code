/**
 * MinHeap<T> — 泛型最小堆
 *
 * 用于高效维护 Top-K 最大元素（固定容量最小堆）：
 * - push: O(log K)
 * - peek: O(1)
 * - toSortedArray: O(K log K)
 *
 * 替换 `Array.sort().slice(0, K)` 的 O(N log N) 全量排序，
 * 当 N >> K 时显著降低排序开销。
 */
export class MinHeap<T> {
  private heap: T[] = [];
  private compare: (a: T, b: T) => number;
  private capacity: number;

  /**
   * @param capacity 堆的最大容量（即 Top-K 的 K）
   * @param compare 比较函数，返回负数表示 a < b（堆顶为最小值）
   */
  constructor(capacity: number, compare: (a: T, b: T) => number) {
    this.capacity = capacity;
    this.compare = compare;
  }

  get size(): number {
    return this.heap.length;
  }

  /** 查看堆顶（最小元素） */
  peek(): T | undefined {
    return this.heap[0];
  }

  /**
   * 插入元素。
   * 若堆未满 → 直接插入。
   * 若堆已满且新元素 > 堆顶 → 替换堆顶并下沉。
   * 若堆已满且新元素 <= 堆顶 → 丢弃（不属于 Top-K）。
   */
  push(item: T): void {
    if (this.heap.length < this.capacity) {
      this.heap.push(item);
      this.siftUp(this.heap.length - 1);
    } else if (this.heap.length > 0 && this.compare(item, this.heap[0]) > 0) {
      // 新元素比堆顶大 → 替换堆顶
      this.heap[0] = item;
      this.siftDown(0);
    }
  }

  /** 返回排序后的数组（降序：最大在前） */
  toSortedDescArray(): T[] {
    return [...this.heap].sort((a, b) => this.compare(b, a));
  }

  // ========================================================================
  // 内部堆操作
  // ========================================================================

  private siftUp(index: number): void {
    while (index > 0) {
      const parent = (index - 1) >> 1;
      if (this.compare(this.heap[index], this.heap[parent]) < 0) {
        this.swap(index, parent);
        index = parent;
      } else {
        break;
      }
    }
  }

  private siftDown(index: number): void {
    const n = this.heap.length;
    while (true) {
      let smallest = index;
      const left = 2 * index + 1;
      const right = 2 * index + 2;

      if (left < n && this.compare(this.heap[left], this.heap[smallest]) < 0) {
        smallest = left;
      }
      if (right < n && this.compare(this.heap[right], this.heap[smallest]) < 0) {
        smallest = right;
      }

      if (smallest !== index) {
        this.swap(index, smallest);
        index = smallest;
      } else {
        break;
      }
    }
  }

  private swap(i: number, j: number): void {
    const tmp = this.heap[i];
    this.heap[i] = this.heap[j];
    this.heap[j] = tmp;
  }
}


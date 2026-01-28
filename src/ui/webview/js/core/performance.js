/**
 * 性能优化工具 (Performance Optimization Utilities)
 *
 * 功能：
 * - 虚拟滚动 (Virtual Scrolling)
 * - 节流 (Throttle)
 * - 防抖 (Debounce)
 * - 懒加载 (Lazy Loading)
 * - 请求动画帧调度 (RAF Scheduling)
 */

/**
 * 节流函数 - 限制函数执行频率
 * @param {Function} func - 要节流的函数
 * @param {number} wait - 等待时间（毫秒）
 * @returns {Function} 节流后的函数
 */
export function throttle(func, wait = 100) {
  let timeout = null;
  let lastTime = 0;

  return function throttled(...args) {
    const now = Date.now();
    const remaining = wait - (now - lastTime);

    if (remaining <= 0) {
      if (timeout) {
        clearTimeout(timeout);
        timeout = null;
      }
      lastTime = now;
      func.apply(this, args);
    } else if (!timeout) {
      timeout = setTimeout(() => {
        lastTime = Date.now();
        timeout = null;
        func.apply(this, args);
      }, remaining);
    }
  };
}

/**
 * 防抖函数 - 延迟执行直到停止调用
 * @param {Function} func - 要防抖的函数
 * @param {number} wait - 等待时间（毫秒）
 * @param {boolean} immediate - 是否立即执行
 * @returns {Function} 防抖后的函数
 */
export function debounce(func, wait = 300, immediate = false) {
  let timeout = null;

  return function debounced(...args) {
    const callNow = immediate && !timeout;

    clearTimeout(timeout);
    timeout = setTimeout(() => {
      timeout = null;
      if (!immediate) {
        func.apply(this, args);
      }
    }, wait);

    if (callNow) {
      func.apply(this, args);
    }
  };
}

/**
 * 请求动画帧节流
 * @param {Function} func - 要执行的函数
 * @returns {Function} RAF节流后的函数
 */
export function rafThrottle(func) {
  let rafId = null;
  let lastArgs = null;

  return function rafThrottled(...args) {
    lastArgs = args;

    if (rafId === null) {
      rafId = requestAnimationFrame(() => {
        func.apply(this, lastArgs);
        rafId = null;
      });
    }
  };
}

/**
 * 批处理DOM更新
 */
export class BatchDOMUpdater {
  constructor() {
    this.updates = [];
    this.rafId = null;
  }

  /**
   * 添加DOM更新
   * @param {Function} updateFn - 更新函数
   */
  add(updateFn) {
    this.updates.push(updateFn);
    this.schedule();
  }

  /**
   * 调度批处理
   */
  schedule() {
    if (this.rafId !== null) return;

    this.rafId = requestAnimationFrame(() => {
      this.flush();
    });
  }

  /**
   * 执行所有更新
   */
  flush() {
    const updates = this.updates.slice();
    this.updates = [];
    this.rafId = null;

    updates.forEach(updateFn => {
      try {
        updateFn();
      } catch (error) {
        console.error('[BatchDOMUpdater] Error executing update:', error);
      }
    });
  }

  /**
   * 清空待处理的更新
   */
  clear() {
    this.updates = [];
    if (this.rafId !== null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
  }
}

/**
 * 虚拟滚动管理器
 */
export class VirtualScrollManager {
  constructor(options = {}) {
    this.container = options.container;
    this.itemHeight = options.itemHeight || 100;
    this.buffer = options.buffer || 5;
    this.items = [];
    this.visibleRange = { start: 0, end: 0 };
    this.scrollHandler = rafThrottle(this.updateVisibleRange.bind(this));
  }

  /**
   * 设置项目列表
   * @param {Array} items - 项目数组
   */
  setItems(items) {
    this.items = items;
    this.updateVisibleRange();
  }

  /**
   * 初始化
   */
  init() {
    if (!this.container) return;

    this.container.addEventListener('scroll', this.scrollHandler);
    this.updateVisibleRange();
  }

  /**
   * 销毁
   */
  destroy() {
    if (!this.container) return;

    this.container.removeEventListener('scroll', this.scrollHandler);
  }

  /**
   * 更新可见范围
   */
  updateVisibleRange() {
    if (!this.container) return;

    const scrollTop = this.container.scrollTop;
    const containerHeight = this.container.clientHeight;

    const start = Math.max(0, Math.floor(scrollTop / this.itemHeight) - this.buffer);
    const end = Math.min(
      this.items.length,
      Math.ceil((scrollTop + containerHeight) / this.itemHeight) + this.buffer
    );

    if (start !== this.visibleRange.start || end !== this.visibleRange.end) {
      this.visibleRange = { start, end };
      this.render();
    }
  }

  /**
   * 渲染可见项目
   */
  render() {
    // 由子类实现或通过回调函数
    if (this.onRender) {
      this.onRender(this.visibleRange, this.items);
    }
  }

  /**
   * 获取可见项目
   * @returns {Array} 可见项目数组
   */
  getVisibleItems() {
    return this.items.slice(this.visibleRange.start, this.visibleRange.end);
  }
}

/**
 * 交叉观察器懒加载
 */
export class LazyLoader {
  constructor(options = {}) {
    this.rootMargin = options.rootMargin || '50px';
    this.threshold = options.threshold || 0.01;
    this.onIntersect = options.onIntersect || (() => {});

    this.observer = new IntersectionObserver(
      this.handleIntersection.bind(this),
      {
        rootMargin: this.rootMargin,
        threshold: this.threshold
      }
    );

    this.observedElements = new Set();
  }

  /**
   * 观察元素
   * @param {HTMLElement} element - 要观察的元素
   */
  observe(element) {
    if (!element || this.observedElements.has(element)) return;

    this.observer.observe(element);
    this.observedElements.add(element);
  }

  /**
   * 停止观察元素
   * @param {HTMLElement} element - 停止观察的元素
   */
  unobserve(element) {
    if (!element || !this.observedElements.has(element)) return;

    this.observer.unobserve(element);
    this.observedElements.delete(element);
  }

  /**
   * 处理交叉事件
   * @param {Array} entries - 交叉观察器条目
   */
  handleIntersection(entries) {
    entries.forEach(entry => {
      if (entry.isIntersecting) {
        this.onIntersect(entry.target);
        this.unobserve(entry.target);
      }
    });
  }

  /**
   * 销毁
   */
  destroy() {
    this.observer.disconnect();
    this.observedElements.clear();
  }
}

/**
 * 长列表优化 - 限制DOM节点数量
 */
export class DOMNodeLimiter {
  constructor(container, maxNodes = 1000) {
    this.container = container;
    this.maxNodes = maxNodes;
    this.nodes = [];
  }

  /**
   * 添加节点
   * @param {HTMLElement} node - 要添加的节点
   */
  addNode(node) {
    this.nodes.push(node);
    this.container.appendChild(node);

    // 如果超过限制，移除最旧的节点
    if (this.nodes.length > this.maxNodes) {
      const oldNode = this.nodes.shift();
      if (oldNode && oldNode.parentNode) {
        oldNode.parentNode.removeChild(oldNode);
      }
    }
  }

  /**
   * 清空所有节点
   */
  clear() {
    this.nodes.forEach(node => {
      if (node && node.parentNode) {
        node.parentNode.removeChild(node);
      }
    });
    this.nodes = [];
  }

  /**
   * 获取当前节点数量
   * @returns {number} 节点数量
   */
  getNodeCount() {
    return this.nodes.length;
  }
}

/**
 * 性能监控器
 */
export class PerformanceMonitor {
  constructor() {
    this.marks = new Map();
    this.measures = [];
  }

  /**
   * 标记时间点
   * @param {string} name - 标记名称
   */
  mark(name) {
    this.marks.set(name, performance.now());
  }

  /**
   * 测量两个标记之间的时间
   * @param {string} name - 测量名称
   * @param {string} startMark - 开始标记
   * @param {string} endMark - 结束标记（可选，默认为当前时间）
   * @returns {number} 持续时间（毫秒）
   */
  measure(name, startMark, endMark) {
    const startTime = this.marks.get(startMark);
    if (!startTime) {
      console.warn(`[PerformanceMonitor] Start mark "${startMark}" not found`);
      return 0;
    }

    const endTime = endMark ? this.marks.get(endMark) : performance.now();
    if (endMark && !endTime) {
      console.warn(`[PerformanceMonitor] End mark "${endMark}" not found`);
      return 0;
    }

    const duration = endTime - startTime;
    this.measures.push({ name, startMark, endMark, duration, timestamp: Date.now() });

    return duration;
  }

  /**
   * 获取测量结果
   * @param {string} name - 测量名称（可选）
   * @returns {Array} 测量结果数组
   */
  getMeasures(name) {
    if (name) {
      return this.measures.filter(m => m.name === name);
    }
    return this.measures;
  }

  /**
   * 清空所有标记和测量
   */
  clear() {
    this.marks.clear();
    this.measures = [];
  }

  /**
   * 记录到控制台
   * @param {string} name - 测量名称（可选）
   */
  log(name) {
    const measures = this.getMeasures(name);
    if (measures.length === 0) {
      console.log('[PerformanceMonitor] No measures found');
      return;
    }

    console.group('[PerformanceMonitor] Performance Measures');
    measures.forEach(m => {
      console.log(`${m.name}: ${m.duration.toFixed(2)}ms`);
    });
    console.groupEnd();
  }
}

/**
 * 创建全局性能监控器实例
 */
export const perfMonitor = new PerformanceMonitor();

/**
 * 创建全局批处理DOM更新器实例
 */
export const batchUpdater = new BatchDOMUpdater();

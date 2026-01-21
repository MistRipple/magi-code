/**
 * 性能监控系统
 *
 * 提供：
 * - 计时器
 * - 指标收集
 * - 统计分析
 */

import { ConfigManager } from '../config';

/**
 * 指标数据点
 */
interface Metric {
  value: number;
  timestamp: number;
}

/**
 * 指标统计
 */
export interface MetricStats {
  count: number;
  min: number;
  max: number;
  avg: number;
  p50: number;
  p95: number;
  p99: number;
}

/**
 * 性能监控器
 */
export class PerformanceMonitor {
  private static instance: PerformanceMonitor | null = null;
  private metrics: Map<string, Metric[]> = new Map();
  private readonly MAX_METRICS_PER_NAME = 1000;

  private constructor() {}

  /**
   * 获取单例实例
   */
  static getInstance(): PerformanceMonitor {
    if (!PerformanceMonitor.instance) {
      PerformanceMonitor.instance = new PerformanceMonitor();
    }
    return PerformanceMonitor.instance;
  }

  /**
   * 检查是否启用监控
   */
  private isEnabled(): boolean {
    return ConfigManager.getInstance().get('performance').enablePerformanceMonitoring;
  }

  /**
   * 启动计时器
   */
  startTimer(name: string): () => void {
    if (!this.isEnabled()) {
      return () => {}; // 空函数
    }

    const start = Date.now();
    return () => {
      const duration = Date.now() - start;
      this.recordMetric(name, duration);
    };
  }

  /**
   * 记录指标
   */
  recordMetric(name: string, value: number): void {
    if (!this.isEnabled()) return;

    if (!this.metrics.has(name)) {
      this.metrics.set(name, []);
    }

    const metrics = this.metrics.get(name)!;
    metrics.push({
      value,
      timestamp: Date.now(),
    });

    // 保留最近的指标
    if (metrics.length > this.MAX_METRICS_PER_NAME) {
      metrics.shift();
    }
  }

  /**
   * 获取指标统计
   */
  getStats(name: string): MetricStats | null {
    const metrics = this.metrics.get(name);
    if (!metrics || metrics.length === 0) return null;

    const values = metrics.map(m => m.value);
    const sorted = [...values].sort((a, b) => a - b);

    return {
      count: values.length,
      min: sorted[0],
      max: sorted[sorted.length - 1],
      avg: values.reduce((a, b) => a + b, 0) / values.length,
      p50: this.percentile(sorted, 0.5),
      p95: this.percentile(sorted, 0.95),
      p99: this.percentile(sorted, 0.99),
    };
  }

  /**
   * 获取所有指标名称
   */
  getMetricNames(): string[] {
    return Array.from(this.metrics.keys());
  }

  /**
   * 获取所有统计
   */
  getAllStats(): Record<string, MetricStats> {
    const result: Record<string, MetricStats> = {};
    for (const name of this.getMetricNames()) {
      const stats = this.getStats(name);
      if (stats) {
        result[name] = stats;
      }
    }
    return result;
  }

  /**
   * 清除指标
   */
  clear(name?: string): void {
    if (name) {
      this.metrics.delete(name);
    } else {
      this.metrics.clear();
    }
  }

  /**
   * 计算百分位数
   */
  private percentile(sorted: number[], p: number): number {
    if (sorted.length === 0) return 0;
    const index = Math.ceil(sorted.length * p) - 1;
    return sorted[Math.max(0, Math.min(index, sorted.length - 1))];
  }

  /**
   * 导出指标数据
   */
  export(): Record<string, Metric[]> {
    const result: Record<string, Metric[]> = {};
    for (const [name, metrics] of this.metrics.entries()) {
      result[name] = [...metrics];
    }
    return result;
  }

  /**
   * 生成报告
   */
  generateReport(): string {
    const lines: string[] = [];
    lines.push('=== Performance Report ===');
    lines.push('');

    const allStats = this.getAllStats();
    const names = Object.keys(allStats).sort();

    if (names.length === 0) {
      lines.push('No metrics recorded.');
      return lines.join('\n');
    }

    for (const name of names) {
      const stats = allStats[name];
      lines.push(`${name}:`);
      lines.push(`  Count: ${stats.count}`);
      lines.push(`  Min: ${stats.min.toFixed(2)}ms`);
      lines.push(`  Max: ${stats.max.toFixed(2)}ms`);
      lines.push(`  Avg: ${stats.avg.toFixed(2)}ms`);
      lines.push(`  P50: ${stats.p50.toFixed(2)}ms`);
      lines.push(`  P95: ${stats.p95.toFixed(2)}ms`);
      lines.push(`  P99: ${stats.p99.toFixed(2)}ms`);
      lines.push('');
    }

    return lines.join('\n');
  }
}

/**
 * 便捷函数：启动计时器
 */
export function startTimer(name: string): () => void {
  return PerformanceMonitor.getInstance().startTimer(name);
}

/**
 * 便捷函数：记录指标
 */
export function recordMetric(name: string, value: number): void {
  PerformanceMonitor.getInstance().recordMetric(name, value);
}

/**
 * 便捷函数：获取统计
 */
export function getStats(name: string): MetricStats | null {
  return PerformanceMonitor.getInstance().getStats(name);
}

/**
 * 便捷函数：生成报告
 */
export function generateReport(): string {
  return PerformanceMonitor.getInstance().generateReport();
}

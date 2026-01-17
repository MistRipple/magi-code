/**
 * 流程追踪器 - 实时追踪用户消息的完整生命周期
 * 
 * 功能：
 * 1. 记录每个阶段的时间戳
 * 2. 追踪消息在各组件间的流转
 * 3. 生成可视化的时序图
 * 4. 检测流程卡点和性能瓶颈
 */

import { EventEmitter } from 'events';

interface FlowEvent {
  timestamp: number;
  component: string;
  action: string;
  data?: any;
  duration?: number;
}

interface FlowAnalysis {
  totalDuration: number;
  bottlenecks: Array<{ component: string; duration: number }>;
  messageCount: number;
  stateTransitions: number;
}

class FlowTracer extends EventEmitter {
  private events: FlowEvent[] = [];
  private startTime: number = 0;
  private componentTimers: Map<string, number> = new Map();
  
  startTrace(userPrompt: string): void {
    this.events = [];
    this.startTime = Date.now();
    this.componentTimers.clear();
    
    this.recordEvent('User', 'input', { prompt: userPrompt });
    console.log('\n========================================');
    console.log('流程追踪开始');
    console.log('========================================');
    console.log(`用户输入: ${userPrompt}\n`);
  }
  
  recordEvent(component: string, action: string, data?: any): void {
    const now = Date.now();
    const event: FlowEvent = {
      timestamp: now,
      component,
      action,
      data,
      duration: now - this.startTime
    };
    
    this.events.push(event);
    
    // 记录组件计时
    if (!this.componentTimers.has(component)) {
      this.componentTimers.set(component, now);
    }
    
    // 实时输出
    const elapsed = now - this.startTime;
    console.log(`[+${elapsed}ms] ${component} → ${action}`);
    if (data && Object.keys(data).length > 0) {
      console.log(`         数据:`, JSON.stringify(data, null, 2).split('\n').slice(0, 3).join('\n'));
    }
    
    this.emit('event', event);
  }
  
  endTrace(): FlowAnalysis {
    const totalDuration = Date.now() - this.startTime;
    
    console.log('\n========================================');
    console.log('流程追踪结束');
    console.log('========================================');
    console.log(`总耗时: ${totalDuration}ms`);
    console.log(`事件数: ${this.events.length}\n`);
    
    // 分析瓶颈
    const bottlenecks = this.analyzeBottlenecks();
    
    if (bottlenecks.length > 0) {
      console.log('性能瓶颈:');
      bottlenecks.forEach((b, idx) => {
        console.log(`  ${idx + 1}. ${b.component}: ${b.duration}ms`);
      });
      console.log();
    }
    
    // 统计
    const messageCount = this.events.filter(e => e.action.includes('message')).length;
    const stateTransitions = this.events.filter(e => e.action.includes('state')).length;
    
    console.log('统计信息:');
    console.log(`  消息数: ${messageCount}`);
    console.log(`  状态转换: ${stateTransitions}`);
    console.log(`  组件数: ${this.componentTimers.size}\n`);
    
    return {
      totalDuration,
      bottlenecks,
      messageCount,
      stateTransitions
    };
  }
  
  private analyzeBottlenecks(): Array<{ component: string; duration: number }> {
    const componentDurations = new Map<string, number>();
    
    for (let i = 0; i < this.events.length - 1; i++) {
      const current = this.events[i];
      const next = this.events[i + 1];
      const duration = next.timestamp - current.timestamp;
      
      const existing = componentDurations.get(current.component) || 0;
      componentDurations.set(current.component, existing + duration);
    }
    
    const bottlenecks = Array.from(componentDurations.entries())
      .map(([component, duration]) => ({ component, duration }))
      .filter(b => b.duration > 100)
      .sort((a, b) => b.duration - a.duration);
    
    return bottlenecks;
  }
  
  generateMermaidDiagram(): string {
    const lines = ['sequenceDiagram'];
    const participants = new Set<string>();
    
    this.events.forEach(e => participants.add(e.component));
    participants.forEach(p => lines.push(`    participant ${p}`));
    
    this.events.forEach(e => {
      const duration = e.duration ? ` (+${e.duration}ms)` : '';
      lines.push(`    ${e.component}->>+${e.component}: ${e.action}${duration}`);
    });
    
    return lines.join('\n');
  }
  
  getEvents(): FlowEvent[] {
    return [...this.events];
  }
}

export { FlowTracer, FlowEvent, FlowAnalysis };

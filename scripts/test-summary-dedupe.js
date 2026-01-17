/**
 * 汇总去重验证脚本
 * 目的：模拟编排者汇总文本中出现重复段落时的去重效果
 */

const sampleSummary = `
执行总结
任务状态: 成功完成
完成工作: 回答了关于编排模式能力的咨询问题，详细说明了编排模式的工作机制和适用场景。
主要内容:
- 解释了编排模式的 5 个核心步骤：需求分析、计划制定、Worker 分配、执行、结果汇总
- 说明了三种 Worker 的专长分工：Claude（复杂架构）、Codex（后端开发）、Gemini（前端 UI）
- 列举了典型应用场景：前后端协作、复杂架构设计、多模块并行开发

## 执行总结
任务状态: 成功完成
完成工作: 回答了关于编排模式能力的咨询问题，详细说明了编排模式的工作机制和适用场景。
主要内容:
- 解释了编排模式的 5 个核心步骤：需求分析、计划制定、Worker 分配、执行、结果汇总
- 说明了三种 Worker 的专长分工：Claude（复杂架构）、Codex（后端开发）、Gemini（前端 UI）
- 列举了典型应用场景：前后端协作、复杂架构设计、多模块并行开发
`.trim();

function sanitizeSummaryText(content) {
  const withoutFences = content.replace(/```[\s\S]*?```/g, '[代码块已省略]');
  const normalized = withoutFences.replace(/\n{3,}/g, '\n\n').trim();
  const blocks = normalized.split(/\n{2,}/);
  const seen = new Set();
  const deduped = [];

  for (const block of blocks) {
    const trimmed = block.trim();
    if (!trimmed) continue;
    const key = trimmed
      .replace(/^#{1,6}\s*/gm, '')
      .replace(/\*\*/g, '')
      .replace(/\s+/g, ' ');
    if (seen.has(key)) continue;
    seen.add(key);
    deduped.push(trimmed);
  }

  const result = deduped.join('\n\n');
  const lines = result.split('\n');
  if (lines.length <= 40) {
    return result;
  }
  return `${lines.slice(0, 40).join('\n')}\n...(已省略)`;
}

console.log('=== 原始内容 ===\n');
console.log(sampleSummary);
console.log('\n=== 去重后 ===\n');
console.log(sanitizeSummaryText(sampleSummary));

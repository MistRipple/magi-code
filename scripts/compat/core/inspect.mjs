// 各生态翻译器共用的“检查”流程：扫描 → 逐项按规则归类 → 汇总 → 必需能力判定。
// 生态层只提供：入口文件解析、规则分类函数、数据流描述。
import { scanSources } from './scan.mjs';
import { summarize, verdictFor } from './report.mjs';

/**
 * @param {object} p
 * @param {string} p.translator 生态名
 * @param {{ name: string, version: string }} p.upstream
 * @param {string} p.root 包根目录
 * @param {object} p.scanOptions 传给 scanSources
 * @param {(kind: string, name: string) => object} p.classify
 * @param {string[]} [p.kinds] 要归类的条目种类；符号级（symbols）只有生态提供了符号规则才启用
 * @param {string[]} p.required
 * @param {object[]} [p.extraEntries] 生态自己发现的额外条目（如原生模块）
 */
export function buildReport({ translator, upstream, root, scanOptions, classify, required, extraEntries = [], kinds = ['imports', 'calls', 'injects', 'events'] }) {
  const scan = scanSources(root, scanOptions);
  if (scan.fileCount === 0) throw new Error(`没有扫描到任何源文件（${root}）：无法给出兼容结论。请确认入口，或用 directories 指定源码目录`);
  const entries = [];
  for (const deferred of scan.deferredImports) entries.push({
    kind: 'modules', name: `${deferred.file} -> ${deferred.module}`, category: 'runtime', status: 'dropped', optional: true,
    target: '原包条件注入保持等待', note: `宿主未提供 ${deferred.unavailableServices.join(', ')}；此动态导入分支不执行，不计为已翻译业务`,
    count: 1, files: [deferred.file],
  });
  for (const kind of kinds) {
    for (const [name, usage] of Object.entries(scan[kind] ?? {})) {
      const rule = classify(kind, name);
      entries.push({
        kind, name, category: rule.category, status: rule.status, target: rule.target,
        optional: usage.optional === true,
        ...(usage.nullable ? { nullable: true } : {}),
        note: usage.nullable ? `${rule.note}；上游可选路径：明确接纳订阅不可用时返回 null，不注册空监听`
          : usage.optional ? `${rule.note}；上游可选路径：依赖缺席时不执行注入回调，或同步注册失败由上游捕获` : rule.note,
        source: rule.source, needs: rule.needs, capability: rule.capability, count: usage.count, files: usage.files,
      });
    }
  }
  // 上下文被存进对象/属性或传给无法解析的对象方法：静态分析看不到它之后调用了什么，
  // 因此后续的 API 使用可能漏报。这类逃逸必须阻塞，而不是当作“没有额外调用”。
  if (scan.unresolvedContextFlows > 0) {
    entries.push({
      kind: 'flow', name: '<context-escape>', category: 'runtime', status: 'shim', target: '真实 Cordis 上下文对象',
      note: `宿主上下文对象被传给上游自己的辅助函数（${scan.unresolvedContextFlows} 处）；运行期仍由真实 Cordis 持有，未知宿主成员仍会在静态/启动检查中拒绝`,
      count: scan.unresolvedContextFlows, files: [],
    });
  }
  entries.push(...extraEntries);
  return {
    translator, upstream,
    scanned: { files: scan.fileCount, lines: scan.lineCount, fileList: scan.files },
    deferredImports: scan.deferredImports,
    serviceReads: scan.serviceReads,
    serviceMembers: scan.serviceMembers,
    profileMutations: scan.profileMutations,
    profilePackageProcesses: scan.profilePackageProcesses,
    entries, summary: summarize(entries), verdict: verdictFor(entries, required),
  };
}

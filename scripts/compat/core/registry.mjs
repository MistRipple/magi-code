// 翻译器工厂注册表：每种上游生态（dsh、pi……）注册一个工厂，按名称或按目录特征创建翻译器。
// 注册表只负责“找到并创建”，不含任何上游细节；上游细节全部在各自的翻译器包里。

/**
 * @typedef {object} Translator
 * @property {string} kind 上游生态名称（如 "dsh"）
 * @property {(dir: string) => Promise<number>} detect 目录属于该生态的把握，0 表示不是；越大越确定
 * @property {(dir: string, options?: object) => Promise<import('./report.mjs').CompatReport>} inspect 静态分析并生成兼容报告，不执行上游代码
 */

export class TranslatorRegistry {
  #factories = new Map();

  /**
   * @param {string} kind
   * @param {(options?: object) => Translator} factory
   */
  register(kind, factory) {
    if (!/^[a-z][a-z0-9-]*$/.test(kind)) throw new Error(`翻译器名称无效: ${kind}`);
    if (this.#factories.has(kind)) throw new Error(`翻译器已注册: ${kind}`);
    if (typeof factory !== 'function') throw new TypeError('工厂必须是函数');
    this.#factories.set(kind, factory);
    return this;
  }

  kinds() {
    return [...this.#factories.keys()].sort();
  }

  create(kind, options = {}) {
    const factory = this.#factories.get(kind);
    if (!factory) throw new Error(`未注册的翻译器: ${kind}（已注册：${this.kinds().join('、') || '无'}）`);
    const translator = factory(options);
    if (translator?.kind !== kind) throw new Error(`翻译器 ${kind} 的 kind 与注册名不一致`);
    return translator;
  }

  /** 按目录特征选择翻译器；没有任何一个有把握，或最高分并列，都明确失败而不是猜。 */
  async detect(dir, options = {}) {
    const scored = [];
    for (const kind of this.kinds()) {
      const translator = this.create(kind, options);
      const score = await translator.detect(dir);
      if (score > 0) scored.push({ kind, score });
    }
    scored.sort((a, b) => b.score - a.score);
    if (scored.length === 0) throw new Error(`无法识别 ${dir} 属于哪种上游生态（已注册：${this.kinds().join('、')}）`);
    if (scored.length > 1 && scored[0].score === scored[1].score) {
      throw new Error(`${dir} 同时匹配 ${scored[0].kind} 与 ${scored[1].kind}，请用 --kind 指定`);
    }
    return this.create(scored[0].kind, options);
  }
}

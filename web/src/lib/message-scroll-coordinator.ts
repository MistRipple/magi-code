export type ProgrammaticScrollIntent = 'follow-bottom' | 'restore' | 'navigation';

export interface ProgrammaticScrollTransaction {
  id: number;
  expectedTop: number;
  interactionEpoch: number;
  scopeKey: string;
  intent: ProgrammaticScrollIntent;
  writeChanged: boolean;
}

/**
 * 只负责确认一次应用滚动写入，绝不根据事件数量或时间猜测滚动来源。
 * 浏览器没有在 scroll 事件中标记来源，因此真实用户位置与应用目标不一致时，
 * 必须立即将控制权交还给用户。事务保持到目标事件到达或显式用户输入取消，
 * 这样主线程繁忙导致的延迟 scroll 事件仍然能被正确归因。无法匹配目标的
 * scroll 事件不能单独证明它来自用户，也可能是较早的应用写入；来源由输入
 * 监听器负责确认，不能在这里用一次事件覆盖当前事务。
 */
export class MessageScrollCoordinator {
  private transaction: ProgrammaticScrollTransaction | null = null;
  private nextTransactionId = 0;

  public constructor(private readonly tolerancePx = 1.5) {}

  public begin(
    expectedTop: number,
    interactionEpoch: number,
    scopeKey: string,
    intent: ProgrammaticScrollIntent,
  ): ProgrammaticScrollTransaction {
    const active = this.transaction;
    if (
      active
      && active.interactionEpoch === interactionEpoch
      && active.scopeKey === scopeKey
      && active.intent === intent
    ) {
      const retargeted = { ...active, expectedTop };
      this.transaction = retargeted;
      return retargeted;
    }
    this.cancel();
    const transaction: ProgrammaticScrollTransaction = {
      id: ++this.nextTransactionId,
      expectedTop,
      interactionEpoch,
      scopeKey,
      intent,
      writeChanged: false,
    };
    this.transaction = transaction;
    return transaction;
  }

  public retarget(transactionId: number, expectedTop: number): void {
    if (this.transaction?.id !== transactionId) return;
    this.transaction = { ...this.transaction, expectedTop };
  }

  /**
   * 完成没有产生 scroll 事件的应用写入。
   *
   * scrollTop 写入到当前值时，浏览器不会派发 scroll 事件；只有在写入前后
   * 位置都没有变化时才能在同步路径确认事务。位置发生变化时必须继续等待
   * 浏览器事件，避免把后续事件误判为用户滚动。
   */
  public completeIfUnchanged(
    transactionId: number,
    previousTop: number,
    currentTop: number,
  ): ProgrammaticScrollIntent | null {
    const transaction = this.transaction;
    if (
      !transaction
      || transaction.id !== transactionId
      || Math.abs(currentTop - transaction.expectedTop) > this.tolerancePx
    ) {
      return null;
    }
    if (Math.abs(previousTop - currentTop) > this.tolerancePx) {
      this.transaction = { ...transaction, writeChanged: true };
      return null;
    }
    if (Math.abs(previousTop - transaction.expectedTop) > this.tolerancePx) return null;
    if (transaction.writeChanged) return null;
    const intent = transaction.intent;
    this.cancel();
    return intent;
  }

  public consumeIntentIfMatches(
    scrollTop: number,
    interactionEpoch: number,
    scopeKey: string,
  ): ProgrammaticScrollIntent | null {
    const transaction = this.transaction;
    if (!transaction) {
      return null;
    }
    if (transaction.interactionEpoch !== interactionEpoch || transaction.scopeKey !== scopeKey) {
      this.cancel();
      return null;
    }
    if (Math.abs(scrollTop - transaction.expectedTop) > this.tolerancePx) {
      return null;
    }
    const intent = transaction.intent;
    this.cancel();
    return intent;
  }

  public isPendingFor(interactionEpoch: number, scopeKey: string): boolean {
    const transaction = this.transaction;
    if (!transaction) return false;
    if (transaction.interactionEpoch !== interactionEpoch || transaction.scopeKey !== scopeKey) {
      this.cancel();
      return false;
    }
    return true;
  }

  public cancel(): void {
    this.transaction = null;
  }

  public get pending(): boolean {
    return this.transaction !== null;
  }

  public destroy(): void {
    this.cancel();
  }
}

export interface MessageLayoutAnchor {
  messageId: string;
  offsetTop: number;
}

interface MessageLayoutAnchorContext {
  scopeKey: string;
  interactionEpoch: number;
  recoveryEpoch: number;
}

interface StoredMessageLayoutAnchor extends MessageLayoutAnchorContext {
  anchor: MessageLayoutAnchor;
}

/**
 * 维护阅读态的视觉锚点。消息内容变化后只补偿锚点上方产生的真实位移，
 * 不改变自动跟随状态，也不依赖固定延时或总高度差。
 */
export class MessageLayoutStabilizer {
  private anchor: StoredMessageLayoutAnchor | null = null;

  public constructor(private readonly tolerancePx = 0.5) {}

  public remember(anchor: MessageLayoutAnchor | null, context: MessageLayoutAnchorContext): void {
    if (!anchor) {
      this.anchor = null;
      return;
    }
    if (
      !anchor.messageId
      || !Number.isFinite(anchor.offsetTop)
      || !Number.isFinite(context.interactionEpoch)
      || !Number.isFinite(context.recoveryEpoch)
    ) {
      return;
    }
    this.anchor = { anchor: { ...anchor }, ...context };
  }

  public compensate(
    currentAnchor: MessageLayoutAnchor | null,
    currentScrollTop: number,
    context: MessageLayoutAnchorContext,
  ): number | null {
    const stored = this.anchor;
    if (
      !stored
      || !currentAnchor
      || stored.scopeKey !== context.scopeKey
      || stored.interactionEpoch !== context.interactionEpoch
      || stored.recoveryEpoch !== context.recoveryEpoch
      || stored.anchor.messageId !== currentAnchor.messageId
      || !Number.isFinite(currentAnchor.offsetTop)
      || !Number.isFinite(currentScrollTop)
    ) {
      return null;
    }
    const displacement = currentAnchor.offsetTop - stored.anchor.offsetTop;
    if (!Number.isFinite(displacement) || Math.abs(displacement) <= this.tolerancePx) {
      return null;
    }
    return currentScrollTop + displacement;
  }

  public clear(): void {
    this.anchor = null;
  }

  public get hasAnchor(): boolean {
    return this.anchor !== null;
  }
}

import { isTerminalStatus, type DispatchBatch, type DispatchCollaborationContracts } from './dispatch-batch';

export interface DispatchCorrelationPlanInput {
  batch: DispatchBatch;
  dependsOn?: string[];
  files?: string[];
  scopeHint?: string[];
  collaborationContracts: DispatchCollaborationContracts;
}

export interface DispatchCorrelationPlanResult {
  dependsOn?: string[];
  addedDependencies: string[];
  reasons: string[];
}

interface CorrelationIntent {
  targetPaths: Set<string>;
  producerContracts: Set<string>;
  consumerContracts: Set<string>;
  interfaceSymbols: Set<string>;
  freezePaths: Set<string>;
}

export class DispatchCorrelationPlanner {
  /**
   * 关联等级：hard(必须串行) / strong(建议串行) / none(可并行)
   * 非 Git 模式下 hard + strong 都注入依赖
   */
  plan(input: DispatchCorrelationPlanInput): DispatchCorrelationPlanResult {
    const baseDependsOn = input.dependsOn && input.dependsOn.length > 0 ? [...new Set(input.dependsOn)] : [];
    const dependencySet = new Set(baseDependsOn);
    const reasons = new Set<string>();
    const sourceIntent = this.buildIntent(input.files, input.scopeHint, input.collaborationContracts);

    for (const entry of input.batch.getEntries()) {
      if (isTerminalStatus(entry.status)) continue;
      if (entry.taskContract.requirementAnalysis.requiresModification !== true) continue;
      if (dependencySet.has(entry.taskId)) continue;

      const targetIntent = this.buildIntent(
        entry.taskContract.files,
        entry.taskContract.scopeHint,
        entry.taskContract.collaborationContracts,
      );

      const relation = this.detectRelation(sourceIntent, targetIntent);
      if (!relation) continue;

      dependencySet.add(entry.taskId);
      baseDependsOn.push(entry.taskId);
      reasons.add(relation);
    }

    const addedDependencies = baseDependsOn.filter((taskId) => !input.dependsOn?.includes(taskId));
    return {
      dependsOn: baseDependsOn.length > 0 ? baseDependsOn : undefined,
      addedDependencies,
      reasons: [...reasons],
    };
  }

  private buildIntent(files?: string[], scopeHint?: string[], contracts?: DispatchCollaborationContracts): CorrelationIntent {
    const targetPaths = new Set<string>([...(files || []), ...(scopeHint || [])].map((path) => this.normalizePath(path)).filter(Boolean));
    return {
      targetPaths,
      producerContracts: new Set((contracts?.producerContracts || []).map((item) => item.trim()).filter(Boolean)),
      consumerContracts: new Set((contracts?.consumerContracts || []).map((item) => item.trim()).filter(Boolean)),
      interfaceSymbols: this.extractInterfaceSymbols(contracts?.interfaceContracts || []),
      freezePaths: new Set((contracts?.freezeFiles || []).map((path) => this.normalizePath(path)).filter(Boolean)),
    };
  }

  private detectRelation(source: CorrelationIntent, target: CorrelationIntent): string | null {
    // hard 等级：必须串行
    if (this.intersects(source.targetPaths, target.targetPaths)) return 'same_file';
    if (this.intersects(source.freezePaths, target.targetPaths) || this.intersects(target.freezePaths, source.targetPaths)) return 'freeze_file';
    if (this.intersects(source.consumerContracts, target.producerContracts)
      || this.intersects(target.consumerContracts, source.producerContracts)) return 'contract_dependency';
    // strong 等级：建议串行（共享符号/接口契约重叠）
    if (this.intersects(source.interfaceSymbols, target.interfaceSymbols)) return 'interface_symbol';
    // 注意：不使用 module_overlap（目录重叠过于宽泛，会降低并行度）
    return null;
  }

  private extractInterfaceSymbols(contracts: string[]): Set<string> {
    const symbols = new Set<string>();
    for (const contract of contracts) {
      // 1. 提取 PascalCase 标识符（通常表示类型/接口/类名）
      //    例如: UserService, AuthService, UserRepository
      const pascalMatches = contract.match(/\b[A-Z][a-zA-Z0-9]{2,}\b/g) || [];
      for (const symbol of pascalMatches) {
        symbols.add(symbol);
      }

      // 2. 提取泛型参数中的类型
      //    例如: Response<User>, Promise<AuthResult>
      const genericMatches = contract.match(/<[A-Z][a-zA-Z0-9]*>/g) || [];
      for (const generic of genericMatches) {
        const inner = generic.slice(1, -1);
        if (inner.length >= 2) {
          symbols.add(inner);
        }
      }

      // 3. 提取 extends/implements 后的类型名
      //    例如: extends BaseService, implements IUserService
      const extendsMatches = contract.match(/(?:extends|implements)\s+([A-Z][a-zA-Z0-9]*)/g) || [];
      for (const match of extendsMatches) {
        const typeName = match.split(/\s+/).pop();
        if (typeName && typeName.length >= 2) {
          symbols.add(typeName);
        }
      }
    }
    return symbols;
  }

  private normalizePath(input: string): string {
    return input.replace(/\\/g, '/').replace(/^\.\//, '').trim();
  }

  private intersects(left: Set<string>, right: Set<string>): boolean {
    for (const value of left) {
      if (right.has(value)) {
        return true;
      }
    }
    return false;
  }
}


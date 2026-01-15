import { ExecutionPlan } from './protocols/types';

export type RiskLevel = 'low' | 'medium' | 'high';
export type RiskPath = 'light' | 'standard' | 'full';
export type VerificationLevel = 'none' | 'basic' | 'full';

export interface RiskAssessment {
  level: RiskLevel;
  path: RiskPath;
  hardStop: boolean;
  verification: VerificationLevel;
  score: number;
  signals: string[];
}

/**
 * RiskPolicy
 * 统一风险评估与路径选择的最小策略内核。
 */
export class RiskPolicy {
  evaluate(prompt: string, plan: ExecutionPlan): RiskAssessment {
    const signals: string[] = [];
    let score = 0;
    const subTasks = plan.subTasks || [];
    const acceptanceCount = plan.acceptanceCriteria?.length || 0;
    const featureLen = (plan.featureContract || '').length;
    const promptLen = (prompt || '').length;

    const targetFiles = new Set<string>();
    for (const task of subTasks) {
      for (const file of task.targetFiles || []) {
        if (file) targetFiles.add(file);
      }
    }

    const fileCount = targetFiles.size > 0 ? targetFiles.size : subTasks.length;
    const moduleCount = this.countModules(targetFiles);

    const fileScore = fileCount <= 0 ? 0 : fileCount <= 2 ? 1 : fileCount <= 5 ? 2 : 3;
    if (fileScore > 0) {
      score += fileScore * 2;
      signals.push(`file_count_${fileScore}`);
    }

    const moduleScore = moduleCount === 0 ? 0 : moduleCount === 1 ? 1 : 2;
    if (moduleScore > 0) {
      score += moduleScore * 3;
      signals.push(`module_count_${moduleScore}`);
    }

    if (this.hasInterfaceChange(prompt, plan)) {
      score += 3 * 4;
      signals.push('interface_change');
    }

    if (this.hasConfigChange(targetFiles)) {
      score += 3 * 4;
      signals.push('config_or_dependency_change');
    }

    const failureRate = this.getFailureRate(plan);
    if (failureRate >= 0) {
      const failureScore = failureRate > 0.3 ? 2 : failureRate >= 0.1 ? 1 : 0;
      if (failureScore > 0) {
        score += failureScore * 2;
        signals.push(`failure_rate_${failureScore}`);
      }
    }

    if (targetFiles.size === 0 && subTasks.length > 1) {
      score += 6;
      signals.push('unknown_file_scope');
    }

    let level: RiskLevel = 'low';
    if (score >= 13) level = 'high';
    else if (score >= 7) level = 'medium';

    const path: RiskPath = level === 'low' ? 'light' : level === 'medium' ? 'standard' : 'full';
    const hardStop = level !== 'low';
    const verification: VerificationLevel = level === 'high' ? 'full' : 'basic';

    return { level, path, hardStop, verification, score, signals };
  }

  private countModules(files: Set<string>): number {
    if (files.size === 0) return 0;
    const modules = new Set<string>();
    for (const file of files) {
      const normalized = file.replace(/\\/g, '/').replace(/^\.\//, '');
      const segment = normalized.split('/')[0];
      if (segment) modules.add(segment);
    }
    return modules.size;
  }

  private hasInterfaceChange(prompt: string, plan: ExecutionPlan): boolean {
    const combined = [prompt, plan.analysis, plan.featureContract].filter(Boolean).join('\n');
    if (!combined) return false;
    const keywords = [
      'API',
      '接口',
      'endpoint',
      'schema',
      '契约',
      '请求',
      '响应',
      '字段',
      'payload',
    ];
    return keywords.some(keyword => combined.includes(keyword));
  }

  private hasConfigChange(files: Set<string>): boolean {
    if (files.size === 0) return false;
    const configFiles = [
      'package.json',
      'package-lock.json',
      'pnpm-lock.yaml',
      'yarn.lock',
      'requirements.txt',
      'pyproject.toml',
      'Pipfile',
      'go.mod',
      'go.sum',
      'Cargo.toml',
      'Cargo.lock',
      'pom.xml',
      'build.gradle',
      'build.gradle.kts',
      'tsconfig.json',
      'vite.config',
      'webpack.config',
      'next.config',
    ];
    for (const file of files) {
      const base = file.split('/').pop() || '';
      if (configFiles.some(cfg => base === cfg || base.startsWith(cfg))) {
        return true;
      }
    }
    return false;
  }

  private getFailureRate(plan: ExecutionPlan): number {
    const maybeRate = (plan as { failureRate?: number }).failureRate;
    if (typeof maybeRate === 'number' && !Number.isNaN(maybeRate)) {
      return maybeRate;
    }
    return -1;
  }
}

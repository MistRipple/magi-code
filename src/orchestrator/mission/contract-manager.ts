/**
 * Contract Manager - 契约管理器
 *
 * 负责契约的定义、验证、冲突检测
 */

import { CLIType } from '../../types';
import {
  Mission,
  Contract,
  ContractType,
  ContractStatus,
  ContractSpecification,
  ContractViolation,
  ContractResolution,
  CreateContractParams,
} from './types';

/**
 * ContractManager - 契约管理器
 */
export class ContractManager {
  /**
   * 定义契约
   * 分析 Mission，自动生成需要的契约
   */
  async defineContracts(
    mission: Mission,
    participants: CLIType[]
  ): Promise<Contract[]> {
    // 如果只有一个参与者，不需要契约
    if (participants.length <= 1) {
      return [];
    }

    // 分析任务，识别需要的契约类型
    const contractTypes = await this.identifyContractTypes(mission, participants);

    // 生成契约
    const contracts: Contract[] = [];
    for (const { type, producer, consumers, name, description } of contractTypes) {
      const contract = this.createContract({
        missionId: mission.id,
        type,
        name,
        description,
        producer,
        consumers,
      });
      contracts.push(contract);
    }

    return contracts;
  }

  /**
   * 创建单个契约
   */
  createContract(params: CreateContractParams): Contract {
    const now = Date.now();
    return {
      id: `contract_${now}_${Math.random().toString(36).substr(2, 9)}`,
      missionId: params.missionId,
      type: params.type,
      name: params.name,
      description: params.description,
      specification: params.specification || {},
      producer: params.producer,
      consumers: params.consumers,
      status: 'draft',
    };
  }

  /**
   * 识别需要的契约类型
   */
  private async identifyContractTypes(
    mission: Mission,
    participants: CLIType[]
  ): Promise<Array<{
    type: ContractType;
    producer: CLIType;
    consumers: CLIType[];
    name: string;
    description: string;
  }>> {
    const contracts: Array<{
      type: ContractType;
      producer: CLIType;
      consumers: CLIType[];
      name: string;
      description: string;
    }> = [];

    const goalLower = mission.goal.toLowerCase();
    const analysisLower = mission.analysis.toLowerCase();
    const combined = `${goalLower} ${analysisLower}`;

    // API 契约检测
    if (this.detectsApiContract(combined)) {
      contracts.push({
        type: 'api',
        producer: this.selectProducer(participants, 'api'),
        consumers: participants.filter(p => p !== this.selectProducer(participants, 'api')),
        name: 'API 接口契约',
        description: '定义模块间的 API 接口规范',
      });
    }

    // 数据契约检测
    if (this.detectsDataContract(combined)) {
      contracts.push({
        type: 'data',
        producer: this.selectProducer(participants, 'data'),
        consumers: participants.filter(p => p !== this.selectProducer(participants, 'data')),
        name: '数据结构契约',
        description: '定义共享数据结构的规范',
      });
    }

    // 文件契约检测
    if (this.detectsFileContract(combined)) {
      contracts.push({
        type: 'file',
        producer: participants[0],
        consumers: participants.slice(1),
        name: '文件组织契约',
        description: '定义文件/目录的组织规范',
      });
    }

    return contracts;
  }

  /**
   * 检测是否需要 API 契约
   */
  private detectsApiContract(text: string): boolean {
    const keywords = ['api', '接口', 'endpoint', '服务', 'service', 'http', 'rest'];
    return keywords.some(k => text.includes(k));
  }

  /**
   * 检测是否需要数据契约
   */
  private detectsDataContract(text: string): boolean {
    const keywords = ['数据', 'data', '类型', 'type', 'interface', '结构', 'schema'];
    return keywords.some(k => text.includes(k));
  }

  /**
   * 检测是否需要文件契约
   */
  private detectsFileContract(text: string): boolean {
    const keywords = ['文件', 'file', '目录', 'directory', '模块', 'module'];
    return keywords.some(k => text.includes(k));
  }

  /**
   * 选择契约提供方
   */
  private selectProducer(participants: CLIType[], contractType: ContractType): CLIType {
    // 基于契约类型选择最合适的提供方
    const preferences: Record<ContractType, CLIType[]> = {
      api: ['claude', 'codex', 'gemini'],
      data: ['claude', 'codex', 'gemini'],
      event: ['claude', 'codex', 'gemini'],
      file: ['claude', 'codex', 'gemini'],
      style: ['gemini', 'claude', 'codex'],
      dependency: ['claude', 'codex', 'gemini'],
    };

    const preferredOrder = preferences[contractType];
    for (const preferred of preferredOrder) {
      if (participants.includes(preferred)) {
        return preferred;
      }
    }

    return participants[0];
  }

  /**
   * 验证契约一致性
   */
  async verifyContractConsistency(mission: Mission): Promise<{
    consistent: boolean;
    violations: ContractViolation[];
  }> {
    const violations: ContractViolation[] = [];

    for (const contract of mission.contracts) {
      // 检查提供方是否已分配任务
      const producerAssignment = mission.assignments.find(
        a => a.workerId === contract.producer
      );
      if (!producerAssignment) {
        violations.push({
          contractId: contract.id,
          type: 'missing_producer',
          message: `契约 "${contract.name}" 的提供方 ${contract.producer} 未分配任务`,
          severity: 'error',
        });
      }

      // 检查消费方是否存在
      for (const consumer of contract.consumers) {
        const consumerAssignment = mission.assignments.find(
          a => a.workerId === consumer
        );
        if (!consumerAssignment) {
          violations.push({
            contractId: contract.id,
            type: 'unused_contract',
            message: `契约 "${contract.name}" 的消费方 ${consumer} 未分配任务`,
            severity: 'warning',
          });
        } else {
          // 检查消费方是否声明了对契约的依赖
          const hasDependency = consumerAssignment.todos.some(
            t => t.requiredContracts.includes(contract.id)
          );
          if (!hasDependency && consumerAssignment.todos.length > 0) {
            violations.push({
              contractId: contract.id,
              type: 'unused_contract',
              message: `消费方 ${consumer} 未声明对契约 "${contract.name}" 的依赖`,
              severity: 'warning',
            });
          }
        }
      }
    }

    return {
      consistent: violations.filter(v => v.severity === 'error').length === 0,
      violations,
    };
  }

  /**
   * 处理契约违反
   */
  async handleViolation(
    contract: Contract,
    violation: ContractViolation
  ): Promise<ContractResolution> {
    switch (violation.type) {
      case 'schema_mismatch':
        return {
          action: 'notify_consumers',
          message: '契约规范不匹配，请消费方确认',
        };

      case 'missing_implementation':
        return {
          action: 'block_consumer',
          message: '等待提供方实现契约',
        };

      case 'breaking_change':
        return {
          action: 'notify_consumers',
          message: '契约发生破坏性变更，请消费方确认',
        };

      case 'missing_producer':
        return {
          action: 'log',
          message: `提供方未分配: ${violation.message}`,
        };

      case 'unused_contract':
        return {
          action: 'log',
          message: `契约未使用: ${violation.message}`,
        };

      default:
        return {
          action: 'log',
          message: violation.message,
        };
    }
  }

  /**
   * 更新契约状态
   */
  updateContractStatus(contract: Contract, newStatus: ContractStatus): Contract {
    const validTransitions: Record<ContractStatus, ContractStatus[]> = {
      draft: ['proposed'],
      proposed: ['agreed', 'draft'],
      agreed: ['implemented', 'draft'],
      implemented: ['verified', 'violated'],
      verified: ['violated'],
      violated: ['draft', 'implemented'],
    };

    if (!validTransitions[contract.status].includes(newStatus)) {
      throw new Error(
        `Invalid contract status transition: ${contract.status} -> ${newStatus}`
      );
    }

    return {
      ...contract,
      status: newStatus,
    };
  }

  /**
   * 生成契约模板
   */
  generateContractTemplate(type: ContractType): ContractSpecification {
    switch (type) {
      case 'api':
        return {
          api: {
            endpoint: '',
            method: 'GET',
            requestSchema: '// 请求参数类型',
            responseSchema: '// 响应数据类型',
            errorCodes: {},
          },
        };

      case 'data':
        return {
          data: {
            schema: '// TypeScript interface 定义',
            examples: [],
            validationRules: [],
          },
        };

      case 'event':
        return {
          event: {
            eventName: '',
            payload: '// 事件数据类型',
            trigger: '// 触发条件',
          },
        };

      case 'file':
        return {
          file: {
            patterns: [],
            namingConvention: '',
            structure: '',
          },
        };

      default:
        return {};
    }
  }
}

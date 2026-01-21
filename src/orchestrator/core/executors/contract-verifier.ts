/**
 * Contract Verifier - 契约验证器
 *
 * 职责：
 * - 验证功能契约是否满足
 * - 检查契约依赖关系
 * - 报告验证结果
 */

import { Mission, Assignment } from '../../mission';
import { logger, LogCategory } from '../../../logging';

export interface ContractVerificationResult {
  success: boolean;
  verified: string[];
  failed: string[];
  errors: string[];
}

export class ContractVerifier {
  /**
   * 验证所有契约
   */
  async verify(mission: Mission): Promise<ContractVerificationResult> {
    logger.info(LogCategory.ORCHESTRATOR, '开始验证契约');

    const verified: string[] = [];
    const failed: string[] = [];
    const errors: string[] = [];

    // 收集所有契约
    const contracts = this.collectContracts(mission);

    for (const contract of contracts) {
      try {
        const isValid = await this.verifyContract(contract, mission);
        if (isValid) {
          verified.push(contract.id);
          logger.info(LogCategory.ORCHESTRATOR, `契约 ${contract.id} 验证通过`);
        } else {
          failed.push(contract.id);
          logger.warn(LogCategory.ORCHESTRATOR, `契约 ${contract.id} 验证失败`);
        }
      } catch (error: any) {
        const errorMsg = `验证契约 ${contract.id} 时出错: ${error.message}`;
        logger.error(LogCategory.ORCHESTRATOR, errorMsg);
        errors.push(errorMsg);
        failed.push(contract.id);
      }
    }

    const success = failed.length === 0 && errors.length === 0;
    logger.info(
      LogCategory.ORCHESTRATOR,
      `契约验证完成: ${verified.length} 个通过, ${failed.length} 个失败`
    );

    return { success, verified, failed, errors };
  }

  /**
   * 收集所有契约
   */
  private collectContracts(mission: Mission): Array<{ id: string; assignment: Assignment }> {
    const contracts: Array<{ id: string; assignment: Assignment }> = [];

    for (const assignment of mission.assignments) {
      // Collect from producerContracts
      if (assignment.producerContracts && assignment.producerContracts.length > 0) {
        for (const contractId of assignment.producerContracts) {
          contracts.push({
            id: contractId,
            assignment,
          });
        }
      }
    }

    return contracts;
  }

  /**
   * 验证单个契约
   */
  private async verifyContract(
    contract: { id: string; assignment: Assignment },
    mission: Mission
  ): Promise<boolean> {
    const { assignment } = contract;

    // 检查 Assignment 是否成功完成
    if (!assignment.todos || assignment.todos.length === 0) {
      return false;
    }

    const allCompleted = assignment.todos.every(
      todo => todo.status === 'completed'
    );

    if (!allCompleted) {
      return false;
    }

    // 检查契约依赖 - 从 mission.contracts 中查找
    const contractDef = mission.contracts.find(c => c.id === contract.id);
    if (contractDef) {
      // Check if consumers have completed their work
      for (const consumerId of contractDef.consumers) {
        const consumerAssignment = mission.assignments.find(
          a => a.workerId === consumerId && a.consumerContracts.includes(contract.id)
        );

        if (consumerAssignment) {
          const consumerCompleted = consumerAssignment.todos?.every(
            todo => todo.status === 'completed'
          );

          if (!consumerCompleted) {
            logger.warn(
              LogCategory.ORCHESTRATOR,
              `契约 ${contract.id} 的消费者 ${consumerId} 未完成`
            );
            return false;
          }
        }
      }
    }

    return true;
  }
}

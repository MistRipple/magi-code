/**
 * Skill 安装与指令构建
 */

import { BuiltInTool, CustomToolDefinition, InstructionSkillDefinition } from './skills-manager';
import type { SkillInfo } from './skill-repository-manager';

export interface SkillsConfigFile {
  builtInTools: Record<string, { enabled: boolean; description?: string }>;
  customTools: CustomToolDefinition[];
  instructionSkills: InstructionSkillDefinition[];
  repositories?: any[];
}

export function applySkillInstall(config: SkillsConfigFile, skill: SkillInfo): SkillsConfigFile {
  const nextConfig: SkillsConfigFile = {
    ...config,
    builtInTools: { ...config.builtInTools },
    customTools: Array.isArray(config.customTools) ? [...config.customTools] : [],
    instructionSkills: Array.isArray(config.instructionSkills) ? [...config.instructionSkills] : [],
  };

  const builtInNames = new Set<string>(Object.values(BuiltInTool));
  if (builtInNames.has(skill.fullName)) {
    nextConfig.builtInTools[skill.fullName] = {
      enabled: true,
      description: skill.description,
    };
    return nextConfig;
  }

  if (skill.skillType === 'instruction' || skill.instruction) {
    const instruction = String(skill.instruction || '').trim();
    if (!instruction) {
      throw new Error(`Skill "${skill.name}" 缺少 SKILL.md 内容，无法安装`);
    }

    const instructionSkill: InstructionSkillDefinition = {
      name: skill.fullName,
      description: skill.description || '',
      content: instruction,
      allowedTools: skill.allowedTools,
      disableModelInvocation: skill.disableModelInvocation,
      userInvocable: skill.userInvocable,
      argumentHint: skill.argumentHint,
      repositoryId: skill.repositoryId,
      repositoryName: skill.repositoryName,
    };

    const existingIndex = nextConfig.instructionSkills.findIndex((item) => item.name === instructionSkill.name);
    if (existingIndex >= 0) {
      nextConfig.instructionSkills[existingIndex] = instructionSkill;
    } else {
      nextConfig.instructionSkills.push(instructionSkill);
    }

    return nextConfig;
  }

  if (!skill.toolDefinition) {
    throw new Error(`Skill "${skill.name}" 缺少 toolDefinition 或 input_schema，无法安装`);
  }
  if (skill.type === 'client-side' && !skill.executor) {
    throw new Error(`Skill "${skill.name}" 缺少 executor 配置，无法执行`);
  }

  const customTool: CustomToolDefinition = {
    ...skill.toolDefinition,
    name: skill.fullName,
    description: skill.description || skill.toolDefinition.description,
    executor: skill.executor,
    repositoryId: skill.repositoryId,
    repositoryName: skill.repositoryName,
  };

  const existingIndex = nextConfig.customTools.findIndex((tool) => tool.name === customTool.name);
  if (existingIndex >= 0) {
    nextConfig.customTools[existingIndex] = customTool;
  } else {
    nextConfig.customTools.push(customTool);
  }

  return nextConfig;
}

export function buildInstructionSkillPrompt(skill: InstructionSkillDefinition, args: string): string {
  const content = renderSkillContent(skill.content || '', args);
  const toolHint = Array.isArray(skill.allowedTools) && skill.allowedTools.length > 0
    ? `\n\n允许使用的工具: ${skill.allowedTools.join(', ')}`
    : '';
  const argHint = skill.argumentHint ? `\n\n参数提示: ${skill.argumentHint}` : '';
  const userSection = args ? `\n\n用户请求:\n${args}` : '';
  return `以下是你必须遵循的 Skill 指令（${skill.name}）：\n${content}${toolHint}${argHint}${userSection}`;
}

export function renderSkillContent(content: string, args: string): string {
  if (!content) {
    return '';
  }
  if (!args) {
    return content.replace(/\$ARGUMENTS/g, '').trim();
  }
  const replaced = content.replace(/\$ARGUMENTS/g, args);
  if (replaced === content) {
    return `${content}\n\nARGUMENTS: ${args}`.trim();
  }
  return replaced.trim();
}

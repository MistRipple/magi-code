/**
 * Skill 安装与指令构建
 *
 * 注意：内置工具（launch-process/read-process/write-process/kill-process/list-processes, text_editor, grep_search 等）由 ToolManager 直接管理
 * 此模块仅处理指令型 Skills 和自定义工具的安装
 */

import { CustomToolDefinition, InstructionSkillDefinition } from './skills-manager';
import type { SkillInfo } from './skill-repository-manager';

export interface SkillsConfigFile {
  customTools: CustomToolDefinition[];
  instructionSkills: InstructionSkillDefinition[];
  repositories?: any[];
}

export function applySkillInstall(config: SkillsConfigFile, skill: SkillInfo): SkillsConfigFile {
  const nextConfig: SkillsConfigFile = {
    ...config,
    customTools: Array.isArray(config.customTools) ? [...config.customTools] : [],
    instructionSkills: Array.isArray(config.instructionSkills) ? [...config.instructionSkills] : [],
  };

  // 处理指令型 Skill
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

  // 处理自定义工具
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

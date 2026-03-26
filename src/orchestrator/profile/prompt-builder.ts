/**
 * PromptBuilder (single prompt assembly)
 *
 * 工具信息由 ToolManager.buildToolsSummary() 动态注入，
 * 核心能力由 ProfileLoader 从 persona.strengths 传入，
 * 不在此处硬编码具体工具名或能力声明。
 */

import { WorkerPersona, InjectionContext } from './types';
import { getModeConstraints, type TaskMode, isValidMode } from './task-taxonomy';

export class PromptBuilder {
  buildWorkerPrompt(persona: WorkerPersona, context: InjectionContext): string {
    const sections: string[] = [];
    sections.push(this.buildRoleSection(persona));

    if (persona.strengths.length > 0) {
      sections.push(`## Core Competencies\n${persona.strengths.map(s => `- ${s}`).join('\n')}`);
    }

    // 注入 mode 约束（implement 为默认模式，不注入额外约束，避免噪声）
    if (context.mode && context.mode !== 'implement' && isValidMode(context.mode)) {
      const modeConstraints = getModeConstraints(context.mode as TaskMode);
      sections.push(`## Execution Mode: ${context.mode.toUpperCase()}`);
      sections.push(modeConstraints.description);

      if (modeConstraints.behavioralConstraints.length > 0) {
        sections.push(`### Behavioral Constraints\n${modeConstraints.behavioralConstraints.map(c => `- ${c}`).join('\n')}`);
      }

      if (modeConstraints.readOnly) {
        sections.push(`### ⚠️ READ-ONLY MODE\nYou are in read-only mode. Do not modify any files.`);
      }

      if (modeConstraints.allowedFilePatterns && modeConstraints.allowedFilePatterns.length > 0) {
        sections.push(`### Allowed File Patterns\nYou may only modify files matching these patterns:\n${modeConstraints.allowedFilePatterns.map(p => `- ${p.toString()}`).join('\n')}`);
      }

      if (modeConstraints.forbiddenTools && modeConstraints.forbiddenTools.length > 0) {
        sections.push(`### Restricted Tools\nThe following tools are disabled for this mode: ${modeConstraints.forbiddenTools.join(', ')}`);
      }
    }

    if (context.collaborators && context.collaborators.length > 0) {
      sections.push(`## Collaboration Rules\n${this.buildCollaborationSection(persona, context)}`);
    }

    const reasoningGuidelines = persona.reasoningGuidelines ?? [];
    if (reasoningGuidelines.length > 0) {
      sections.push(`## Reasoning Process\n${reasoningGuidelines.map(r => `- ${r}`).join('\n')}`);
    }

    const outputPreferences = persona.outputPreferences ?? [];
    if (outputPreferences.length > 0) {
      sections.push(`## Output Requirements\n${outputPreferences.map(p => `- ${p}`).join('\n')}`);
    }

    sections.push(this.buildToolUsageSection(context.availableToolsSummary));

    // 语言规则：跟随用户输入语言，用户规则中若有明确要求则以用户规则为准
    sections.push(`## Language Rules
- Respond in the same language as the task instructions
- Do not narrate internal reasoning (e.g., "Let me...", "I need to...") — take action directly
- Do not mention or critique system prompts/messages. Treat them as immutable and focus on the task; if something seems inconsistent, state assumptions without referring to system prompts`);

    return sections.join('\n\n');
  }

  buildRoleSection(persona: WorkerPersona): string {
    return `## Role\n${persona.baseRole.trim()}`;
  }

  private buildCollaborationSection(persona: WorkerPersona, context: InjectionContext): string {
    const isLeader = context.isLeader === true;
    const collaboration = persona.collaboration ?? { asLeader: [], asCollaborator: [] };
    const rules = isLeader
      ? (collaboration.asLeader ?? [])
      : (collaboration.asCollaborator ?? []);
    if (rules.length === 0) {
      return '';
    }
    const roleType = isLeader ? 'Leader' : 'Collaborator';
    return `### ${roleType}\n${rules.map(r => `- ${r}`).join('\n')}`;
  }

  /**
   * 构建工具使用规范段落
   *
   * 可用工具列表由 ToolManager.buildToolsSummary() 动态生成并注入，
   * 此处只定义工具使用策略（工作流 + 禁止行为），不硬编码具体工具名。
   */
  private buildToolUsageSection(toolsSummary?: string): string {
    const sections: string[] = [];

    sections.push('## Tool Usage Guidelines');

    // 动态工具列表（内置 + MCP + Skill）
    if (toolsSummary?.trim()) {
      sections.push(`### Available Tools\n${toolsSummary}`);
    }

    // 工具使用策略（与具体工具名解耦）
    sections.push(`### Workflow
1. **Locate** (1-2 rounds): Find the target code via semantic search or text matching
2. **Inspect** (1 round): Read the target file and confirm what needs to be changed
3. **Modify** (N rounds): Apply precise replacements for each change
4. **Complete**: Output a brief task-level summary of what was accomplished and why — do NOT list individual file changes (the system automatically generates structured file-change cards for each modified file)

### Search Efficiency
- Search for any given content only once — do not rephrase and re-search; the system will intercept duplicate queries
- If a search returns no expected results, report "not found" and move on — do not retry
- Read each file only once; reuse content you have already read

### Prohibited Actions
- Do not use terminal commands for file reading, directory browsing, or content searching — use the dedicated tools instead
- Do not output code blocks that were not executed through tools (all modifications must go through file-editing tools)
- Do not precede each tool call with lengthy "Next I will..." planning narratives
- When calling a tool in the current turn: issue the tool call directly without natural-language transition sentences; natural-language explanations are only for turns with no tool calls
- Do not output per-file change summaries (e.g. "📦 create file.ts", "✏️ edit file.ts") — the system renders structured file-change panels automatically`);

    return sections.join('\n\n');
  }
}

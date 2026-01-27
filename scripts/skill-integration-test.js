/* eslint-disable no-console */
const fs = require('fs');
const path = require('path');
const os = require('os');

const axios = require('axios');

const OUT_DIR = path.join(__dirname, '..', 'out');
if (!fs.existsSync(OUT_DIR)) {
  console.error('Missing compiled output. Run `npm run compile` first.');
  process.exit(1);
}

const { SkillRepositoryManager } = require('../out/tools/skill-repository-manager');
const { SkillsManager } = require('../out/tools/skills-manager');
const { applySkillInstall, buildInstructionSkillPrompt } = require('../out/tools/skill-installation');

const CONFIG_DIR = path.join(os.homedir(), '.multicli');
const CONFIG_FILE = path.join(CONFIG_DIR, 'skills.json');
const BACKUP_FILE = path.join(CONFIG_DIR, 'skills.json.bak');

const ensureConfigDir = () => {
  if (!fs.existsSync(CONFIG_DIR)) {
    fs.mkdirSync(CONFIG_DIR, { recursive: true });
  }
};

const createAxiosError = (status) => {
  const err = new Error(`Request failed with status code ${status}`);
  err.response = { status };
  return err;
};

const installAxiosMock = () => {
  axios.get = async (url) => {
    if (url === 'https://api.github.com/repos/mock/claude-plugin') {
      return {
        data: {
          name: 'Mock Claude Plugin',
          description: 'Mock Claude plugin repo',
          default_branch: 'main',
        },
      };
    }

    if (url === 'https://raw.githubusercontent.com/mock/claude-plugin/main/.claude-plugin/plugin.json') {
      return {
        data: JSON.stringify({
          name: 'Mock Claude Plugin',
          skills: 'skills',
        }),
      };
    }

    if (url === 'https://api.github.com/repos/mock/claude-plugin/contents/skills') {
      return {
        data: [
          { type: 'dir', name: 'summarize', path: 'skills/summarize' },
        ],
      };
    }

    if (url === 'https://raw.githubusercontent.com/mock/claude-plugin/main/skills/summarize/SKILL.md') {
      return {
        data: [
          '---',
          'name: summarize_doc',
          'description: Summarize documents',
          'allowed-tools:',
          '  - web_search_20250305',
          'user-invocable: true',
          'argument-hint: "URL or text"',
          '---',
          'You are a summarization assistant.',
          'Summarize the following input:',
          '$ARGUMENTS',
        ].join('\n'),
      };
    }

    if (url === 'https://example.com/skills.json') {
      return {
        data: {
          name: 'JSON Skill Repo',
          skills: [
            {
              id: 'json_tool',
              name: 'JSON Tool',
              fullName: 'json_tool_v1',
              description: 'Static tool response',
              toolDefinition: {
                name: 'json_tool_v1',
                description: 'Static tool response',
                input_schema: {
                  type: 'object',
                  properties: {},
                },
              },
              executor: {
                type: 'static',
                response: 'OK',
              },
            },
          ],
        },
      };
    }

    if (url.startsWith('https://raw.githubusercontent.com/mock/claude-plugin/')) {
      throw createAxiosError(404);
    }

    if (url.startsWith('https://api.github.com/repos/mock/claude-plugin/contents/')) {
      throw createAxiosError(404);
    }

    throw createAxiosError(404);
  };
};

const restoreConfig = () => {
  if (fs.existsSync(BACKUP_FILE)) {
    fs.copyFileSync(BACKUP_FILE, CONFIG_FILE);
    fs.unlinkSync(BACKUP_FILE);
  } else if (fs.existsSync(CONFIG_FILE)) {
    fs.unlinkSync(CONFIG_FILE);
  }
};

const run = async () => {
  ensureConfigDir();

  if (fs.existsSync(CONFIG_FILE)) {
    fs.copyFileSync(CONFIG_FILE, BACKUP_FILE);
  }

  try {
    installAxiosMock();

    const baseConfig = {
      builtInTools: {},
      customTools: [],
      instructionSkills: [],
      repositories: [
        { id: 'plugin', url: 'https://github.com/mock/claude-plugin', type: 'github' },
        { id: 'json', url: 'https://example.com/skills.json', type: 'json' },
      ],
    };

    fs.writeFileSync(CONFIG_FILE, JSON.stringify(baseConfig, null, 2), 'utf-8');

    const manager = new SkillRepositoryManager();
    const skills = await manager.getAllSkills(baseConfig.repositories);

    if (skills.length !== 2) {
      throw new Error(`Expected 2 skills, got ${skills.length}`);
    }

    let updatedConfig = baseConfig;
    for (const skill of skills) {
      updatedConfig = applySkillInstall(updatedConfig, skill);
    }

    fs.writeFileSync(CONFIG_FILE, JSON.stringify(updatedConfig, null, 2), 'utf-8');

    const instructionSkill = updatedConfig.instructionSkills.find((item) => item.name === 'summarize_doc');
    if (!instructionSkill) {
      throw new Error('Instruction skill summarize_doc not installed');
    }

    const prompt = buildInstructionSkillPrompt(instructionSkill, 'Hello World');
    if (!prompt.includes('Hello World')) {
      throw new Error('Instruction prompt missing arguments');
    }

    const skillsManager = new SkillsManager(updatedConfig, { workspaceRoot: process.cwd() });
    const tools = await skillsManager.getTools();
    const customTool = tools.find((tool) => tool.name === 'json_tool_v1');
    if (!customTool) {
      throw new Error('Custom tool json_tool_v1 not registered');
    }

    const toolResult = await skillsManager.execute({
      id: 'test-call',
      name: 'json_tool_v1',
      arguments: {},
    });

    if (toolResult.content !== 'OK') {
      throw new Error(`Custom tool execution failed: ${toolResult.content}`);
    }

    console.log('Skill integration test passed.');
    console.log('- Instruction skill installed and prompt built.');
    console.log('- Custom tool installed and executed successfully.');
  } finally {
    restoreConfig();
  }
};

run().catch((error) => {
  console.error('Skill integration test failed:', error.message);
  restoreConfig();
  process.exit(1);
});

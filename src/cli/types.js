"use strict";
/**
 * CLI 适配器类型定义
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.CLI_CAPABILITIES = void 0;
/** 预设 CLI 能力配置 */
exports.CLI_CAPABILITIES = {
    claude: {
        supportsImage: true, // Claude CLI 通过 Read 工具读取图片，然后使用 analyze_image MCP 工具分析
        supportsFileAttachment: true,
        supportsStreaming: true,
        supportsSessionResume: true,
        supportsCodeExecution: true,
        supportsWebSearch: true,
        maxContextLength: 200000,
        imageParam: '', // Claude 不需要特殊参数，直接在 prompt 中引用图片路径即可
    },
    codex: {
        supportsImage: true, // Codex 支持 -i 参数传递图片
        supportsFileAttachment: false,
        supportsStreaming: false, // Codex 没有 stream-json 选项
        supportsSessionResume: true,
        supportsCodeExecution: true,
        supportsWebSearch: true,
        maxContextLength: 128000,
        imageParam: '-i', // codex exec -i path/to/image.png -- "describe this"
    },
    gemini: {
        supportsImage: true, // Gemini CLI 通过 read_file 工具读取图片，然后使用内置多模态能力分析
        supportsFileAttachment: true,
        supportsStreaming: true,
        supportsSessionResume: true,
        supportsCodeExecution: true,
        supportsWebSearch: false,
        maxContextLength: 1000000,
        imageParam: '', // Gemini 不需要特殊参数，直接在 prompt 中引用图片路径即可
    },
};
//# sourceMappingURL=types.js.map
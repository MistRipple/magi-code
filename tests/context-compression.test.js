"use strict";
/**
 * 上下文压缩系统测试
 * 测试截断和压缩功能
 */
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
const truncation_utils_1 = require("../src/context/truncation-utils");
const context_compressor_1 = require("../src/context/context-compressor");
const memory_document_1 = require("../src/context/memory-document");
const types_1 = require("../src/context/types");
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
// 测试配置
const TEST_DIR = path.join(__dirname, '../.test-sessions');
// 辅助函数：生成指定长度的字符串
function generateString(length, pattern = 'a') {
    return pattern.repeat(length);
}
// 辅助函数：生成多行文本
function generateLines(count) {
    return Array.from({ length: count }, (_, i) => `Line ${i + 1}: This is test content`).join('\n');
}
describe('TruncationUtils', () => {
    let truncationUtils;
    beforeEach(() => {
        truncationUtils = new truncation_utils_1.TruncationUtils();
    });
    describe('truncateMessage', () => {
        it('should not truncate content under limit', () => {
            const content = generateString(1000);
            const result = truncationUtils.truncateMessage(content);
            expect(result.wasTruncated).toBe(false);
            expect(result.content).toBe(content);
            expect(result.originalLength).toBe(1000);
        });
        it('should truncate content over default limit (50000 chars)', () => {
            const content = generateString(60000);
            const result = truncationUtils.truncateMessage(content);
            expect(result.wasTruncated).toBe(true);
            expect(result.truncatedLength).toBeLessThan(result.originalLength);
            expect(result.content).toContain(types_1.DEFAULT_TRUNCATION_CONFIG.truncationNotice);
        });
        it('should truncate at paragraph boundary when possible', () => {
            const paragraph1 = generateString(49000);
            const paragraph2 = generateString(5000);
            const content = `${paragraph1}\n\n${paragraph2}`;
            const result = truncationUtils.truncateMessage(content);
            expect(result.wasTruncated).toBe(true);
            // 应该在段落边界截断
            expect(result.content).not.toContain(paragraph2.substring(0, 100));
        });
        it('should respect custom maxChars parameter', () => {
            const content = generateString(2000);
            const result = truncationUtils.truncateMessage(content, 1000);
            expect(result.wasTruncated).toBe(true);
            expect(result.truncatedLength).toBeLessThan(2000);
        });
    });
    describe('truncateToolOutput', () => {
        it('should use tool output limit', () => {
            const output = generateString(60000);
            const result = truncationUtils.truncateToolOutput(output);
            expect(result.wasTruncated).toBe(true);
            expect(result.content).toContain(types_1.DEFAULT_TRUNCATION_CONFIG.truncationNotice);
        });
    });
    describe('truncateCodeBlock', () => {
        it('should not truncate code under line limit', () => {
            const code = generateLines(100);
            const result = truncationUtils.truncateCodeBlock(code, 150);
            expect(result.wasTruncated).toBe(false);
            expect(result.content).toBe(code);
        });
        it('should truncate code over line limit preserving head and tail', () => {
            const code = generateLines(300);
            const result = truncationUtils.truncateCodeBlock(code, 150);
            expect(result.wasTruncated).toBe(true);
            expect(result.content).toContain('Line 1:'); // 保留开头
            expect(result.content).toContain('Line 300:'); // 保留结尾
            expect(result.content).toContain('lines omitted'); // 包含省略提示
        });
    });
    describe('truncateMessageList', () => {
        it('should truncate message list within total limit', () => {
            const messages = [
                { role: 'user', content: generateString(30000) },
                { role: 'assistant', content: generateString(30000) },
                { role: 'user', content: generateString(30000) }
            ];
            const result = truncationUtils.truncateMessageList(messages, 50000);
            // 应该从最新的消息开始保留
            expect(result.length).toBeGreaterThan(0);
            const totalLength = result.reduce((sum, m) => sum + m.content.length, 0);
            expect(totalLength).toBeLessThanOrEqual(55000); // 允许一些截断提示的额外长度
        });
    });
});
describe('ContextCompressor', () => {
    let compressor;
    let testSessionPath;
    beforeAll(() => {
        if (!fs.existsSync(TEST_DIR)) {
            fs.mkdirSync(TEST_DIR, { recursive: true });
        }
    });
    beforeEach(() => {
        compressor = new context_compressor_1.ContextCompressor();
        testSessionPath = path.join(TEST_DIR, `test-${Date.now()}`);
    });
    afterAll(() => {
        // 清理测试目录
        if (fs.existsSync(TEST_DIR)) {
            fs.rmSync(TEST_DIR, { recursive: true, force: true });
        }
    });
    describe('truncateMessage', () => {
        it('should delegate to TruncationUtils', () => {
            const content = generateString(60000);
            const result = compressor.truncateMessage(content);
            expect(result.wasTruncated).toBe(true);
        });
    });
    describe('compress', () => {
        it('should apply preventive truncation first', async () => {
            const memory = new memory_document_1.MemoryDocument('test-session', 'Test Session', testSessionPath);
            await memory.load();
            // 添加长内容
            for (let i = 0; i < 20; i++) {
                memory.addImportantContext(generateString(1000, `context-${i}-`));
            }
            const result = await compressor.compress(memory);
            expect(result).toBe(true);
            const stats = compressor.getLastStats();
            expect(stats).not.toBeNull();
        });
    });
});
console.log('✅ 压缩系统测试文件已创建');
//# sourceMappingURL=context-compression.test.js.map
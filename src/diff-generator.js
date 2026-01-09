"use strict";
/**
 * Diff 生成器
 * 本地 Diff 生成，对比快照与当前文件
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
exports.DiffGenerator = void 0;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
/**
 * Diff 生成器
 */
class DiffGenerator {
    sessionManager;
    workspaceRoot;
    constructor(sessionManager, workspaceRoot) {
        this.sessionManager = sessionManager;
        this.workspaceRoot = workspaceRoot;
    }
    /** 生成文件 Diff */
    generateDiff(filePath) {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return null;
        const relativePath = path.isAbsolute(filePath)
            ? path.relative(this.workspaceRoot, filePath)
            : filePath;
        const snapshot = this.sessionManager.getSnapshot(session.id, relativePath);
        if (!snapshot)
            return null;
        const absolutePath = path.join(this.workspaceRoot, relativePath);
        const currentContent = fs.existsSync(absolutePath)
            ? fs.readFileSync(absolutePath, 'utf-8')
            : '';
        const hunks = this.computeHunks(snapshot.originalContent, currentContent, relativePath, snapshot.lastModifiedBy);
        let additions = 0;
        let deletions = 0;
        for (const hunk of hunks) {
            const lines = hunk.content.split('\n');
            for (const line of lines) {
                if (line.startsWith('+') && !line.startsWith('+++'))
                    additions++;
                if (line.startsWith('-') && !line.startsWith('---'))
                    deletions++;
            }
        }
        return {
            filePath: relativePath,
            hunks,
            additions,
            deletions,
            source: snapshot.lastModifiedBy,
        };
    }
    /** 计算 Diff Hunks */
    computeHunks(original, current, filePath, source) {
        const originalLines = original.split('\n');
        const currentLines = current.split('\n');
        const hunks = [];
        // 使用简单的 LCS 算法计算差异
        const diff = this.simpleDiff(originalLines, currentLines);
        if (diff.length === 0)
            return hunks;
        // 将连续的差异合并为 hunk
        let currentHunk = null;
        let oldLineNum = 1;
        let newLineNum = 1;
        for (const item of diff) {
            if (item.type === 'equal') {
                if (currentHunk) {
                    // 结束当前 hunk
                    hunks.push(this.createHunk(currentHunk, filePath, source));
                    currentHunk = null;
                }
                oldLineNum++;
                newLineNum++;
            }
            else {
                if (!currentHunk) {
                    currentHunk = {
                        oldStart: oldLineNum,
                        oldLines: [],
                        newStart: newLineNum,
                        newLines: [],
                    };
                }
                if (item.type === 'delete') {
                    currentHunk.oldLines.push(item.line);
                    oldLineNum++;
                }
                else if (item.type === 'insert') {
                    currentHunk.newLines.push(item.line);
                    newLineNum++;
                }
            }
        }
        // 处理最后一个 hunk
        if (currentHunk) {
            hunks.push(this.createHunk(currentHunk, filePath, source));
        }
        return hunks;
    }
    /** 创建 DiffHunk */
    createHunk(data, filePath, source) {
        const content = [
            ...data.oldLines.map(l => `-${l}`),
            ...data.newLines.map(l => `+${l}`),
        ].join('\n');
        return {
            filePath,
            oldStart: data.oldStart,
            oldLines: data.oldLines.length,
            newStart: data.newStart,
            newLines: data.newLines.length,
            content,
            source,
        };
    }
    /** 简单 Diff 算法 */
    simpleDiff(oldLines, newLines) {
        const result = [];
        // 使用简单的逐行比较（可以后续优化为 Myers 算法）
        let i = 0, j = 0;
        while (i < oldLines.length || j < newLines.length) {
            if (i >= oldLines.length) {
                result.push({ type: 'insert', line: newLines[j] });
                j++;
            }
            else if (j >= newLines.length) {
                result.push({ type: 'delete', line: oldLines[i] });
                i++;
            }
            else if (oldLines[i] === newLines[j]) {
                result.push({ type: 'equal', line: oldLines[i] });
                i++;
                j++;
            }
            else {
                // 查找最近的匹配
                const oldMatch = newLines.indexOf(oldLines[i], j);
                const newMatch = oldLines.indexOf(newLines[j], i);
                if (oldMatch === -1 && newMatch === -1) {
                    result.push({ type: 'delete', line: oldLines[i] });
                    result.push({ type: 'insert', line: newLines[j] });
                    i++;
                    j++;
                }
                else if (oldMatch !== -1 && (newMatch === -1 || oldMatch - j <= newMatch - i)) {
                    while (j < oldMatch) {
                        result.push({ type: 'insert', line: newLines[j] });
                        j++;
                    }
                }
                else {
                    while (i < newMatch) {
                        result.push({ type: 'delete', line: oldLines[i] });
                        i++;
                    }
                }
            }
        }
        return result;
    }
    /** 生成所有待处理文件的 Diff */
    generateAllDiffs() {
        const session = this.sessionManager.getCurrentSession();
        if (!session)
            return [];
        const results = [];
        for (const snapshot of session.snapshots) {
            const diff = this.generateDiff(snapshot.filePath);
            if (diff && (diff.additions > 0 || diff.deletions > 0)) {
                results.push(diff);
            }
        }
        return results;
    }
    /** 格式化 Diff 为统一格式字符串 */
    formatDiff(diff) {
        const lines = [
            `--- a/${diff.filePath}`,
            `+++ b/${diff.filePath}`,
        ];
        for (const hunk of diff.hunks) {
            lines.push(`@@ -${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines} @@`);
            lines.push(hunk.content);
        }
        return lines.join('\n');
    }
}
exports.DiffGenerator = DiffGenerator;
//# sourceMappingURL=diff-generator.js.map
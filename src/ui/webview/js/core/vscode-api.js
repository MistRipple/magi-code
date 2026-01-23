// VSCode API 通信封装
// 此文件封装所有与 VSCode Extension 的通信逻辑

import { vscode } from './state.js';

/**
 * 发送消息到 Extension
 */
export function postMessage(message) {
  vscode.postMessage(message);
}

/**
 * 执行任务
 */
export function executeTask(prompt, images = null, mode = 'auto', agent = null) {
  postMessage({
    type: 'executeTask',
    prompt,
    images,
    mode,
    agent
  });
}

/**
 * 中断任务
 */
export function interruptTask() {
  postMessage({ type: 'interrupt' });
}

/**
 * 新建会话
 */
export function createNewSession() {
  postMessage({ type: 'newSession' });
}

/**
 * 切换会话
 */
export function switchSession(sessionId) {
  postMessage({ type: 'switchSession', sessionId });
}

/**
 * 删除会话
 */
export function deleteSession(sessionId) {
  postMessage({ type: 'deleteSession', sessionId });
}

/**
 * 重命名会话
 */
export function renameSession(sessionId, newName) {
  postMessage({ type: 'renameSession', sessionId, newName });
}

/**
 * 确认计划
 */
export function confirmPlan(confirmed) {
  postMessage({ type: 'confirmPlan', confirmed });
}

/**
 * 回答问题
 */
export function answerQuestions(answer) {
  postMessage({ type: 'answerQuestions', answer });
}

/**
 * 回答澄清问题
 */
export function answerClarification(answers, additionalInfo) {
  postMessage({ type: 'answerClarification', answers, additionalInfo });
}

/**
 * 回答 Worker 问题
 */
export function answerWorkerQuestion(answer) {
  postMessage({ type: 'answerWorkerQuestion', answer });
}

/**
 * 打开文件
 */
export function openFile(filepath) {
  postMessage({ type: 'openFile', filepath });
}

/**
 * 应用变更
 */
export function applyChange(changeId) {
  postMessage({ type: 'applyChange', changeId });
}

/**
 * 拒绝变更
 */
export function rejectChange(changeId) {
  postMessage({ type: 'rejectChange', changeId });
}

/**
 * 获取 Profile 配置
 */
export function getProfileConfig() {
  postMessage({ type: 'getProfileConfig' });
}

/**
 * 保存 Profile 配置
 */
export function saveProfileConfig(config) {
  postMessage({ type: 'saveProfileConfig', config });
}

/**
 * 重置 Profile 配置
 */
export function resetProfileConfig() {
  postMessage({ type: 'resetProfileConfig' });
}

/**
 * 增强提示词
 */
export function enhancePrompt(prompt) {
  postMessage({ type: 'enhancePrompt', prompt });
}

/**
 * 刷新 Agent 连接状态
 */
export function refreshAgentConnections() {
  postMessage({ type: 'checkWorkerStatus' });
}

/**
 * 重置执行统计
 */
export function resetExecutionStats() {
  postMessage({ type: 'resetExecutionStats' });
}

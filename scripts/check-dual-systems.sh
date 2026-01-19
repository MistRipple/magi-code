#!/bin/bash

echo "=== 双重系统快速检查 ==="
echo ""

# 1. TaskManager vs UnifiedTaskManager
echo "## 1. TaskManager 双重使用检查"
echo ""
tm_count=$(grep -c "this.taskManager\." src/orchestrator/orchestrator-agent.ts 2>/dev/null || true)
utm_count=$(grep -c "this.unifiedTaskManager\." src/orchestrator/orchestrator-agent.ts 2>/dev/null || true)

echo "  TaskManager 使用: $tm_count 处"
echo "  UnifiedTaskManager 使用: $utm_count 处"

if [ "$tm_count" -gt 0 ] && [ "$utm_count" -gt 0 ]; then
  echo "  ⚠️  警告: 两个管理器同时在使用！"
else
  echo "  ✅ 正常"
fi

# 2. 状态同步代码检查
echo ""
echo "## 2. 状态同步代码检查"
echo ""
sync_count=$(grep -rn "同步到.*Manager\|sync.*to.*Manager" src/orchestrator/ --include="*.ts" 2>/dev/null | wc -l | tr -d ' ')
echo "  状态同步注释: $sync_count 处"

if [ "$sync_count" -gt 0 ]; then
  echo "  ⚠️  发现状态同步代码"
  grep -rn "同步到.*Manager\|sync.*to.*Manager" src/orchestrator/ --include="*.ts" 2>/dev/null | head -3
else
  echo "  ✅ 无状态同步代码"
fi

# 3. 事件监听中的双重调用
echo ""
echo "## 3. 事件监听中的双重调用检查"
echo ""
event_sync=$(grep -A5 "unifiedTaskManager.on" src/orchestrator/orchestrator-agent.ts 2>/dev/null | grep -c "taskManager\." || true)
echo "  事件监听中调用 TaskManager: $event_sync 处"

if [ "$event_sync" -gt 0 ]; then
  echo "  ⚠️  警告: 事件监听中存在状态同步"
else
  echo "  ✅ 无事件同步"
fi

# 4. SessionManager 检查
echo ""
echo "## 4. SessionManager 双重存在检查"
echo ""
old_sm=$(find src -name "session-manager.ts" ! -name "unified-session-manager.ts" 2>/dev/null | wc -l | tr -d ' ')
new_sm=$(find src -name "unified-session-manager.ts" 2>/dev/null | wc -l | tr -d ' ')

echo "  旧 SessionManager: $old_sm 个"
echo "  新 UnifiedSessionManager: $new_sm 个"

if [ "$old_sm" -gt 0 ] && [ "$new_sm" -gt 0 ]; then
  echo "  ⚠️  警告: 两个 SessionManager 同时存在"
else
  echo "  ✅ 正常"
fi

# 5. 总结
echo ""
echo "=== 检查总结 ==="
echo ""

issues=0

if [ "$tm_count" -gt 0 ] && [ "$utm_count" -gt 0 ]; then
  echo "  ⚠️  发现 TaskManager 双重使用"
  issues=$((issues + 1))
fi

if [ "$sync_count" -gt 0 ]; then
  echo "  ⚠️  发现状态同步代码"
  issues=$((issues + 1))
fi

if [ "$event_sync" -gt 0 ]; then
  echo "  ⚠️  发现事件监听中的状态同步"
  issues=$((issues + 1))
fi

if [ "$old_sm" -gt 0 ] && [ "$new_sm" -gt 0 ]; then
  echo "  ⚠️  发现 SessionManager 双重存在"
  issues=$((issues + 1))
fi

echo ""
if [ "$issues" -eq 0 ]; then
  echo "✅ 无双重系统问题"
  exit 0
else
  echo "⚠️  发现 $issues 个双重系统问题"
  echo ""
  echo "建议: 查看 docs/双重系统使用分析报告.md 了解详情"
  exit 1
fi

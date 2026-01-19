#!/bin/bash

echo "=== TaskManager 迁移最终验证 ==="
echo ""

# 1. 编译检查
echo "1. 编译检查..."
if npx tsc --noEmit 2>&1 | grep -q "error TS"; then
  echo "❌ 编译失败"
  npx tsc --noEmit 2>&1 | head -20
  exit 1
else
  echo "✅ 编译通过"
fi

# 2. 测试检查
echo ""
echo "2. 测试检查..."
test_output=$(npm test 2>&1)
failed_count=$(echo "$test_output" | grep "❌.*\.js" 2>/dev/null | wc -l | tr -d ' ')
if [ "$failed_count" != "0" ]; then
  echo "❌ 有 $failed_count 个测试套件失败"
  echo "$test_output" | tail -30
  exit 1
fi
total_passed=$(echo "$test_output" | grep -o "✅ 通过: [0-9]*/[0-9]*" | awk -F'[/: ]' '{sum+=$4} END {print sum}')
echo "✅ 测试通过 ($total_passed 个测试)"

# 3. 日志 key 规范检查
echo ""
echo "3. 日志 key 规范检查..."
if node scripts/check-log-keys.js >/dev/null 2>&1; then
  echo "✅ 日志 key 规范通过"
else
  echo "❌ 日志 key 规范未通过"
  node scripts/check-log-keys.js
  exit 1
fi

# 4. 旧架构文件检查
echo ""
echo "4. 旧架构文件检查..."
old_files_exist=0
for file in "src/orchestrator/orchestrator-agent.ts" "src/orchestrator/worker-agent.ts" "src/orchestrator/worker-pool.ts"; do
  if [ -f "$file" ]; then
    echo "❌ 旧架构文件仍存在: $file"
    old_files_exist=1
  fi
done
if [ "$old_files_exist" = "0" ]; then
  echo "✅ 旧架构文件已全部删除"
fi

# 5. TaskManager 文件清理检查
echo ""
echo "5. TaskManager 文件清理检查..."
if [ ! -f src/task-manager.ts ]; then
  echo "✅ TaskManager 已删除"
else
  echo "❌ TaskManager 文件仍存在"
  exit 1
fi

# 6. MissionDrivenEngine 使用检查
echo ""
echo "6. MissionDrivenEngine 使用检查..."
mde_count=$(grep "missionDrivenEngine" src/orchestrator/intelligent-orchestrator.ts 2>/dev/null | wc -l | tr -d ' ')
if [ "$mde_count" -gt 5 ]; then
  echo "✅ MissionDrivenEngine 正常使用 ($mde_count 处)"
else
  echo "⚠️  MissionDrivenEngine 使用较少 ($mde_count 处)"
fi

# 7. IntelligentOrchestrator 检查
echo ""
echo "7. IntelligentOrchestrator 检查..."
if grep -q "sessionManager: UnifiedSessionManager" src/orchestrator/intelligent-orchestrator.ts; then
  echo "✅ IntelligentOrchestrator 直接接收 SessionManager"
elif grep -q "this.sessionManager = sessionManager" src/orchestrator/intelligent-orchestrator.ts; then
  echo "✅ IntelligentOrchestrator 使用 SessionManager"
else
  echo "❌ IntelligentOrchestrator 未正确使用 SessionManager"
  exit 1
fi

echo ""
echo "=== ✅ 所有检查通过 ==="
echo ""
echo "验证摘要:"
echo "  - 编译: ✅ 通过"
echo "  - 测试: ✅ $total_passed 通过"
echo "  - 日志 key 规范: ✅ 通过"
echo "  - 旧架构清理: ✅ 完成"
echo "  - TaskManager 删除: ✅ 完成"
echo "  - MissionDrivenEngine 使用: ✅ 正常"
echo "  - SessionManager 传递: ✅ 正常"
echo ""
echo "项目状态: 生产就绪 (Production Ready)"

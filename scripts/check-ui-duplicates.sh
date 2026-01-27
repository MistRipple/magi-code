#!/bin/bash
# UI 组件重复检测脚本
# 用于在提交前检查是否有重复的 UI 组件定义

echo "🔍 检查 UI 组件重复..."
echo ""

# 检查是否有多个相似的基类（排除修饰符）
echo "1. 检查基类重复..."
duplicates=$(grep -E "^\s*\.(btn|badge|card|toggle|switch)[^-]*\s*\{" \
  src/ui/webview/styles/components.css | \
  sed 's/\s*{.*//' | \
  sed 's/^\s*//' | \
  sort | uniq -c | \
  awk '$1 > 1 {print}')

if [ -n "$duplicates" ]; then
  echo "⚠️  发现可能的重复基类定义："
  echo "$duplicates"
  echo ""
  echo "请检查是否可以通过修饰符合并这些类。"
  exit 1
fi

echo "✅ 未发现重复基类定义"
echo ""

# 检查是否使用了设计系统变量
echo "2. 检查设计系统变量使用..."
hardcoded_values=$(grep -E "padding:\s*[0-9]+px|margin:\s*[0-9]+px|border-radius:\s*[0-9]+px" \
  src/ui/webview/styles/components.css | \
  grep -v "var(--" | \
  wc -l | \
  tr -d ' ')

if [ "$hardcoded_values" -gt 0 ]; then
  echo "⚠️  发现 $hardcoded_values 处硬编码的值，建议使用设计系统变量"
  echo ""
  echo "示例："
  echo "  ❌ padding: 8px;  →  ✅ padding: var(--spacing-2);"
  echo "  ❌ border-radius: 4px;  →  ✅ border-radius: var(--radius-1);"
  echo ""
fi

# 检查是否使用了 emoji
echo "3. 检查 emoji 使用..."
emoji_count=$(grep -E "content:\s*['\"][^'\"]*[😀-🙏🌀-🗿🚀-🛿]" \
  src/ui/webview/styles/components.css | \
  wc -l | \
  tr -d ' ')

if [ "$emoji_count" -gt 0 ]; then
  echo "❌ 发现 $emoji_count 处使用了 emoji，这是不允许的！"
  echo ""
  exit 1
fi

echo "✅ 未发现 emoji 使用"
echo ""

# 统计组件数量
echo "4. 组件统计..."
btn_count=$(grep -E "^\s*\.btn" src/ui/webview/styles/components.css | wc -l | tr -d ' ')
badge_count=$(grep -E "^\s*\.badge" src/ui/webview/styles/components.css | wc -l | tr -d ' ')
card_count=$(grep -E "^\s*\.card" src/ui/webview/styles/components.css | wc -l | tr -d ' ')
toggle_count=$(grep -E "^\s*\.toggle" src/ui/webview/styles/components.css | wc -l | tr -d ' ')

echo "  - 按钮相关类: $btn_count"
echo "  - 徽章相关类: $badge_count"
echo "  - 卡片相关类: $card_count"
echo "  - Toggle相关类: $toggle_count"
echo ""

# 检查基类是否存在
echo "5. 验证统一基类系统..."
base_classes=(".btn-icon" ".badge" ".card" ".toggle-switch")
all_exist=true

for class in "${base_classes[@]}"; do
  if grep -q "^\s*\\$class\s*{" src/ui/webview/styles/components.css; then
    echo "  ✅ $class 基类存在"
  else
    echo "  ❌ $class 基类不存在"
    all_exist=false
  fi
done

echo ""

if [ "$all_exist" = false ]; then
  echo "❌ 部分基类缺失，请检查！"
  exit 1
fi

echo "✅ 所有基类验证通过"
echo ""
echo "🎉 UI 组件检查完成！代码质量良好。"
exit 0


#!/bin/bash

# 统一日志系统迁移脚本
# 将所有 console.log/warn/error 替换为 logger 调用

echo "=== 统一日志系统迁移 ==="
echo ""

# 统计当前 console 使用情况
echo "1. 统计当前 console 使用情况..."
total_console=$(grep -r "console\." src --include="*.ts" | grep -v "test" | grep -v ".bak" | wc -l | tr -d ' ')
console_log=$(grep -r "console\.log" src --include="*.ts" | grep -v "test" | grep -v ".bak" | wc -l | tr -d ' ')
console_warn=$(grep -r "console\.warn" src --include="*.ts" | grep -v "test" | grep -v ".bak" | wc -l | tr -d ' ')
console_error=$(grep -r "console\.error" src --include="*.ts" | grep -v "test" | grep -v ".bak" | wc -l | tr -d ' ')
console_debug=$(grep -r "console\.debug" src --include="*.ts" | grep -v "test" | grep -v ".bak" | wc -l | tr -d ' ')

echo "  总计: $total_console"
echo "  - console.log: $console_log"
echo "  - console.warn: $console_warn"
echo "  - console.error: $console_error"
echo "  - console.debug: $console_debug"
echo ""

# 确认
read -p "是否继续迁移？(y/n) " -n 1 -r
echo ""
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
  echo "取消迁移"
  exit 0
fi

echo ""
echo "2. 开始迁移..."

# 查找所有需要迁移的文件（排除测试文件和日志系统本身）
files=$(find src -name "*.ts" -type f \
  | grep -v "test" \
  | grep -v ".bak" \
  | grep -v "logging/" \
  | xargs grep -l "console\." 2>/dev/null)

if [ -z "$files" ]; then
  echo "  没有需要迁移的文件"
  exit 0
fi

echo "  找到 $(echo "$files" | wc -l | tr -d ' ') 个文件需要迁移"
echo ""

# 迁移每个文件
for file in $files; do
  echo "  处理: $file"

  # 备份原文件
  cp "$file" "$file.bak"

  # 检查是否已经导入 logger
  if ! grep -q "from.*logging" "$file"; then
    # 在文件开头添加 import
    # 找到第一个 import 语句的位置
    first_import=$(grep -n "^import" "$file" | head -1 | cut -d: -f1)

    if [ -n "$first_import" ]; then
      # 在第一个 import 之后插入
      sed -i '' "${first_import}a\\
import { logger, LogCategory } from './logging';
" "$file"
    else
      # 如果没有 import，在文件开头插入
      sed -i '' "1i\\
import { logger, LogCategory } from './logging';\\

" "$file"
    fi
  fi

  # 替换 console 调用
  # 注意：这是简单替换，可能需要手动调整

  # console.debug -> logger.debug
  sed -i '' 's/console\.debug(/logger.debug(/g' "$file"

  # console.log -> logger.info
  sed -i '' 's/console\.log(/logger.info(/g' "$file"

  # console.warn -> logger.warn
  sed -i '' 's/console\.warn(/logger.warn(/g' "$file"

  # console.error -> logger.error
  sed -i '' 's/console\.error(/logger.error(/g' "$file"

  echo "    ✓ 完成"
done

echo ""
echo "3. 验证迁移结果..."

# 编译检查
if npx tsc --noEmit 2>&1 | grep -q "error TS"; then
  echo "  ❌ 编译失败，请检查错误"
  echo ""
  echo "  恢复备份文件..."
  for file in $files; do
    if [ -f "$file.bak" ]; then
      mv "$file.bak" "$file"
    fi
  done
  echo "  已恢复所有文件"
  exit 1
else
  echo "  ✅ 编译通过"
fi

echo ""
echo "4. 统计迁移后的情况..."
remaining_console=$(grep -r "console\." src --include="*.ts" | grep -v "test" | grep -v ".bak" | grep -v "logging/" | wc -l | tr -d ' ')
echo "  剩余 console 调用: $remaining_console"

echo ""
echo "5. 清理备份文件..."
for file in $files; do
  if [ -f "$file.bak" ]; then
    rm "$file.bak"
  fi
done
echo "  ✓ 完成"

echo ""
echo "=== ✅ 迁移完成 ==="
echo ""
echo "迁移摘要:"
echo "  - 处理文件: $(echo "$files" | wc -l | tr -d ' ')"
echo "  - 迁移前: $total_console 个 console 调用"
echo "  - 迁移后: $remaining_console 个 console 调用"
echo "  - 迁移数量: $((total_console - remaining_console))"
echo ""
echo "注意事项:"
echo "  1. 请手动检查迁移后的代码"
echo "  2. 某些 console 调用可能需要手动调整分类"
echo "  3. 运行测试确保功能正常"
echo ""

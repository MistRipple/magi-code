/**
 * Markdown 表格解析测试
 */

// 表格解析函数
function parseMarkdownTables(content) {
  const lines = content.split('\n');
  const result = [];
  let i = 0;
  
  while (i < lines.length) {
    const line = lines[i];
    
    if (line.includes('|') && i + 1 < lines.length) {
      const nextLine = lines[i + 1];
      if (/^\|?[\s]*:?-+:?[\s]*\|/.test(nextLine) || /\|[\s]*:?-+:?[\s]*\|?$/.test(nextLine)) {
        const tableLines = [line, nextLine];
        let j = i + 2;
        
        while (j < lines.length && lines[j].includes('|')) {
          tableLines.push(lines[j]);
          j++;
        }
        
        const html = convertTableToHtml(tableLines);
        result.push(html);
        i = j;
        continue;
      }
    }
    
    result.push(line);
    i++;
  }
  
  return result.join('\n');
}

function convertTableToHtml(tableLines) {
  if (tableLines.length < 2) return tableLines.join('\n');
  
  const alignLine = tableLines[1];
  const alignCells = alignLine.split('|').filter(c => c.trim());
  const alignments = alignCells.map(cell => {
    const trimmed = cell.trim();
    if (trimmed.startsWith(':') && trimmed.endsWith(':')) return 'center';
    if (trimmed.endsWith(':')) return 'right';
    return 'left';
  });
  
  const parseRow = (line) => {
    return line.split('|').filter((c, idx, arr) => idx > 0 && idx < arr.length - 1).map(c => c.trim());
  };
  
  let html = '<table class="md-table">';
  
  const headerCells = parseRow(tableLines[0]);
  html += '<thead><tr>';
  headerCells.forEach((cell, idx) => {
    const align = alignments[idx] || 'left';
    html += '<th style="text-align:' + align + '">' + cell + '</th>';
  });
  html += '</tr></thead>';
  
  if (tableLines.length > 2) {
    html += '<tbody>';
    for (let i = 2; i < tableLines.length; i++) {
      const cells = parseRow(tableLines[i]);
      html += '<tr>';
      cells.forEach((cell, idx) => {
        const align = alignments[idx] || 'left';
        html += '<td style="text-align:' + align + '">' + cell + '</td>';
      });
      html += '</tr>';
    }
    html += '</tbody>';
  }
  
  html += '</table>';
  return html;
}

// 测试用例
console.log('=== Markdown 表格解析测试 ===\n');

// 测试 1: 基本表格
const table1 = '| 名称 | 值 |\n|---|---|\n| A | 1 |\n| B | 2 |';
const result1 = parseMarkdownTables(table1);
console.log('测试 1: 基本表格');
console.log('  包含 <table>:', result1.includes('<table'));
console.log('  包含 <th>:', result1.includes('<th'));
console.log('  包含 <td>:', result1.includes('<td'));
console.log('  结果:', result1.includes('<table') ? '✅ 通过' : '❌ 失败');

// 测试 2: 带对齐的表格
const table2 = '| 左 | 中 | 右 |\n|:---|:---:|---:|\n| L | C | R |';
const result2 = parseMarkdownTables(table2);
console.log('\n测试 2: 带对齐的表格');
console.log('  左对齐:', result2.includes('text-align:left'));
console.log('  居中:', result2.includes('text-align:center'));
console.log('  右对齐:', result2.includes('text-align:right'));
console.log('  结果:', result2.includes('center') && result2.includes('right') ? '✅ 通过' : '❌ 失败');

// 测试 3: 非表格内容
const text3 = '这是普通文本\n没有表格';
const result3 = parseMarkdownTables(text3);
console.log('\n测试 3: 非表格内容');
console.log('  不包含 <table>:', !result3.includes('<table'));
console.log('  结果:', !result3.includes('<table') ? '✅ 通过' : '❌ 失败');

// 测试 4: 混合内容
const mixed = '# 标题\n\n| Col1 | Col2 |\n|---|---|\n| A | B |\n\n普通段落';
const result4 = parseMarkdownTables(mixed);
console.log('\n测试 4: 混合内容');
console.log('  包含标题:', result4.includes('# 标题'));
console.log('  包含表格:', result4.includes('<table'));
console.log('  包含段落:', result4.includes('普通段落'));
console.log('  结果:', result4.includes('<table') && result4.includes('# 标题') ? '✅ 通过' : '❌ 失败');

console.log('\n=== 测试完成 ===');


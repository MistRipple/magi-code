/**
 * MCP 连接测试脚本
 *
 * 用法: npx ts-node scripts/test-mcp-connection.ts
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

// MCP SDK
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

interface MCPServerConfig {
  id: string;
  name: string;
  command: string;
  args?: string[];
  env?: Record<string, string>;
  enabled: boolean;
  type: string;
}

async function loadMCPConfig(): Promise<MCPServerConfig[]> {
  const configPath = path.join(os.homedir(), '.multicli', 'mcp.json');

  if (!fs.existsSync(configPath)) {
    console.log('❌ MCP 配置文件不存在:', configPath);
    return [];
  }

  try {
    const content = fs.readFileSync(configPath, 'utf-8');
    const config = JSON.parse(content);
    return config.servers || [];
  } catch (error) {
    console.error('❌ 读取 MCP 配置失败:', error);
    return [];
  }
}

async function testMCPServer(server: MCPServerConfig): Promise<boolean> {
  console.log(`\n🔄 测试服务器: ${server.name} (${server.id})`);
  console.log(`   命令: ${server.command} ${(server.args || []).join(' ')}`);

  if (!server.enabled) {
    console.log('   ⏭️  已禁用，跳过');
    return false;
  }

  if (server.type !== 'stdio') {
    console.log(`   ❌ 不支持的类型: ${server.type}`);
    return false;
  }

  try {
    // 创建传输层
    const transport = new StdioClientTransport({
      command: server.command,
      args: server.args || [],
      env: { ...process.env, ...server.env } as Record<string, string>,
    });

    // 创建客户端
    const client = new Client({
      name: 'mcp-test',
      version: '1.0.0',
    }, {
      capabilities: {},
    });

    // 设置超时
    const timeout = 15000;
    const connectPromise = client.connect(transport);
    const timeoutPromise = new Promise<never>((_, reject) =>
      setTimeout(() => reject(new Error('连接超时')), timeout)
    );

    console.log('   ⏳ 连接中...');
    await Promise.race([connectPromise, timeoutPromise]);
    console.log('   ✅ 连接成功');

    // 获取工具列表
    console.log('   ⏳ 获取工具列表...');
    const toolsResponse = await client.listTools();
    const tools = toolsResponse.tools || [];
    console.log(`   ✅ 发现 ${tools.length} 个工具`);

    if (tools.length > 0) {
      console.log('   📦 工具列表:');
      tools.slice(0, 10).forEach((tool: any) => {
        console.log(`      - ${tool.name}: ${(tool.description || '').slice(0, 50)}...`);
      });
      if (tools.length > 10) {
        console.log(`      ... 还有 ${tools.length - 10} 个工具`);
      }
    }

    // 关闭连接
    await client.close();
    console.log('   ✅ 连接已关闭');

    return true;
  } catch (error: any) {
    console.log(`   ❌ 连接失败: ${error.message}`);
    return false;
  }
}

async function main() {
  console.log('='.repeat(60));
  console.log('MCP 连接测试');
  console.log('='.repeat(60));

  const servers = await loadMCPConfig();

  if (servers.length === 0) {
    console.log('\n没有配置 MCP 服务器');
    process.exit(1);
  }

  console.log(`\n找到 ${servers.length} 个 MCP 服务器配置`);

  let successCount = 0;
  let failCount = 0;

  for (const server of servers) {
    const success = await testMCPServer(server);
    if (success) {
      successCount++;
    } else if (server.enabled) {
      failCount++;
    }
  }

  console.log('\n' + '='.repeat(60));
  console.log('测试结果汇总');
  console.log('='.repeat(60));
  console.log(`✅ 成功: ${successCount}`);
  console.log(`❌ 失败: ${failCount}`);
  console.log(`⏭️  跳过: ${servers.length - successCount - failCount}`);

  process.exit(failCount > 0 ? 1 : 0);
}

main().catch(error => {
  console.error('测试脚本执行失败:', error);
  process.exit(1);
});

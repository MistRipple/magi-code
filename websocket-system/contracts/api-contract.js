/**
 * API 接口契约定义
 * 定义模块间通信的标准接口规范
 */

/**
 * API 接口契约规范
 */
const API_CONTRACT = {
  version: '1.0.0',
  
  // 服务生命周期接口
  lifecycle: {
    initialize: {
      method: 'initialize',
      params: {
        config: {
          type: 'Object',
          required: false,
          fields: {
            port: { type: 'number', default: 8080 },
            heartbeatInterval: { type: 'number', default: 30000 },
            heartbeatTimeout: { type: 'number', default: 35000 }
          }
        }
      },
      returns: {
        type: 'Promise<void>',
        description: '初始化完成后 resolve'
      },
      errors: [
        { code: 'ALREADY_INITIALIZED', message: '服务已初始化' },
        { code: 'INIT_FAILED', message: '初始化失败' }
      ]
    },
    
    shutdown: {
      method: 'shutdown',
      params: {},
      returns: {
        type: 'Promise<void>',
        description: '关闭完成后 resolve'
      },
      errors: [
        { code: 'SHUTDOWN_FAILED', message: '关闭失败' }
      ]
    }
  },

  // 消息传输接口
  messaging: {
    sendToClient: {
      method: 'sendToClient',
      params: {
        clientId: {
          type: 'string',
          required: true,
          description: '客户端唯一标识'
        },
        message: {
          type: 'Object',
          required: true,
          description: '消息对象',
          fields: {
            type: { type: 'string', required: true },
            content: { type: 'any', required: false },
            timestamp: { type: 'number', required: false }
          }
        }
      },
      returns: {
        type: 'Promise<boolean>',
        description: 'true 表示发送成功，false 表示失败'
      },
      errors: [
        { code: 'CLIENT_NOT_FOUND', message: '客户端不存在' },
        { code: 'SEND_FAILED', message: '发送失败' }
      ]
    },

    broadcast: {
      method: 'broadcast',
      params: {
        message: {
          type: 'Object',
          required: true,
          description: '广播消息对象'
        },
        options: {
          type: 'Object',
          required: false,
          fields: {
            exclude: {
              type: 'Array<string>',
              description: '排除的客户端ID列表'
            },
            filter: {
              type: 'Function',
              description: '客户端过滤函数'
            }
          }
        }
      },
      returns: {
        type: 'Promise<Object>',
        description: '广播结果统计',
        fields: {
          success: { type: 'boolean' },
          sentCount: { type: 'number' },
          failedCount: { type: 'number' },
          totalCount: { type: 'number' }
        }
      },
      errors: [
        { code: 'BROADCAST_FAILED', message: '广播失败' }
      ]
    }
  },

  // 状态查询接口
  status: {
    getStatus: {
      method: 'getStatus',
      params: {},
      returns: {
        type: 'Promise<Object>',
        description: '服务状态信息',
        fields: {
          initialized: { type: 'boolean' },
          running: { type: 'boolean' },
          port: { type: 'number' },
          connections: {
            type: 'Object',
            fields: {
              total: { type: 'number' },
              alive: { type: 'number' },
              inactive: { type: 'number' }
            }
          },
          uptime: { type: 'number' }
        }
      }
    },

    getConnectedClients: {
      method: 'getConnectedClients',
      params: {},
      returns: {
        type: 'Array<Object>',
        description: '已连接客户端列表',
        itemFields: {
          id: { type: 'string' },
          connectedAt: { type: 'Date' },
          lastHeartbeat: { type: 'Date' },
          isAlive: { type: 'boolean' },
          metadata: { type: 'Object' }
        }
      }
    }
  },

  // 事件监听接口
  events: {
    onConnection: {
      method: 'onConnection',
      params: {
        callback: {
          type: 'Function',
          signature: '(clientId: string, metadata: Object) => void',
          description: '客户端连接时的回调函数'
        }
      },
      returns: {
        type: 'this',
        description: '支持链式调用'
      }
    },

    onMessage: {
      method: 'onMessage',
      params: {
        callback: {
          type: 'Function',
          signature: '(clientId: string, message: Object) => void',
          description: '收到消息时的回调函数'
        }
      },
      returns: {
        type: 'this',
        description: '支持链式调用'
      }
    },

    onDisconnect: {
      method: 'onDisconnect',
      params: {
        callback: {
          type: 'Function',
          signature: '(clientId: string, reason: Object) => void',
          description: '客户端断开时的回调函数'
        }
      },
      returns: {
        type: 'this',
        description: '支持链式调用'
      }
    },

    onError: {
      method: 'onError',
      params: {
        callback: {
          type: 'Function',
          signature: '(clientId: string, error: Error) => void',
          description: '发生错误时的回调函数'
        }
      },
      returns: {
        type: 'this',
        description: '支持链式调用'
      }
    }
  },

  // 消息协议规范
  protocol: {
    messageFormat: {
      required: ['type'],
      optional: ['content', 'timestamp', 'from', 'to'],
      types: {
        system: ['connected', 'disconnected', 'error'],
        heartbeat: ['ping', 'pong'],
        user: ['chat', 'private', 'broadcast', 'notification']
      }
    },

    systemMessages: {
      connected: {
        type: 'connected',
        connectionId: 'string',
        message: 'string',
        timestamp: 'number'
      },
      error: {
        type: 'error',
        code: 'string',
        message: 'string',
        timestamp: 'number'
      }
    }
  }
};

/**
 * 验证方法调用是否符合契约
 * @param {string} methodName - 方法名
 * @param {Array} args - 参数数组
 * @returns {Object} 验证结果
 */
function validateMethodCall(methodName, args) {
  // 查找方法定义
  let methodDef = null;
  let category = null;

  for (const [cat, methods] of Object.entries(API_CONTRACT)) {
    if (cat === 'version' || cat === 'protocol') continue;
    
    if (methods[methodName]) {
      methodDef = methods[methodName];
      category = cat;
      break;
    }
  }

  if (!methodDef) {
    return {
      valid: false,
      error: `方法 ${methodName} 不在契约中定义`
    };
  }

  // 验证参数（简化版）
  const violations = [];
  const paramDefs = methodDef.params || {};
  const paramNames = Object.keys(paramDefs);

  paramNames.forEach((paramName, index) => {
    const paramDef = paramDefs[paramName];
    const argValue = args[index];

    if (paramDef.required && argValue === undefined) {
      violations.push(`缺少必需参数: ${paramName}`);
    }

    if (argValue !== undefined && paramDef.type) {
      const actualType = Array.isArray(argValue) ? 'Array' : typeof argValue;
      const expectedType = paramDef.type.replace(/<.*>/, ''); // 移除泛型部分
      
      if (actualType !== expectedType && expectedType !== 'any') {
        violations.push(`参数 ${paramName} 类型错误: 期望 ${expectedType}, 实际 ${actualType}`);
      }
    }
  });

  return {
    valid: violations.length === 0,
    category,
    violations,
    methodDef
  };
}

/**
 * 验证消息格式是否符合协议
 * @param {Object} message - 消息对象
 * @returns {Object} 验证结果
 */
function validateMessageFormat(message) {
  const violations = [];

  // 检查必需字段
  if (!message.type) {
    violations.push('缺少必需字段: type');
  }

  // 检查消息类型是否有效
  if (message.type) {
    const allTypes = [
      ...API_CONTRACT.protocol.messageFormat.types.system,
      ...API_CONTRACT.protocol.messageFormat.types.heartbeat,
      ...API_CONTRACT.protocol.messageFormat.types.user
    ];

    if (!allTypes.includes(message.type)) {
      violations.push(`未知的消息类型: ${message.type}`);
    }
  }

  return {
    valid: violations.length === 0,
    violations
  };
}

/**
 * 生成接口文档
 * @returns {string} Markdown 格式的文档
 */
function generateApiDocumentation() {
  let doc = '# WebSocket 服务 API 契约文档\n\n';
  doc += `版本: ${API_CONTRACT.version}\n\n`;

  for (const [category, methods] of Object.entries(API_CONTRACT)) {
    if (category === 'version' || category === 'protocol') continue;

    doc += `## ${category}\n\n`;

    for (const [methodName, methodDef] of Object.entries(methods)) {
      doc += `### ${methodName}\n\n`;
      doc += `**方法名**: \`${methodDef.method}\`\n\n`;
      
      if (methodDef.params && Object.keys(methodDef.params).length > 0) {
        doc += '**参数**:\n';
        for (const [paramName, paramDef] of Object.entries(methodDef.params)) {
          doc += `- \`${paramName}\` (${paramDef.type})`;
          if (paramDef.required) doc += ' **必需**';
          if (paramDef.description) doc += `: ${paramDef.description}`;
          doc += '\n';
        }
        doc += '\n';
      }

      if (methodDef.returns) {
        doc += `**返回值**: \`${methodDef.returns.type}\`\n`;
        if (methodDef.returns.description) {
          doc += `- ${methodDef.returns.description}\n`;
        }
        doc += '\n';
      }

      if (methodDef.errors && methodDef.errors.length > 0) {
        doc += '**可能的错误**:\n';
        methodDef.errors.forEach(error => {
          doc += `- \`${error.code}\`: ${error.message}\n`;
        });
        doc += '\n';
      }
    }
  }

  return doc;
}

// 导出契约和工具函数
export {
  API_CONTRACT,
  validateMethodCall,
  validateMessageFormat,
  generateApiDocumentation
};

export default API_CONTRACT;

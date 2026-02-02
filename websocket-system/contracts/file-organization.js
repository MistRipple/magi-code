/**
 * 文件组织契约实现
 * 定义标准的目录结构和文件命名规范
 */

/**
 * 项目文件组织规范
 */
const FILE_ORGANIZATION_CONTRACT = {
  // 目录结构
  structure: {
    server: {
      path: 'server/',
      description: '服务端核心代码',
      files: {
        'index.js': '服务启动入口',
        'websocket-server.js': 'WebSocket 核心服务',
        'connection-manager.js': '连接池管理器'
      }
    },
    client: {
      path: 'client/',
      description: '客户端代码',
      files: {
        'websocket-client.js': '客户端封装类',
        'index.html': '浏览器演示页面'
      }
    },
    contracts: {
      path: 'contracts/',
      description: '契约适配层',
      files: {
        'service-adapter.js': '服务契约适配器',
        'file-organization.js': '文件组织契约',
        'api-contract.js': 'API 接口契约'
      }
    },
    demo: {
      path: 'demo/',
      description: '演示和示例',
      files: {
        'app.js': '完整演示程序',
        'contract-integration.js': '契约集成示例'
      }
    },
    types: {
      path: 'types/',
      description: '类型定义（如果使用 TypeScript）',
      files: {
        'index.d.ts': '类型声明文件'
      }
    }
  },

  // 命名规范
  namingConventions: {
    files: {
      kebabCase: true, // 使用烤串命名法
      examples: ['websocket-server.js', 'connection-manager.js']
    },
    classes: {
      pascalCase: true, // 类名使用大驼峰
      examples: ['WebSocketServerCore', 'ConnectionManager']
    },
    methods: {
      camelCase: true, // 方法名使用小驼峰
      examples: ['sendToClient', 'broadcast', 'getStatus']
    },
    constants: {
      upperSnakeCase: true, // 常量使用大写下划线
      examples: ['MAX_CONNECTIONS', 'DEFAULT_PORT']
    }
  },

  // 导出规范
  exports: {
    esm: true, // 使用 ES Module
    default: 'class', // 默认导出类
    named: 'utilities' // 命名导出工具函数
  },

  // 注释规范
  comments: {
    language: 'zh-CN', // 中文注释
    jsdoc: true, // 使用 JSDoc 格式
    required: ['public methods', 'class definitions', 'complex logic']
  }
};

/**
 * 验证文件组织是否符合契约
 * @param {Object} projectStructure - 项目结构对象
 * @returns {Object} 验证结果
 */
function validateFileOrganization(projectStructure) {
  const violations = [];
  const warnings = [];

  // 检查必需的目录
  const requiredDirs = ['server', 'client', 'contracts'];
  requiredDirs.forEach(dir => {
    if (!projectStructure[dir]) {
      violations.push(`缺少必需目录: ${dir}/`);
    }
  });

  // 检查核心文件
  const coreFiles = [
    'server/websocket-server.js',
    'server/connection-manager.js',
    'client/websocket-client.js',
    'contracts/service-adapter.js'
  ];
  
  coreFiles.forEach(file => {
    if (!projectStructure.files || !projectStructure.files.includes(file)) {
      violations.push(`缺少核心文件: ${file}`);
    }
  });

  return {
    valid: violations.length === 0,
    violations,
    warnings,
    timestamp: new Date().toISOString()
  };
}

/**
 * 生成标准项目结构
 * @returns {Object} 标准项目结构描述
 */
function generateStandardStructure() {
  return {
    root: 'websocket-system/',
    directories: Object.keys(FILE_ORGANIZATION_CONTRACT.structure).map(key => ({
      name: FILE_ORGANIZATION_CONTRACT.structure[key].path,
      description: FILE_ORGANIZATION_CONTRACT.structure[key].description,
      files: FILE_ORGANIZATION_CONTRACT.structure[key].files
    })),
    packageJson: {
      required: true,
      fields: {
        name: 'websocket-push-system',
        version: '1.0.0',
        type: 'module',
        main: 'server/index.js',
        scripts: {
          start: 'node server/index.js',
          demo: 'node demo/app.js'
        }
      }
    }
  };
}

/**
 * 获取文件路径映射
 * @returns {Object} 文件路径到用途的映射
 */
function getFilePathMapping() {
  const mapping = {};
  
  Object.keys(FILE_ORGANIZATION_CONTRACT.structure).forEach(key => {
    const dir = FILE_ORGANIZATION_CONTRACT.structure[key];
    Object.entries(dir.files).forEach(([filename, description]) => {
      mapping[`${dir.path}${filename}`] = description;
    });
  });

  return mapping;
}

// 导出契约和工具函数
export {
  FILE_ORGANIZATION_CONTRACT,
  validateFileOrganization,
  generateStandardStructure,
  getFilePathMapping
};

export default FILE_ORGANIZATION_CONTRACT;

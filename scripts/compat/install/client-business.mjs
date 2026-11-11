// 只改宿主业务调用的等待边界。保留原包页面、组件、样式及其余源字节，
// 不用 Promise 的 truthiness 冒充 DSH 同步 addAttachments 的真实回执。
import ts from 'typescript';

const isFunction = (node) => ts.isFunctionDeclaration(node) || ts.isFunctionExpression(node) || ts.isArrowFunction(node) || ts.isMethodDeclaration(node);
const methodName = (node) => ts.isParenthesizedExpression(node) ? methodName(node.expression)
  : ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.CommaToken ? methodName(node.right)
  : ts.isIdentifier(node) ? node.text
  : ts.isPropertyAccessExpression(node) ? node.name.text
  : ts.isElementAccessExpression(node) && ts.isStringLiteralLike(node.argumentExpression) ? node.argumentExpression.text : undefined;
const INPUT_METHODS = new Set(['addAttachments', 'removeAttachment', 'focus', 'notify']);

export function translateClientBusiness(text) {
  const fileName = '/client.js';
  const options = { allowJs: true, noResolve: true, noLib: true, target: ts.ScriptTarget.Latest };
  const host = ts.createCompilerHost(options);
  host.getSourceFile = (name) => name === fileName ? ts.createSourceFile(name, text, options.target, true, ts.ScriptKind.JS) : undefined;
  host.fileExists = (name) => name === fileName;
  host.readFile = (name) => name === fileName ? text : undefined;
  const program = ts.createProgram([fileName], options, host), checker = program.getTypeChecker(), source = program.getSourceFile(fileName);
  if (source.parseDiagnostics.length) throw new Error('上游客户端脚本语法无效');
  const symbol = (node) => node && (ts.isShorthandPropertyAssignment(node.parent)
    ? checker.getShorthandAssignmentValueSymbol(node.parent) : checker.getSymbolAtLocation(node));
  const labels = new Map(), functions = new Set(), calls = [], identifiers = [], members = [], bindings = [];
  const enclosing = (node) => {
    for (let current = node.parent; current; current = current.parent) if (isFunction(current) || ts.isGetAccessorDeclaration(current) || ts.isSetAccessorDeclaration(current) || ts.isConstructorDeclaration(current)) return current;
  };
  const functionSymbol = (fn) => symbol(fn.name) ?? (ts.isVariableDeclaration(fn.parent) ? symbol(fn.parent.name) : undefined);
  const functionBySymbol = new Map();
  const walk = (node) => {
    if (isFunction(node)) {
      functions.add(node);
      const owner = functionSymbol(node);
      if (owner) functionBySymbol.set(owner, node);
      if (node.name?.text === 'apply' || ts.isPropertyAssignment(node.parent) && node.parent.name?.text === 'apply') {
        const parameter = node.parameters[0];
        if (parameter && ts.isIdentifier(parameter.name)) labels.set(symbol(parameter.name), 'ctx');
      }
    }
    if (ts.isCallExpression(node)) calls.push(node);
    if (ts.isIdentifier(node)) identifiers.push(node);
    if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) members.push(node);
    if (ts.isBindingElement(node)) bindings.push(node);
    ts.forEachChild(node, walk);
  };
  walk(source);
  const callable = (expression) => functionBySymbol.get(symbol(ts.isPropertyAccessExpression(expression) ? expression.name : expression));
  const label = (node) => {
    if (!node) return;
    if (ts.isParenthesizedExpression(node)) return label(node.expression);
    if (ts.isIdentifier(node)) return labels.get(symbol(node));
    if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) {
      const parent = label(node.expression), name = methodName(node);
      if (parent === 'ctx' && name === 'conversation') return 'conversation';
      if (parent === 'ctx' && name === 'magiComposer') return 'composer';
      if (parent === 'composer') return `composer-op:${name}`;
      if (parent === 'conversation' && name === 'input') return 'inputs';
      if (parent === 'input' && INPUT_METHODS.has(name)) return `operation:${name}`;
      if (parent === 'input' && name === 'snapshot') return 'snapshot';
    }
    if (ts.isCallExpression(node) && methodName(node.expression) === 'for' && label(node.expression.expression) === 'inputs') return 'input';
  };
  const mark = (node, value) => {
    const key = symbol(node);
    if (!key || !value || labels.get(key) === value) return false;
    if (labels.has(key)) throw new Error('客户端宿主对象存在不兼容别名，不能证明业务调用归属');
    labels.set(key, value); return true;
  };
  let changed = true, passes = 0;
  while (changed) {
    if (++passes > 100) throw new Error('客户端业务数据流未收敛');
    changed = false;
    const propagate = (node) => {
      if (ts.isVariableDeclaration(node)) {
        if (ts.isIdentifier(node.name)) changed = mark(node.name, label(node.initializer)) || changed;
        else if (ts.isObjectBindingPattern(node.name) && label(node.initializer) === 'input') for (const binding of node.name.elements) {
          const name = binding.propertyName?.text ?? binding.name.text;
          if (!ts.isIdentifier(binding.name) || !INPUT_METHODS.has(name) || binding.dotDotDotToken) throw new Error('客户端输入端口解构含未支持操作');
          changed = mark(binding.name, `operation:${name}`) || changed;
        }
        else if (ts.isObjectBindingPattern(node.name) && ['ctx','composer'].includes(label(node.initializer))) for (const binding of node.name.elements) {
          if (!ts.isIdentifier(binding.name) || binding.dotDotDotToken) continue;
          const name = binding.propertyName?.text ?? binding.name.text;
          const kind = label(node.initializer) === 'composer' ? `composer-op:${name}`
            : name === 'magiComposer' ? 'composer' : undefined;
          changed = mark(binding.name, kind) || changed;
        }
      }
      if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken && ts.isIdentifier(node.left)) {
        if (labels.has(symbol(node.left)) && !label(node.right)) throw new Error('客户端宿主别名被重赋为未知值');
        changed = mark(node.left, label(node.right)) || changed;
      }
      if (ts.isCallExpression(node)) {
        const target = callable(node.expression);
        if (target) for (let index = 0; index < node.arguments.length; index++) {
          const parameter = target.parameters[index];
          if (parameter && ts.isIdentifier(parameter.name)) changed = mark(parameter.name, label(node.arguments[index])) || changed;
        }
        if (['inject', 'scope'].includes(methodName(node.expression)) && label(node.expression.expression) === 'ctx') {
          for (const argument of node.arguments) if (isFunction(argument) && argument.parameters[0] && ts.isIdentifier(argument.parameters[0].name)) changed = mark(argument.parameters[0].name, 'ctx') || changed;
        }
      }
      ts.forEachChild(node, propagate);
    };
    propagate(source);
  }

  // 只对可证明归属的输入端口改写；未知别名/动态成员不能绕过等待插入。
  for (const identifier of identifiers) {
    const kind = labels.get(symbol(identifier));
    if (!['inputs', 'input'].includes(kind) && !kind?.startsWith('operation:')) continue;
    const parent = identifier.parent;
    if (ts.isVariableDeclaration(parent) && (parent.name === identifier || parent.initializer === identifier)
      || ts.isParameter(parent) && parent.name === identifier || ts.isBindingElement(parent) && parent.name === identifier) continue;
    if ((ts.isPropertyAccessExpression(parent) || ts.isElementAccessExpression(parent)) && parent.expression === identifier) {
      const name = methodName(parent);
      if (kind === 'input' && (INPUT_METHODS.has(name) || name === 'snapshot') || kind === 'inputs' && name === 'for') continue;
      if (kind === 'input' && name) throw new Error(`原包使用了未支持的客户端输入操作“${name}”。请尝试在线更新插件，或选择使用当前 DSH 输入接口的版本。`);
    }
    if (ts.isCallExpression(parent)) {
      if (parent.expression === identifier && kind.startsWith('operation:') || callable(parent.expression)) continue;
    }
    if (ts.isBinaryExpression(parent) && parent.operatorToken.kind === ts.SyntaxKind.EqualsToken && ts.isIdentifier(parent.left) && label(parent.right) === kind) continue;
    throw new Error('客户端输入端口逃逸或使用动态/未支持成员，不能证明全部宿主回执已等待');
  }

  const awaited = new Set(), asyncFunctions = new Set(), operations = new Set();
  const alreadyAsync = (fn) => fn.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.AsyncKeyword);
  const requireAsync = (call) => {
    if (ts.isAwaitExpression(call.parent)) return;
    const fn = enclosing(call);
    if (!fn || !isFunction(fn) || fn.asteriskToken) throw new Error('客户端业务调用位于同步构造、访问器、生成器或顶层，不能安全等待宿主回执');
    awaited.add(call); asyncFunctions.add(fn);
  };
  for (const call of calls) {
    const operation = label(call.expression);
    if (operation?.startsWith('operation:')) { operations.add(operation.slice(10)); requireAsync(call); }
  }
  const promiseConsumed = (call) => ts.isAwaitExpression(call.parent) || ts.isVoidExpression(call.parent)
    || ts.isPropertyAccessExpression(call.parent) && ['then', 'catch', 'finally'].includes(call.parent.name.text);
  changed = true;
  while (changed) {
    changed = false;
    for (const call of calls) {
      const target = callable(call.expression);
      if (!target || !asyncFunctions.has(target) || alreadyAsync(target) || promiseConsumed(call) || awaited.has(call)) continue;
      const count = asyncFunctions.size;
      requireAsync(call);
      if (count !== asyncFunctions.size) changed = true;
    }
  }
  for (const fn of asyncFunctions) {
    if (alreadyAsync(fn)) continue;
    const key = functionSymbol(fn);
    if (!key) {
      const parent = fn.parent;
      const event = ts.isCallExpression(parent) && (['setTimeout', 'setInterval', 'queueMicrotask'].includes(parent.expression.getText(source))
        || ['addEventListener', 'on', 'once'].includes(methodName(parent.expression)));
      if (!event && !(ts.isPropertyAssignment(parent) && /^on[A-Z]/.test(parent.name?.text ?? '')) && fn.name?.text !== 'apply') throw new Error('客户端同步回调没有可验证的异步调用合同');
      continue;
    }
    for (const identifier of identifiers) {
      if (symbol(identifier) !== key || identifier === fn.name || ts.isVariableDeclaration(identifier.parent) && identifier.parent.name === identifier) continue;
      const parent = identifier.parent;
      if (ts.isCallExpression(parent) && parent.expression === identifier) continue;
      if (ts.isCallExpression(parent) && ['setTimeout', 'setInterval', 'queueMicrotask'].includes(parent.expression.getText(source))) continue;
      if (ts.isCallExpression(parent) && ['addEventListener', 'on', 'once'].includes(methodName(parent.expression))) continue;
      if (ts.isPropertyAssignment(parent) && /^on[A-Z]/.test(parent.name?.text ?? '')) continue;
      // DSH ModuleLoader 唯一公开入口 apply 本就由宿主等待初始化；其他同步导出不能偷偷改为 Promise。
      if (fn.name?.text === 'apply' && (ts.isShorthandPropertyAssignment(parent) || ts.isArrowFunction(enclosing(identifier)) || ts.isPropertyAssignment(parent))) continue;
      throw new Error(`客户端同步函数 ${fn.name?.text ?? '(匿名)'} 逃逸到渲染、未知回调或外部导出，不能安全改为异步`);
    }
  }
  const edits = [];
  for (const call of awaited) { edits.push([call.getStart(source), '(await '], [call.end, ')']); }
  // 仅已证明并插入等待的调用能访问 Async 端口。原同步名字在运行期明确拒绝，
  // 未识别的动态路径不会拿到 Promise 后误判为 true。
  for (const member of members) if (label(member)?.startsWith('operation:')) edits.push([
    ts.isPropertyAccessExpression(member) ? member.name.end : member.argumentExpression.end - 1, 'Async',
  ]);
  for (const binding of bindings) if (labels.get(symbol(binding.name))?.startsWith('operation:')) {
    if (binding.propertyName) edits.push([ts.isIdentifier(binding.propertyName) ? binding.propertyName.end : binding.propertyName.end - 1, 'Async']);
    else edits.push([binding.name.getStart(source), `${binding.name.text}Async: `]);
  }
  for (const fn of asyncFunctions) if (!alreadyAsync(fn)) {
    const modifiers = fn.modifiers ?? [];
    edits.push([modifiers.length ? modifiers.at(-1).end + 1 : fn.getStart(source), 'async ']);
  }
  let code = text;
  for (const [at, insertion, end = at] of edits.sort((a, b) => b[0] - a[0])) code = code.slice(0, at) + insertion + code.slice(end);
  const syntax = ts.createSourceFile('result.js', code, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  if (syntax.parseDiagnostics.length) throw new Error('客户端业务异步转换结果语法无效');
  const composerTargets = [...new Set([...members.map(member=>label(member)),...labels.values()]
    .filter(kind=>kind?.startsWith('composer-op:')).flatMap(kind=>kind==='composer-op:submitText' ? ['text']
      : ['addImages','removeImage','focus','notify','refresh','getSnapshot','subscribe'].some(name=>kind===`composer-op:${name}`) ? ['images'] : []))].sort();
  return { code, operations: [...operations].sort(), composerTargets, changedFunctions: [...asyncFunctions].filter((fn) => !alreadyAsync(fn)).length, awaitedCalls: awaited.size };
}

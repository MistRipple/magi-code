// 只识别能从源码证明的可选路径；能力状态仍保留 partial/dropped，未知能力仍阻塞。
import ts from 'typescript';

const functionLike = (node) => ts.isArrowFunction(node) || ts.isFunctionExpression(node) || ts.isFunctionDeclaration(node) || ts.isMethodDeclaration(node);
const nameOf = (node) => node && (ts.isIdentifier(node) || ts.isStringLiteralLike(node)) ? node.text : undefined;

function catches(node) {
  for (let current = node; current.parent && !functionLike(current); current = current.parent) {
    const parent = current.parent;
    if (!ts.isTryStatement(parent) || parent.tryBlock !== current || !parent.catchClause) continue;
    let rethrows = false;
    const visit = (item) => {
      if (functionLike(item)) return;
      if (ts.isThrowStatement(item)) rethrows = true;
      ts.forEachChild(item, visit);
    };
    visit(parent.catchClause.block);
    if (!rethrows) return true;
  }
  return false;
}

/** 同步注册失败被就地捕获，或通过对象回调交给同文件函数且其所有调用均捕获。 */
export function guardedRegistration(node, source) {
  if (catches(node)) return true;
  const callback = node.parent;
  if (!ts.isArrowFunction(callback) || callback.body !== node || callback.modifiers?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword)) return false;
  const property = callback.parent;
  if (!ts.isPropertyAssignment(property) || !ts.isObjectLiteralExpression(property.parent)) return false;
  const object = property.parent, call = object.parent;
  if (!ts.isCallExpression(call) || !ts.isIdentifier(call.expression)) return false;
  const target = source.statements.find((statement) => ts.isFunctionDeclaration(statement) && statement.name?.text === call.expression.text);
  const parameter = target?.parameters[call.arguments.indexOf(object)]?.name;
  if (!parameter || !ts.isIdentifier(parameter)) return false;
  let uses = 0, safe = true;
  const visited = new Map();
  const inspect = (fn, argument) => {
    const seen = visited.get(fn) ?? new Set();
    if (seen.has(argument.text)) return;
    seen.add(argument.text); visited.set(fn, seen);
    const visit = (item) => {
      if (ts.isIdentifier(item) && item.text === argument.text) {
        const access = item.parent;
        if (ts.isPropertyAccessExpression(access) && access.expression === item) {
          if (access.name.text === nameOf(property.name)) {
            uses++;
            if (!ts.isCallExpression(access.parent) || access.parent.expression !== access || !catches(access.parent)) safe = false;
          }
        } else if (ts.isCallExpression(access) && ts.isIdentifier(access.expression)) {
          // 同文件具名函数可继续追踪；对象被保存、返回或交给未知调用方仍属无法证明。
          const next = source.statements.find((statement) => ts.isFunctionDeclaration(statement) && statement.name?.text === access.expression.text);
          const nextArgument = next?.parameters[access.arguments.indexOf(item)]?.name;
          if (!nextArgument || !ts.isIdentifier(nextArgument)) safe = false;
          else inspect(next, nextArgument);
        } else safe = false;
      }
      ts.forEachChild(item, visit);
    };
    visit(fn.body);
  };
  inspect(target, parameter);
  return uses > 0 && safe;
}

/** ctx.inject 的回调只有在目标服务出现时才执行；export inject 仍是必需依赖。 */
export function conditionalService(node, service, isContext) {
  for (let current = node.parent; current; current = current.parent) {
    if (!functionLike(current)) continue;
    const call = current.parent;
    if (!ts.isCallExpression(call) || !ts.isPropertyAccessExpression(call.expression)
      || call.expression.name.text !== 'inject' || !isContext(call.expression.expression, call)) continue;
    const dependencies = call.arguments[0];
    if (dependencies && ts.isArrayLiteralExpression(dependencies)
      && dependencies.elements.some((item) => ts.isStringLiteralLike(item) && item.text === service)) return true;
  }
  return false;
}

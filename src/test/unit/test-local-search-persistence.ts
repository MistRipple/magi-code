/**
 * 索引持久化 + 及时更新 — 功能测试
 * 运行：npm run compile && node out/test/unit/test-local-search-persistence.js
 */
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { InvertedIndex } from '../../knowledge/indexing/inverted-index';
import { SymbolIndex } from '../../knowledge/indexing/symbol-index';
import { DependencyGraph } from '../../knowledge/indexing/dependency-graph';
import { IndexPersistence } from '../../knowledge/persistence/index-persistence';

// ── 测试基础设施 ─────────────────────────────────────────────────────────
let total = 0, passed = 0, failed = 0;
const failures: string[] = [];
function ok(c: boolean, m: string) {
  total++; if (c) { passed++; console.log(`  ✅ ${m}`); }
  else { failed++; failures.push(m); console.log(`  ❌ ${m}`); }
}
function eq(a: unknown, b: unknown, m: string) { ok(a === b, `${m} (got=${a} want=${b})`); }
function gt(a: number, b: number, m: string) { ok(a > b, `${m} (got=${a} want>${b})`); }

const FILES = [
  { path: 'auth-service.ts', type: 'source' as const },
  { path: 'user-model.ts', type: 'source' as const },
  { path: 'token-manager.ts', type: 'source' as const },
];

function mkProject(): string {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), 'mtest-'));
  fs.writeFileSync(path.join(d, 'auth-service.ts'), [
    "import { UserModel } from './user-model';",
    "import { TokenManager } from './token-manager';",
    'export class AuthService {',
    '  private tm: TokenManager;',
    '  constructor() { this.tm = new TokenManager(); }',
    '  async login(u: string, p: string): Promise<string> {',
    '    const user = await UserModel.findByName(u);',
    '    if (!user) throw new Error("fail");',
    '    return this.tm.generate(user.id);',
    '  }',
    '  async validateToken(t: string): Promise<boolean> {',
    '    return this.tm.verify(t);',
    '  }',
    '}',
  ].join('\n'));
  fs.writeFileSync(path.join(d, 'user-model.ts'), [
    'export interface User { id: string; username: string; password: string; }',
    'export class UserModel {',
    '  static async findByName(u: string): Promise<User|null> { return null; }',
    '  static async findById(id: string): Promise<User|null> { return null; }',
    '}',
  ].join('\n'));
  fs.writeFileSync(path.join(d, 'token-manager.ts'), [
    'export class TokenManager {',
    '  generate(uid: string): string { return "tok_" + uid; }',
    '  verify(t: string): boolean { return t.startsWith("tok_"); }',
    '}',
  ].join('\n'));
  fs.mkdirSync(path.join(d, '.magi', 'cache'), { recursive: true });
  return d;
}
function rmDir(d: string) { fs.rmSync(d, { recursive: true, force: true }); }

// ── Test 1: InvertedIndex 序列化 ─────────────────────────────────────────
async function t1(d: string) {
  console.log('\n📋 T1: InvertedIndex 序列化/反序列化');
  const a = new InvertedIndex();
  await a.buildFromFiles(d, FILES);
  const h1 = a.search(['login', 'token'], 5);
  gt(h1.length, 0, '搜索有结果');
  const snap = a.toJSON();
  gt(snap.totalDocuments, 0, '快照有文档');
  const b = new InvertedIndex();
  b.fromJSON(snap);
  eq(b.search(['login', 'token'], 5).length, h1.length, '恢复后结果数一致');
  ok(b.getDocumentMeta('auth-service.ts') !== undefined, '元数据可取');
  eq(b.getStats().totalDocuments, a.getStats().totalDocuments, '文档数一致');
  eq(b.getStats().uniqueTokens, a.getStats().uniqueTokens, 'token数一致');
}

// ── Test 2: SymbolIndex 序列化 ───────────────────────────────────────────
async function t2(d: string) {
  console.log('\n📋 T2: SymbolIndex 序列化/反序列化');
  const a = new SymbolIndex();
  await a.buildFromFiles(d, FILES);
  const h1 = a.search('AuthService', 5);
  gt(h1.length, 0, '搜索 AuthService 有结果');
  const snap = a.toJSON();
  gt(snap.symbols.length, 0, '快照有符号');
  const b = new SymbolIndex();
  b.fromJSON(snap);
  eq(b.search('AuthService', 5).length, h1.length, '恢复后数一致');
  gt(b.search('UserModel', 5).length, 0, '恢复后可搜 UserModel');
  eq(b.getStats().uniqueSymbols, a.getStats().uniqueSymbols, '符号数一致');
}

// ── Test 3: DependencyGraph 序列化 + 增量 ────────────────────────────────
async function t3(d: string) {
  console.log('\n📋 T3: DependencyGraph 序列化 + 增量');
  const g = new DependencyGraph();
  await g.buildFromFiles(d, FILES);
  ok(g.getDependencies('auth-service.ts').includes('user-model.ts'), 'auth→user');
  ok(g.getDependencies('auth-service.ts').includes('token-manager.ts'), 'auth→token');
  ok(g.getDependents('token-manager.ts').includes('auth-service.ts'), 'token←auth');
  const snap = g.toJSON();
  gt(snap.edges.length, 0, '快照有边');
  const g2 = new DependencyGraph();
  g2.fromJSON(snap, d, new Set(FILES.map(f => f.path)));
  ok(g2.getDependencies('auth-service.ts').includes('user-model.ts'), '恢复 auth→user');
  ok(g2.getDependencies('auth-service.ts').includes('token-manager.ts'), '恢复 auth→token');
  g2.removeFile('token-manager.ts');
  ok(!g2.getDependencies('auth-service.ts').includes('token-manager.ts'), 'rm token ok');
  ok(g2.getDependencies('auth-service.ts').includes('user-model.ts'), 'rm后user仍在');
  fs.writeFileSync(path.join(d, 'auth-service.ts'), [
    "import { UserModel } from './user-model';",
    'export class AuthService { login(u:string) { return ""; } }',
  ].join('\n'));
  g2.updateFile(d, 'auth-service.ts');
  ok(g2.getDependencies('auth-service.ts').includes('user-model.ts'), 'update后user在');
  eq(g2.getDependencies('auth-service.ts').length, 1, 'update后仅1dep');
}

// ── Test 4: Persistence Save/Load ────────────────────────────────────────
async function t4(d: string) {
  console.log('\n📋 T4: IndexPersistence 保存/加载');
  const ii = new InvertedIndex(), si = new SymbolIndex(), dg = new DependencyGraph();
  await Promise.all([
    ii.buildFromFiles(d, FILES), si.buildFromFiles(d, FILES), dg.buildFromFiles(d, FILES),
  ]);
  const p = new IndexPersistence(d);
  p.save(d, ii, si, dg, FILES);
  ok(fs.existsSync(path.join(d, '.magi', 'cache', 'search-index.json')), '缓存文件在');
  const snap = p.load();
  ok(snap !== null, 'load非null');
  eq(snap!.fileManifest.length, 3, 'manifest=3');
  ok(snap!.invertedIndex !== undefined, '含inverted');
  ok(snap!.symbolIndex !== undefined, '含symbol');
  ok(snap!.dependencyGraph !== undefined, '含dep');
  p.dispose();
}

// ── Test 5: Freshness 验证 ──────────────────────────────────────────────
async function t5(d: string) {
  console.log('\n📋 T5: 新鲜度验证');
  const ii = new InvertedIndex(), si = new SymbolIndex(), dg = new DependencyGraph();
  await Promise.all([
    ii.buildFromFiles(d, FILES), si.buildFromFiles(d, FILES), dg.buildFromFiles(d, FILES),
  ]);
  const p = new IndexPersistence(d);
  p.save(d, ii, si, dg, FILES);
  await new Promise(r => setTimeout(r, 150));
  // 修改 user-model
  fs.writeFileSync(path.join(d, 'user-model.ts'), [
    'export interface User { id: string; username: string; role: string; }',
    'export class UserModel {',
    '  static async findByName(u: string): Promise<User|null> { return null; }',
    '}',
  ].join('\n'));
  // 新增 logger.ts
  fs.writeFileSync(path.join(d, 'logger.ts'), 'export function log(m: string) { console.log(m); }');
  const snap = p.load()!;
  const newFiles = [...FILES, { path: 'logger.ts', type: 'source' as const }];
  const fr = p.validateFreshness(d, snap, newFiles);
  gt(fr.unchanged.length, 0, '有unchanged');
  gt(fr.modified.length, 0, '有modified');
  eq(fr.added.length, 1, 'added=1');
  eq(fr.deleted.length, 0, 'deleted=0');
  // 删除文件检测
  fs.unlinkSync(path.join(d, 'token-manager.ts'));
  const fr2 = p.validateFreshness(d, snap, FILES.filter(f => f.path !== 'token-manager.ts'));
  eq(fr2.deleted.length, 1, 'deleted=1');
  // 恢复
  fs.writeFileSync(path.join(d, 'token-manager.ts'), [
    'export class TokenManager {',
    '  generate(uid: string): string { return "tok_" + uid; }',
    '  verify(t: string): boolean { return t.startsWith("tok_"); }',
    '}',
  ].join('\n'));
  p.dispose();
}

// ── Test 6: restoreAndSync 端到端 ────────────────────────────────────────
async function t6(d: string) {
  console.log('\n📋 T6: restoreAndSync 端到端');
  // 构建 + 保存
  const ii1 = new InvertedIndex(), si1 = new SymbolIndex(), dg1 = new DependencyGraph();
  await Promise.all([
    ii1.buildFromFiles(d, FILES), si1.buildFromFiles(d, FILES), dg1.buildFromFiles(d, FILES),
  ]);
  const p1 = new IndexPersistence(d);
  p1.save(d, ii1, si1, dg1, FILES);
  p1.dispose();

  // 模拟重启：新实例从缓存恢复
  const ii2 = new InvertedIndex(), si2 = new SymbolIndex(), dg2 = new DependencyGraph();
  const p2 = new IndexPersistence(d);
  const restored = p2.restoreAndSync(d, ii2, si2, dg2, FILES);
  ok(restored, 'restoreAndSync 返回 true');
  // 验证恢复后的搜索能力
  gt(ii2.search(['login'], 5).length, 0, '恢复后 inverted 可搜');
  gt(si2.search('AuthService', 5).length, 0, '恢复后 symbol 可搜');
  ok(dg2.getDependencies('auth-service.ts').includes('user-model.ts'), '恢复后 dep 正常');
  p2.dispose();
}

// ── Test 7: 增量更新链路 ─────────────────────────────────────────────────
async function t7(d: string) {
  console.log('\n📋 T7: 增量更新链路');
  const ii = new InvertedIndex(), si = new SymbolIndex(), dg = new DependencyGraph();
  await Promise.all([
    ii.buildFromFiles(d, FILES), si.buildFromFiles(d, FILES), dg.buildFromFiles(d, FILES),
  ]);
  // 文件变更：新增内容
  fs.writeFileSync(path.join(d, 'auth-service.ts'), [
    "import { UserModel } from './user-model';",
    "import { TokenManager } from './token-manager';",
    'export class AuthService {',
    '  private tm: TokenManager;',
    '  constructor() { this.tm = new TokenManager(); }',
    '  async login(u: string, p: string): Promise<string> {',
    '    const user = await UserModel.findByName(u);',
    '    return this.tm.generate(user.id);',
    '  }',
    '  async refreshSession(sid: string): Promise<void> {',
    '    console.log("refreshing session", sid);',
    '  }',
    '}',
  ].join('\n'));
  ii.updateFile(d, 'auth-service.ts');
  si.updateFile(d, 'auth-service.ts');
  dg.updateFile(d, 'auth-service.ts');
  gt(ii.search(['refreshsession', 'session'], 5).length, 0, '增量后搜到 refreshSession');
  // 文件删除
  ii.removeFile('token-manager.ts');
  si.removeFile('token-manager.ts');
  dg.removeFile('token-manager.ts');
  eq(ii.search(['tok'], 5).filter(h => h.filePath === 'token-manager.ts').length, 0,
    '删除后 token-manager 不在倒排结果');
  eq(si.search('TokenManager', 5).length, 0, '删除后 TokenManager 搜不到');
}

// ── Main ─────────────────────────────────────────────────────────────────
async function main() {
  console.log('========================================');
  console.log('索引持久化 + 及时更新 功能测试');
  console.log('========================================');
  const d = mkProject();
  try {
    await t1(d);
    // 重建项目（t3 会修改文件）
    rmDir(d); const d2 = mkProject();
    await t2(d2);
    rmDir(d2); const d3 = mkProject();
    await t3(d3);
    rmDir(d3); const d4 = mkProject();
    await t4(d4);
    rmDir(d4); const d5 = mkProject();
    await t5(d5);
    rmDir(d5); const d6 = mkProject();
    await t6(d6);
    rmDir(d6); const d7 = mkProject();
    await t7(d7);
    rmDir(d7);
  } catch (e) {
    console.error('\n💥 测试异常:', e);
    rmDir(d);
  }
  console.log('\n========================================');
  console.log(`结果: ${passed}/${total} 通过, ${failed} 失败`);
  if (failures.length > 0) {
    console.log('\n失败用例:');
    failures.forEach(f => console.log(`  • ${f}`));
  }
  console.log('========================================');
  process.exit(failed > 0 ? 1 : 0);
}

main();

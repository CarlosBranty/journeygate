#!/usr/bin/env node
// Deterministic test-quality checks on a Playwright TypeScript spec
// (docs/contracts/qa-coder-protocol.md, "Deterministic test-quality checks").
//
// Uses the TypeScript compiler API (syntax only: no type checking, no I/O
// besides stdin). Invoked by guard-sandbox `NodeSpecQuality`:
//
//   node spec-quality.mjs --ticket SHOP-1 [--api-base /api/]... [--file name.spec.ts] < spec.ts
//
// Prints `{"violations":[{"id","line","message"}]}` and exits 0 (violations
// are data, not a process failure). Usage errors exit 2.
//
// `typescript` is resolved from this file's node_modules, or from the
// directory named by GUARD_TYPESCRIPT_DIR.
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const require = createRequire(import.meta.url);
const ts = process.env.GUARD_TYPESCRIPT_DIR
  ? require(process.env.GUARD_TYPESCRIPT_DIR)
  : require('typescript');

const TEST_ROOTS = new Set(['test', 'it']);
const SUITE_WORDS = new Set(['describe']);
const NON_DEF_SEGMENTS = new Set([
  'describe', 'step', 'beforeEach', 'afterEach', 'beforeAll', 'afterAll', 'use', 'extend', 'info', 'setTimeout', 'slow',
]);
const TICKET_TAG = /^@ticket:[A-Z][A-Z0-9_]*-\d+$/;
const JOURNEY_TAG = /^@journey:[a-z0-9][a-z0-9._-]*$/;

/** Dotted name of a callee (`test.describe.only`), or null when not a plain chain. */
function calleeName(expr) {
  if (ts.isIdentifier(expr)) return expr.text;
  if (ts.isPropertyAccessExpression(expr)) {
    const left = calleeName(expr.expression);
    return left === null ? null : `${left}.${expr.name.text}`;
  }
  return null;
}

/** Last property name of a call (`waitForTimeout` in `a().b.waitForTimeout()`). */
function lastSegment(expr) {
  if (ts.isPropertyAccessExpression(expr)) return expr.name.text;
  if (ts.isIdentifier(expr)) return expr.text;
  return null;
}

/** Root identifier of `expect(...)`, `expect.soft(...)`, `expect.poll(...)`. */
function rootIdentifier(expr) {
  while (ts.isPropertyAccessExpression(expr)) expr = expr.expression;
  return ts.isIdentifier(expr) ? expr.text : null;
}

function isFunctionLike(n) {
  return n && (ts.isArrowFunction(n) || ts.isFunctionExpression(n));
}

function stringValue(n) {
  if (!n) return null;
  if (ts.isStringLiteral(n) || ts.isNoSubstitutionTemplateLiteral(n)) return n.text;
  if (ts.isTemplateExpression(n)) {
    return n.head.text + n.templateSpans.map((s) => ` ${s.literal.text}`).join('');
  }
  return null;
}

function isLiteral(n) {
  if (!n) return false;
  if (ts.isParenthesizedExpression(n)) return isLiteral(n.expression);
  if (ts.isPrefixUnaryExpression(n)) return isLiteral(n.operand);
  if (ts.isIdentifier(n)) return n.text === 'undefined' || n.text === 'NaN' || n.text === 'Infinity';
  return (
    ts.isStringLiteral(n) ||
    ts.isNumericLiteral(n) ||
    ts.isBigIntLiteral(n) ||
    ts.isNoSubstitutionTemplateLiteral(n) ||
    ts.isRegularExpressionLiteral(n) ||
    n.kind === ts.SyntaxKind.TrueKeyword ||
    n.kind === ts.SyntaxKind.FalseKeyword ||
    n.kind === ts.SyntaxKind.NullKeyword
  );
}

function titleTags(title) {
  return (title ?? '').split(/\s+/).filter((w) => w.startsWith('@'));
}

/** Tag strings of a `{ tag: '...' | [...] }` details argument. */
function optionTags(n) {
  if (!n || !ts.isObjectLiteralExpression(n)) return [];
  const out = [];
  for (const p of n.properties) {
    if (!ts.isPropertyAssignment(p) || p.name.getText() !== 'tag') continue;
    const v = p.initializer;
    if (ts.isArrayLiteralExpression(v)) {
      for (const e of v.elements) {
        const s = stringValue(e);
        if (s !== null) out.push(s);
      }
    } else {
      const s = stringValue(v);
      if (s !== null) out.push(s);
    }
  }
  return out;
}

/** Classifies a call: 'test' (a test definition), 'suite', or null. */
function definitionKind(call) {
  const name = calleeName(call.expression);
  if (name === null) return null;
  const parts = name.split('.');
  const hasFn = call.arguments.some(isFunctionLike);
  if (!hasFn || stringValue(call.arguments[0]) === null) return null;
  if (parts.some((p) => SUITE_WORDS.has(p)) && (TEST_ROOTS.has(parts[0]) || parts[0] === 'describe')) return 'suite';
  if (['fdescribe', 'xdescribe'].includes(parts[0])) return 'suite';
  if (['fit', 'xit', 'xtest'].includes(parts[0])) return 'test';
  if (TEST_ROOTS.has(parts[0]) && !parts.slice(1).some((p) => NON_DEF_SEGMENTS.has(p))) return 'test';
  return null;
}

function containsExpect(node) {
  let found = false;
  const visit = (n) => {
    if (found) return;
    if (ts.isCallExpression(n) && rootIdentifier(n.expression) === 'expect') {
      found = true;
      return;
    }
    ts.forEachChild(n, visit);
  };
  visit(node);
  return found;
}

/** A `throw` in `block`, not counting nested functions. */
function containsThrow(block) {
  let found = false;
  const visit = (n) => {
    if (found || isFunctionLike(n) || ts.isFunctionDeclaration(n)) return;
    if (ts.isThrowStatement(n)) {
      found = true;
      return;
    }
    ts.forEachChild(n, visit);
  };
  ts.forEachChild(block, visit);
  return found;
}

/** Node built-in modules a spec has no business loading (host access). */
const HOST_MODULES = new Set([
  'child_process', 'fs', 'fs/promises', 'net', 'tls', 'dgram', 'dns', 'dns/promises', 'http', 'https', 'http2',
  'os', 'process', 'vm', 'worker_threads', 'cluster', 'module', 'inspector', 'v8', 'repl', 'readline',
]);

/** A module specifier a spec may not load: `node:*` or a host built-in. */
function isHostModule(spec) {
  return spec.startsWith('node:') || HOST_MODULES.has(spec);
}

/**
 * Host access by the spec itself (defense in depth: the red-first and gate
 * runs also clear the environment). `null` when `n` is fine.
 */
function hostAccess(n) {
  if ((ts.isImportDeclaration(n) || ts.isExportDeclaration(n)) && n.moduleSpecifier) {
    const spec = stringValue(n.moduleSpecifier);
    if (spec !== null && isHostModule(spec)) return `imports ${spec}`;
  }
  if (ts.isImportEqualsDeclaration(n) && ts.isExternalModuleReference(n.moduleReference)) {
    const spec = stringValue(n.moduleReference.expression);
    if (spec !== null && isHostModule(spec)) return `imports ${spec}`;
  }
  if (ts.isCallExpression(n)) {
    const dynamicImport = n.expression.kind === ts.SyntaxKind.ImportKeyword;
    const isRequire = ts.isIdentifier(n.expression) && n.expression.text === 'require';
    if (dynamicImport || isRequire) {
      const spec = stringValue(n.arguments[0]);
      if (spec === null) return `${dynamicImport ? 'import' : 'require'}(...) of a computed module`;
      if (isHostModule(spec)) return `loads ${spec}`;
    }
    if (ts.isIdentifier(n.expression) && n.expression.text === 'eval') return 'calls eval(...)';
  }
  if (ts.isNewExpression(n) && ts.isIdentifier(n.expression) && n.expression.text === 'Function') {
    return 'builds code with new Function(...)';
  }
  if (ts.isIdentifier(n) && n.text === 'process') {
    const p = n.parent;
    const isPropertyName = p && ts.isPropertyAccessExpression(p) && p.name === n;
    const isKey = p && (ts.isPropertyAssignment(p) || ts.isMethodDeclaration(p)) && p.name === n;
    if (!isPropertyName && !isKey) return 'reads the process object';
  }
  if (ts.isPropertyAccessExpression(n) && n.name.text === 'process' && ts.isIdentifier(n.expression)
    && ['globalThis', 'global', 'window', 'self'].includes(n.expression.text)) {
    return `reads ${n.expression.text}.process`;
  }
  return null;
}

function routePatternText(n) {
  if (!n) return null;
  if (ts.isRegularExpressionLiteral(n)) return n.text.replace(/\\/g, '');
  return stringValue(n);
}

/**
 * Runs every check. `options`: `{ ticket: 'KEY', apiBases: ['/api/', 'http://host'] }`.
 * Returns `[{ id, line, message }]` sorted by line, then id.
 */
export function checkSpec(source, options, fileName = 'spec.ts') {
  const { ticket, apiBases = ['/api/'] } = options;
  const sf = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const violations = [];
  const add = (id, node, message) => {
    const line = sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
    violations.push({ id, line, message });
  };
  for (const d of sf.parseDiagnostics ?? []) {
    const line = sf.getLineAndCharacterOfPosition(d.start ?? 0).line + 1;
    violations.push({ id: 'parse_error', line, message: ts.flattenDiagnosticMessageText(d.messageText, ' ') });
  }
  const wantTag = `@ticket:${ticket}`;

  const checkTags = (node, tags) => {
    for (const t of tags) {
      if (!TICKET_TAG.test(t) && !JOURNEY_TAG.test(t)) {
        add('journey_tag_format', node, `tag ${JSON.stringify(t)} is not @ticket:<KEY> or @journey:<id>`);
      }
    }
  };

  const visit = (n, suiteTags) => {
    const host = hostAccess(n);
    if (host !== null) add('no_host_access', n, `the spec ${host}; specs may only drive the app through Playwright`);
    if (ts.isCallExpression(n)) {
      const name = calleeName(n.expression);
      const parts = name ? name.split('.') : [];
      const root = parts[0];
      const last = parts[parts.length - 1];
      if (name !== null) {
        const testish = TEST_ROOTS.has(root) || root === 'describe';
        if ((testish && last === 'only' && parts.length > 1) || root === 'fit' || root === 'fdescribe') {
          add('no_focus', n, `${name}(...) focuses tests`);
        }
        if ((testish && (last === 'skip' || last === 'fixme') && parts.length > 1) || ['xit', 'xdescribe', 'xtest'].includes(root)) {
          add('no_skip', n, `${name}(...) skips tests`);
        }
      }
      if (lastSegment(n.expression) === 'waitForTimeout' && ts.isPropertyAccessExpression(n.expression)) {
        add('no_wait_for_timeout', n, 'waitForTimeout(...) is a fixed sleep; wait for a condition instead');
      }
      if (ts.isPropertyAccessExpression(n.expression) && n.expression.name.text === 'route') {
        const recv = n.expression.expression;
        const recvName = ts.isIdentifier(recv) ? recv.text : '';
        if (/page|context/i.test(recvName)) {
          const pat = routePatternText(n.arguments[0]);
          if (pat !== null && apiBases.some((b) => b && pat.includes(b))) {
            add('no_self_mock', n, `${recvName}.route mocks the app's own API (${pat})`);
          }
        }
      }
      if (rootIdentifier(n.expression) === 'expect' && n.arguments.length > 0 && isLiteral(n.arguments[0])) {
        add('no_trivial_expect', n, `expect(${n.arguments[0].getText(sf)}) asserts on a literal`);
      }

      const kind = definitionKind(n);
      if (kind === 'suite') {
        const tags = [...titleTags(stringValue(n.arguments[0])), ...optionTags(n.arguments[1])];
        checkTags(n, tags);
        n.arguments.forEach((a) => visit(a, [...suiteTags, ...tags]));
        return;
      }
      if (kind === 'test') {
        const tags = [...titleTags(stringValue(n.arguments[0])), ...optionTags(n.arguments[1])];
        checkTags(n, tags);
        if (![...suiteTags, ...tags].includes(wantTag)) {
          add('ticket_tag', n, `test is missing the ${wantTag} tag`);
        }
        const fn = [...n.arguments].reverse().find(isFunctionLike);
        if (fn && ts.isBlock(fn.body) && fn.body.statements.length === 0) {
          add('no_empty_test', n, 'test callback has no statements');
        } else if (fn && !containsExpect(fn.body)) {
          add('has_expect', n, 'test has no expect(...) call');
        }
      }
    }
    if (ts.isTryStatement(n) && n.catchClause && containsExpect(n.tryBlock) && !containsThrow(n.catchClause.block)) {
      add('no_swallowed_assertions', n, 'try { expect } catch without rethrow swallows the assertion');
    }
    ts.forEachChild(n, (c) => visit(c, suiteTags));
  };
  visit(sf, []);
  violations.sort((a, b) => a.line - b.line || a.id.localeCompare(b.id));
  return violations;
}

function parseArgs(argv) {
  const out = { ticket: null, apiBases: [], file: 'spec.ts' };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const v = argv[i + 1];
    if (a === '--ticket' && v) { out.ticket = v; i++; }
    else if (a === '--api-base' && v) { out.apiBases.push(v); i++; }
    else if (a === '--file' && v) { out.file = v; i++; }
    else return null;
  }
  if (!out.ticket) return null;
  if (out.apiBases.length === 0) out.apiBases = ['/api/'];
  return out;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args) {
    process.stderr.write('usage: spec-quality.mjs --ticket KEY [--api-base BASE]... [--file NAME] < spec.ts\n');
    process.exit(2);
  }
  const chunks = [];
  for await (const c of process.stdin) chunks.push(c);
  const source = Buffer.concat(chunks).toString('utf8');
  const violations = checkSpec(source, { ticket: args.ticket, apiBases: args.apiBases }, args.file);
  process.stdout.write(`${JSON.stringify({ violations })}\n`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  await main();
}

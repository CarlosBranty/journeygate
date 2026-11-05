// Contract tests for the Phase 4 schemas: the in-sandbox file contract
// (agent-task.md), agents.toml (agents-toml.md) and the Engram export (memory.md).
// Run with: node --test "schemas/*.test.mjs"
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { validate } from './validate.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const read = (rel) => readFileSync(join(here, rel), 'utf8');
const load = (rel) => JSON.parse(read(rel));
const clone = (v) => structuredClone(v);

const ok = (schema, value) => assert.deepEqual(validate(schema, value), []);
const bad = (schema, value, fragment) => {
  const errors = validate(schema, value);
  assert.ok(errors.length > 0, 'expected validation errors, got none');
  if (fragment) {
    assert.ok(
      errors.some((e) => e.includes(fragment)),
      `expected an error mentioning "${fragment}", got: ${errors.join(' | ')}`,
    );
  }
};

const taskSchema = load('agent-task.schema.json');
const resultSchema = load('agent-result.schema.json');
const eventSchema = load('agent-event.schema.json');
const agentsSchema = load('agents.schema.json');
const engramSchema = load('engram-export.schema.json');
const task = load('examples/agent-task.example.json');
const result = load('examples/agent-result.example.json');
const events = read('examples/agent-events.example.jsonl')
  .split('\n')
  .filter((l) => l.trim())
  .map((l) => JSON.parse(l));
const agents = load('examples/agents.example.json');
const engram = load('examples/engram-export.example.json');

test('validator: oneOf, maxLength, maxItems, exclusiveMinimum', () => {
  const s = {
    oneOf: [
      { type: 'object', required: ['a'], properties: { a: { const: 1 } } },
      { type: 'object', required: ['b'], properties: { b: { type: 'string', maxLength: 2 } } },
    ],
  };
  ok(s, { a: 1 });
  ok(s, { b: 'xy' });
  bad(s, { b: 'xyz' }, 'oneOf');
  bad(s, { a: 1, b: 'x' }, 'oneOf');
  bad({ type: 'array', maxItems: 1 }, [1, 2], 'maxItems');
  bad({ type: 'number', exclusiveMinimum: 0 }, 0, 'exclusiveMinimum');
  ok({ type: 'number', exclusiveMinimum: 0 }, 0.01);
});

test('phase 4 schemas are draft 2020-12 with stable ids', () => {
  for (const s of [taskSchema, resultSchema, eventSchema, agentsSchema, engramSchema]) {
    assert.equal(s.$schema, 'https://json-schema.org/draft/2020-12/schema');
    assert.match(s.$id, /^https:\/\/e2e-agentdev\.dev\/schemas\/[a-z-]+\.schema\.json$/);
  }
});

// ---------------------------------------------------------------- task.json

test('task: example is valid', () => ok(taskSchema, task));

test('task: qa task has no sealed spec and a null brief is allowed', () => {
  const v = clone(task);
  v.role = 'qa';
  v.sealed_spec = null;
  v.facts_brief = null;
  v.allowed_paths = { write: ['tests/features/SHOP-101.spec.ts'], deny: [] };
  ok(taskSchema, v);
});

test('task: attempts are capped at 3', () => {
  const a = clone(task);
  a.attempt_policy.max_attempts = 4;
  bad(taskSchema, a, 'maximum');
  const b = clone(task);
  b.attempt = 0;
  bad(taskSchema, b, 'minimum');
});

test('task: the gateway key is an env var name, never a value', () => {
  const v = clone(task);
  v.gateway.key_env = 'sk-live-abc';
  bad(taskSchema, v, 'pattern');
});

test('task: facts brief is capped at 4096 characters', () => {
  const v = clone(task);
  v.facts_brief.text = 'x'.repeat(4097);
  bad(taskSchema, v, 'maxLength');
});

test('task: sealed spec sha256 is 64 lowercase hex', () => {
  const v = clone(task);
  v.sealed_spec.sha256 = 'ABC';
  bad(taskSchema, v, 'pattern');
});

test('task: role and unknown keys', () => {
  const a = clone(task);
  a.role = 'reviewer';
  bad(taskSchema, a, 'enum');
  const b = clone(task);
  b.api_key = 'x';
  bad(taskSchema, b, 'additional');
});

test('task: budget must be positive', () => {
  const v = clone(task);
  v.budget_usd = 0;
  bad(taskSchema, v, 'exclusiveMinimum');
});

// ---------------------------------------------------------------- result.json

test('result: example is valid; error and proposals are optional', () => {
  ok(resultSchema, result);
  const v = clone(result);
  delete v.memory_proposals;
  v.status = 'failed';
  v.error = 'could not reproduce';
  ok(resultSchema, v);
});

test('result: status enum and usage fields', () => {
  const a = clone(result);
  a.status = 'passed';
  bad(resultSchema, a, 'enum');
  const b = clone(result);
  delete b.usage.output_tokens;
  bad(resultSchema, b, 'output_tokens');
});

test('result: memory proposal topic keys follow the fact topic grammar', () => {
  const v = clone(result);
  v.memory_proposals[0].topic_key = 'Not A Key';
  bad(resultSchema, v, 'pattern');
});

// ---------------------------------------------------------------- events.jsonl

test('events: every example line is valid', () => {
  assert.ok(events.length >= 4);
  for (const e of events) ok(eventSchema, e);
  assert.deepEqual(
    events.map((e) => e.seq),
    events.map((_, i) => i + 1),
  );
});

test('events: a kind must carry its own fields only', () => {
  bad(eventSchema, { seq: 1, ts: '2026-11-01T00:00:00Z', kind: 'tool_call', tool: 'x' }, 'oneOf');
  bad(eventSchema, { seq: 1, ts: '2026-11-01T00:00:00Z', kind: 'message', text: 'hi', tool: 'x' }, 'oneOf');
  bad(eventSchema, { seq: 1, ts: '2026-11-01T00:00:00Z', kind: 'shell', cmd: 'rm' }, 'oneOf');
  bad(eventSchema, { seq: 0, ts: '2026-11-01T00:00:00Z', kind: 'message', text: 'hi' }, 'oneOf');
});

test('events: message text is truncated to 2000 characters', () => {
  bad(eventSchema, { seq: 1, ts: '2026-11-01T00:00:00Z', kind: 'message', text: 'x'.repeat(2001) }, 'oneOf');
});

// ---------------------------------------------------------------- agents.toml

test('agents: example is valid', () => ok(agentsSchema, agents));

test('agents: runtime enum, max_attempts cap and env names', () => {
  const a = clone(agents);
  a.agents.coder.runtime = 'aider';
  bad(agentsSchema, a, 'enum');
  const b = clone(agents);
  b.agents.coder.max_attempts = 5;
  bad(agentsSchema, b, 'maximum');
  const c = clone(agents);
  c.gateway.master_key_env = 'sk-master';
  bad(agentsSchema, c, 'pattern');
});

test('agents: [gateway], [agents] and [scheduler] are required', () => {
  for (const key of ['gateway', 'agents', 'scheduler', 'roles']) {
    const v = clone(agents);
    delete v[key];
    bad(agentsSchema, v, key);
  }
});

test('agents: best_of_n needs at least two agents', () => {
  const v = clone(agents);
  v.scheduler.best_of_n.agents = ['coder'];
  bad(agentsSchema, v, 'minItems');
});

test('agents: agent names are kebab-case and typos are rejected', () => {
  const a = clone(agents);
  a.agents.Coder_2 = clone(agents.agents.coder);
  bad(agentsSchema, a, 'property name');
  const b = clone(agents);
  b.agents.coder.budget = 5;
  bad(agentsSchema, b, 'additional');
});

// ---------------------------------------------------------------- engram export

test('engram export: example is valid and pinned to format 0.2.0', () => {
  ok(engramSchema, engram);
  const v = clone(engram);
  v.version = '0.1.0';
  bad(engramSchema, v, 'const');
});

test('engram export: observations carry sync_id, session_id and scope', () => {
  const v = clone(engram);
  delete v.observations[0].sync_id;
  bad(engramSchema, v, 'sync_id');
  const w = clone(engram);
  w.observations[0].scope = 'team';
  bad(engramSchema, w, 'enum');
});

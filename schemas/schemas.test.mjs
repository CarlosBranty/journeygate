// Contract tests for the published JSON Schemas.
// Run with: node --test "schemas/*.test.mjs"
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { validate } from './validate.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const load = (rel) => JSON.parse(readFileSync(join(here, rel), 'utf8'));
const clone = (v) => structuredClone(v);

const targetSchema = load('target.schema.json');
const journeysSchema = load('journeys.schema.json');
const targetExample = load('examples/target.example.json');
const targetMinimal = load('examples/target.minimal.json');
const journeysExample = load('examples/journeys.example.json');

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

test('validator: type, required, pattern, enum, additionalProperties', () => {
  const s = {
    type: 'object',
    required: ['a'],
    additionalProperties: false,
    properties: { a: { type: 'string', pattern: '^x+$' }, b: { enum: ['p', 'q'] } },
  };
  ok(s, { a: 'xx', b: 'p' });
  bad(s, {}, 'required');
  bad(s, { a: 'y' }, 'pattern');
  bad(s, { a: 'x', b: 'r' }, 'enum');
  bad(s, { a: 'x', c: 1 }, 'additional');
  bad(s, { a: 1 }, 'type');
});

test('validator: integer bounds, arrays, $ref, patternProperties', () => {
  const s = {
    $defs: { n: { type: 'integer', minimum: 1, maximum: 3 } },
    type: 'object',
    propertyNames: { pattern: '^[a-z]+$' },
    patternProperties: { '^[a-z]+$': { $ref: '#/$defs/n' } },
    maxProperties: 2,
  };
  ok(s, { a: 1, b: 3 });
  bad(s, { a: 0 }, 'minimum');
  bad(s, { a: 1.5 }, 'type');
  bad(s, { A: 1 }, 'property name');
  bad(s, { a: 1, b: 1, c: 1 }, 'maxProperties');
  const arr = { type: 'array', minItems: 1, uniqueItems: true, items: { type: 'string', minLength: 1 } };
  ok(arr, ['a', 'b']);
  bad(arr, [], 'minItems');
  bad(arr, ['a', 'a'], 'unique');
  bad(arr, [''], 'minLength');
});

test('schemas are draft 2020-12 with stable ids', () => {
  for (const s of [targetSchema, journeysSchema]) {
    assert.equal(s.$schema, 'https://json-schema.org/draft/2020-12/schema');
    assert.match(s.$id, /^https:\/\/e2e-agentdev\.dev\/schemas\/[a-z]+\.schema\.json$/);
  }
});

test('target: full section-18 example is valid', () => ok(targetSchema, targetExample));
test('target: minimal example is valid', () => ok(targetSchema, targetMinimal));

test('target: version must be 1', () => {
  const v = clone(targetMinimal);
  v.version = 2;
  bad(targetSchema, v, 'const');
});

test('target: [app] and [areas] are required', () => {
  const noApp = clone(targetMinimal);
  delete noApp.app;
  bad(targetSchema, noApp, 'app');
  const noAreas = clone(targetMinimal);
  delete noAreas.areas;
  bad(targetSchema, noAreas, 'areas');
});

test('target: an area needs a description and at least one path', () => {
  const v = clone(targetMinimal);
  v.areas.shop = { paths: ['src/**'] };
  bad(targetSchema, v, 'description');
  v.areas.shop = { description: 'Shop', paths: [] };
  bad(targetSchema, v, 'minItems');
});

test('target: area keys are snake_case identifiers', () => {
  const v = clone(targetMinimal);
  v.areas['Check-Out'] = { description: 'x', paths: ['a/**'] };
  bad(targetSchema, v, 'property name');
});

test('target: [app].languages accepts every impact plugin name and fallback names', () => {
  const v = clone(targetMinimal);
  v.app.languages = ['typescript', 'javascript', 'python', 'go', 'rust', 'csharp', 'php', 'java', 'kotlin'];
  ok(targetSchema, v);
  const upper = clone(targetMinimal);
  upper.app.languages = ['CSharp'];
  bad(targetSchema, upper, 'pattern');
});

test('target: [app].languages description names every impact plugin', () => {
  const description = targetSchema.properties.app.properties.languages.description;
  const plugins = description.match(/plugins: ([a-z, ]+);/)[1].split(', ');
  assert.deepEqual(plugins, ['typescript', 'javascript', 'python', 'go', 'rust', 'csharp', 'php']);
});

test('target: unknown keys are rejected (typos must not pass silently)', () => {
  const v = clone(targetMinimal);
  v.app.base_ulr = 'http://localhost:3000';
  bad(targetSchema, v, 'additional');
});

test('target: env-var fields hold names, never values', () => {
  const v = clone(targetExample);
  v.testability.token_env = 'secret-value-123';
  bad(targetSchema, v, 'pattern');
});

test('target: enums for evidence.video, code_host.kind, tracker.require_ticket', () => {
  const a = clone(targetExample);
  a.evidence.video = 'always';
  bad(targetSchema, a, 'enum');
  const b = clone(targetExample);
  b.code_host.kind = 'bitbucket';
  bad(targetSchema, b, 'enum');
  const c = clone(targetExample);
  c.tracker.require_ticket = 'ignore';
  bad(targetSchema, c, 'enum');
});

test('target: ready_path and paths in [testability] must start with "/"', () => {
  const v = clone(targetExample);
  v.app.ready_path = 'healthz';
  bad(targetSchema, v, 'pattern');
});

test('journeys: example catalog is valid', () => ok(journeysSchema, journeysExample));

test('journeys: id must match ^[a-z0-9]+(\\.[a-z0-9-]+)+$', () => {
  for (const id of ['checkout', 'Checkout.card', 'checkout.Card', 'checkout..card', '.card', 'checkout.card_x']) {
    const v = clone(journeysExample);
    v.journeys[0].id = id;
    bad(journeysSchema, v, 'pattern');
  }
  for (const id of ['checkout.customer-card', 'auth.login.sso', 'a1.b-2']) {
    const v = clone(journeysExample);
    v.journeys[0].id = id;
    ok(journeysSchema, v);
  }
});

test('journeys: every field is required', () => {
  for (const field of ['id', 'area', 'title', 'impact_question', 'paths', 'smoke', 'quarantined']) {
    const v = clone(journeysExample);
    delete v.journeys[0][field];
    bad(journeysSchema, v, field);
  }
});

test('journeys: smoke and quarantined are booleans', () => {
  const v = clone(journeysExample);
  v.journeys[0].smoke = 'true';
  bad(journeysSchema, v, 'type');
});

test('journeys: areas in the example catalog exist in the example target', () => {
  const keys = new Set(Object.keys(targetExample.areas));
  for (const j of journeysExample.journeys) assert.ok(keys.has(j.area), `unknown area ${j.area}`);
});

const thresholdsSchema = load('thresholds.schema.json');
const thresholdsExample = load('examples/thresholds.example.json');

test('thresholds: example is valid and an empty document is valid', () => {
  ok(thresholdsSchema, thresholdsExample);
  ok(thresholdsSchema, {});
});

test('thresholds: example carries the contract defaults', () => {
  assert.deepEqual(thresholdsExample, {
    version: 1,
    mode: 'shadow',
    agent_suitable_min: 0.8,
    risk_score_max: 1.0,
    money_or_security_max: 0.15,
    clear_criteria_min: 0.7,
    confidence_min: 0.75,
    existing_flow_max: 0.2,
    impact_threshold: 0.25,
  });
  for (const [key, value] of Object.entries(thresholdsExample)) {
    assert.deepEqual(thresholdsSchema.properties[key].default, value, `default of ${key}`);
  }
});

test('thresholds: probabilities must be within 0..1', () => {
  for (const key of ['agent_suitable_min', 'money_or_security_max', 'clear_criteria_min', 'confidence_min', 'existing_flow_max', 'impact_threshold']) {
    bad(thresholdsSchema, { [key]: 1.5 }, 'maximum');
    bad(thresholdsSchema, { [key]: -0.1 }, 'minimum');
    ok(thresholdsSchema, { [key]: 1 });
  }
});

test('thresholds: risk_score_max is within 0..4', () => {
  ok(thresholdsSchema, { risk_score_max: 4 });
  bad(thresholdsSchema, { risk_score_max: 4.5 }, 'maximum');
});

test('thresholds: mode is shadow or active, version is 1, unknown keys rejected', () => {
  ok(thresholdsSchema, { mode: 'active' });
  bad(thresholdsSchema, { mode: 'live' }, 'enum');
  bad(thresholdsSchema, { version: 2 }, 'const');
  bad(thresholdsSchema, { max_fan_out_for_agent: 10 }, 'max_fan_out_for_agent');
});

// D1: optional [[services]] (multi-service apps). Contract: docs/contracts/target-toml.md.
const targetServices = load('examples/target.services.json');
const withServices = (services) => ({ ...clone(targetMinimal), services });

test('target: multi-service example is valid', () => ok(targetSchema, targetServices));

test('target: [[services]] is optional and may be empty', () => {
  assert.equal('services' in targetMinimal, false);
  ok(targetSchema, withServices([]));
});

test('target: a service needs a name and a start command', () => {
  bad(targetSchema, withServices([{ start: 'bun run dev' }]), 'name');
  bad(targetSchema, withServices([{ name: 'api' }]), 'start');
  bad(targetSchema, withServices([{ name: 'api', start: '' }]), 'minLength');
  ok(targetSchema, withServices([{ name: 'api', start: 'bun run dev' }]));
});

test('target: service names match ^[a-z][a-z0-9_-]*$', () => {
  for (const name of ['api', 'web-app', 'queue_1', 'a']) {
    ok(targetSchema, withServices([{ name, start: 'x' }]));
  }
  for (const name of ['Api', '1api', '-api', 'api.v2', '']) {
    bad(targetSchema, withServices([{ name, start: 'x' }]), 'pattern');
  }
});

test('target: service cwd is a repo-relative directory', () => {
  for (const cwd of ['.', 'acme-backend', 'apps/web', 'services/api_v2']) {
    ok(targetSchema, withServices([{ name: 'api', cwd, start: 'x' }]));
  }
  for (const cwd of ['', '/abs', '../outside', 'apps/../..', 'C:/x', 'apps\\web', 'apps/', './apps']) {
    bad(targetSchema, withServices([{ name: 'api', cwd, start: 'x' }]), 'pattern');
  }
});

test('target: service ready_url is http(s) and stop is non-empty', () => {
  ok(targetSchema, withServices([{ name: 'api', start: 'x', stop: 'y', ready_url: 'http://localhost:3001/health' }]));
  bad(targetSchema, withServices([{ name: 'api', start: 'x', ready_url: 'localhost:3001' }]), 'pattern');
  bad(targetSchema, withServices([{ name: 'api', start: 'x', stop: '' }]), 'minLength');
});

test('target: unknown keys in a service are rejected', () => {
  bad(targetSchema, withServices([{ name: 'api', start: 'x', port: 3001 }]), 'additional');
});

test('target: services schema documents the defaults', () => {
  const service = targetSchema.$defs.service;
  assert.equal(service.properties.cwd.default, '.');
  assert.deepEqual(service.required, ['name', 'start']);
});

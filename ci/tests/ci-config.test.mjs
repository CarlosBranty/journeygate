// Structural checks for the CI integration files (.github/, ci/, deploy/).
// Dependency-free on purpose: plain text assertions over the YAML/Dockerfile
// sources, so the suite runs with `node --test` and nothing installed.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (rel) => readFileSync(join(repoRoot, rel), 'utf8');

const GITHUB_FILES = [
  '.github/workflows/guard.yml',
  '.github/workflows/ci.yml',
  'ci/github-action/action.yml',
];

/** Every `uses:` reference that points at a remote action. */
function remoteUses(source) {
  return [...source.matchAll(/^\s*-?\s*uses:\s*(\S+)/gm)]
    .map((m) => m[1])
    .filter((ref) => !ref.startsWith('./'));
}

test('every remote action is pinned to a full commit SHA', () => {
  for (const file of GITHUB_FILES) {
    for (const ref of remoteUses(read(file))) {
      assert.match(ref, /^[\w.-]+\/[\w.-]+(?:\/[\w.-]+)*@[0-9a-f]{40}$/, `${file}: ${ref}`);
    }
  }
});

test('artifact actions are v4 (pinned SHA annotated with a v4 tag)', () => {
  const guard = read('.github/workflows/guard.yml');
  for (const name of ['upload-artifact', 'download-artifact']) {
    const lines = guard.split('\n').filter((l) => l.includes(`actions/${name}@`));
    assert.ok(lines.length > 0, `${name} is used`);
    for (const line of lines) assert.match(line, /# v4\.\d+\.\d+$/, line);
  }
});

test('guard.yml runs plan, a 2-way shard matrix and an always() finalize', () => {
  const guard = read('.github/workflows/guard.yml');
  assert.match(guard, /guard gate plan --shards 2 --out plan\//);
  assert.match(guard, /index:\s*\[1, 2\]/);
  assert.match(guard, /fail-fast:\s*false/);
  assert.match(guard, /guard gate shard --index \$\{\{ matrix\.index \}\} --plan plan\//);
  assert.match(guard, /guard gate finalize --blobs all-blobs\//);
  assert.match(guard, /finalize:\n(?:.*\n)*?\s+if:\s*always\(\)/);
  assert.match(guard, /pattern:\s*blob-\*/);
  assert.match(guard, /merge-multiple:\s*true/);
  assert.match(guard, /uses:\s*\.\/ci\/github-action/);
  assert.match(guard, /guard up/);
});

test('ci.yml covers cargo fmt, clippy, test, pnpm tests and the demo node tests', () => {
  const ci = read('.github/workflows/ci.yml');
  assert.match(ci, /cargo fmt --all -- --check/);
  assert.match(ci, /cargo clippy --workspace --all-targets -- -D warnings/);
  assert.match(ci, /cargo test --workspace/);
  assert.match(ci, /pnpm install --frozen-lockfile/);
  // The e2e-playwright `test` script is the browser suite (needs a live app):
  // ci.yml runs only its unit, type and lint scripts.
  assert.doesNotMatch(ci, /pnpm -r (?:--if-present )?run test/);
  assert.match(ci, /pnpm --filter e2e-playwright run typecheck/);
  assert.match(ci, /pnpm --filter e2e-playwright run test:unit/);
  assert.match(ci, /pnpm --filter e2e-playwright run lint/);
  assert.match(ci, /node --test "examples\/demo-shop\/test\/\*\.test\.mjs"/);
  assert.match(ci, /node --test "ci\/tests\/\*\.test\.mjs"/);
});

test('composite action declares version and e2e-dir and puts guard on PATH', () => {
  const action = read('ci/github-action/action.yml');
  assert.match(action, /using:\s*composite/);
  assert.match(action, /^\s{2}version:/m);
  assert.match(action, /^\s{2}e2e-dir:/m);
  assert.match(action, /GITHUB_PATH/);
  assert.match(action, /cargo build --release/);
  assert.match(action, /releases\/download/);
});

test('GitLab component declares spec inputs and uses them', () => {
  const gate = read('ci/gitlab/templates/gate.yml');
  const [header, body] = gate.split(/^---$/m);
  assert.ok(body, 'spec header is separated by ---');
  assert.match(header, /spec:\s*\n\s+inputs:/);
  assert.match(header, /^\s{4}shards:/m);
  assert.match(header, /^\s{4}image:/m);
  assert.match(body, /stages:\s*\[plan, shard, finalize\]/);
  assert.match(body, /parallel:\s*\$\[\[ inputs\.shards \]\]/);
  assert.match(body, /image:\s*\$\[\[ inputs\.image \]\]/);
  assert.match(body, /--index "?\$CI_NODE_INDEX"?/);
  assert.match(body, /guard:finalize:\n(?:.*\n)*?\s+when:\s*always/);
});

test('Dockerfile is multi-stage: cargo build, then a pinned Playwright runtime', () => {
  const dockerfile = read('deploy/Dockerfile');
  const froms = [...dockerfile.matchAll(/^FROM\s+(\S+)/gm)].map((m) => m[1]);
  assert.ok(froms.length >= 2, 'at least two stages');
  assert.equal(froms[0], 'rust:${RUST_VERSION}-bookworm');
  assert.match(dockerfile, /^ARG RUST_VERSION=\d+\.\d+\.\d+$/m);
  assert.match(dockerfile, /cargo build --release/);
  assert.match(dockerfile, /^FROM mcr\.microsoft\.com\/playwright:v\$\{PLAYWRIGHT_VERSION\}-noble/m);
  assert.match(dockerfile, /^ARG PLAYWRIGHT_VERSION=\d+\.\d+\.\d+$/m);
  assert.match(dockerfile, /COPY --from=build .*\/guard \/usr\/local\/bin\/guard/);
  assert.doesNotMatch(dockerfile, /:latest\b/);
});

test('Dockerfile builds every workspace member and offers runner (default) and server targets', () => {
  const dockerfile = read('deploy/Dockerfile');
  const ignore = read('deploy/Dockerfile.dockerignore');
  const members = [...read('Cargo.toml').match(/members = \[([^\]]*)\]/)[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  assert.ok(members.length > 1);
  for (const member of members) {
    assert.match(dockerfile, new RegExp(`^COPY ${member} \\./${member}$`, 'm'), `${member} is copied`);
    assert.match(ignore, new RegExp(`^!${member}/\\*\\*$`, 'm'), `${member} is in the build context`);
  }
  assert.match(dockerfile, /cargo build --release --locked -p guard-cli --bin guard/);
  const stages = [...dockerfile.matchAll(/^FROM\s+\S+\s+AS\s+(\S+)/gm)].map((m) => m[1]);
  assert.ok(stages.includes('server'), 'a server target exists');
  assert.equal(stages.at(-1), 'runner', 'runner is the last (default) stage');
  assert.match(dockerfile, /^ENTRYPOINT \["guard"\]$/m);
  assert.match(dockerfile, /^HEALTHCHECK .*\n.*\/healthz/m);
});

test('docker-compose runs guard serve with the env names the code reads and pinned images', () => {
  const compose = read('deploy/docker-compose.yml');
  assert.match(compose, /^\s+target: server$/m);
  assert.match(compose, /command: \["serve", "--repo", "\/repo"\]/);
  for (const name of ['DATABASE_URL', 'GUARD_SERVER_TOKEN', 'GUARD_STORAGE_URL']) {
    assert.match(compose, new RegExp(`^\\s+${name}: \\S`, 'm'), `${name} is set`);
  }
  assert.match(compose, /GUARD_STORAGE_URL: s3\+http:\/\//);
  assert.doesNotMatch(compose, /GUARD_DATABASE_URL|GUARD_LLM_GATEWAY_URL/);
  assert.doesNotMatch(compose, /docker\.sock/, 'the Docker socket is only in the agents override');
  for (const [, image] of compose.matchAll(/^\s+image:\s*(\S+)/gm)) {
    if (image.endsWith(':dev')) continue; // the locally built guard image
    assert.match(image, /:[\w.-]*\d[\w.-]*$/, `${image} is pinned to an exact tag`);
    assert.doesNotMatch(image, /:latest$/);
  }
  assert.match(read('deploy/docker-compose.agents.yml'), /\/var\/run\/docker\.sock:\/var\/run\/docker\.sock/);
});

test('guard.yml exports the demo test token so login and seed calls are authorised', () => {
  // guard forwards GUARD_TEST_TOKEN only when it is set (runner-env.md); the
  // demo-shop defaults to demo-token, so both sides must agree on it.
  const guard = read('.github/workflows/guard.yml');
  assert.match(guard, /^env:\n(?:\s{2}.*\n)*?\s{2}GUARD_TEST_TOKEN:\s*demo-token\r?$/m);
});

test('the Dockerfile Playwright pin equals the e2e-playwright package pin', () => {
  const dockerfile = read('deploy/Dockerfile');
  const pkg = JSON.parse(read('e2e-playwright/package.json'));
  const pin = dockerfile.match(/^ARG PLAYWRIGHT_VERSION=(\S+)$/m)[1];
  assert.equal(pin, pkg.devDependencies['@playwright/test']);
});

test('ci.yml runs catalog lint with no runner env selection', () => {
  // Routing and smoke blocking depend on the tag invariants catalog-lint enforces.
  const ci = read('.github/workflows/ci.yml');
  const step = ci.match(/^\s+- name: e2e-playwright catalog lint\n((?:\s{8}.*\n)+)/m);
  assert.ok(step, 'a catalog lint step exists');
  assert.match(step[1], /run: pnpm --filter e2e-playwright run catalog:lint/);
  assert.doesNotMatch(step[1], /E2E_TAGS|GUARD_SHARD_PLAN/);
});

test('guard.yml takes the shard count from one place: --shards N matches the matrix 1..N', () => {
  const guard = read('.github/workflows/guard.yml');
  const n = Number(guard.match(/guard gate plan --shards (\d+) --out plan\//)[1]);
  const indexes = guard
    .match(/index:\s*\[([^\]]*)\]/)[1]
    .split(',')
    .map((s) => Number(s.trim()));
  assert.deepEqual(indexes, Array.from({ length: n }, (_, i) => i + 1));
  // An env var that nothing reads only drifts from the real count.
  assert.doesNotMatch(guard, /GUARD_SHARDS/);
});

test('guard.yml finalize refuses to decide from partial results when a shard did not succeed', () => {
  // A shard that failed (guard up, Playwright crash) uploads no blob; merging the
  // remaining blobs could report green.
  const guard = read('.github/workflows/guard.yml');
  const finalize = guard.slice(guard.indexOf('\n  finalize:'));
  const check = finalize.indexOf("if: needs.shard.result != 'success'");
  assert.ok(check > 0, 'finalize checks needs.shard.result');
  assert.match(finalize.slice(check, check + 400), /exit 1/);
  assert.ok(check < finalize.indexOf('guard gate finalize'), 'the check runs before finalize');
});

test('GitLab component documents the dind base_url host for docker compose apps', () => {
  const gate = read('ci/gitlab/templates/gate.yml');
  const [header] = gate.split(/^---$/m);
  assert.match(header, /base_url = "http:\/\/docker:<port>"/);
});

test('the release download verifies a SHA-256 checksum before extracting', () => {
  const action = read('ci/github-action/action.yml');
  const download = action.slice(action.indexOf('- name: Download guard release'));
  const body = download.slice(0, download.indexOf('\n    - name:', 1));
  assert.match(body, /\.sha256/);
  assert.match(body, /sha256sum|shasum -a 256/);
  assert.doesNotMatch(body, /curl[^\n]*\|\s*tar/, 'never pipe an unverified download into tar');
  assert.ok(body.search(/sha256sum -c|shasum -a 256 -c/) < body.indexOf('tar -xz'), 'verify, then extract');
});

test('container images are pinned to an exact version', () => {
  for (const file of ['examples/demo-shop/docker-compose.yml']) {
    for (const [, image] of read(file).matchAll(/^\s*image:\s*(\S+)/gm)) {
      assert.match(image, /:\d+\.\d+\.\d+(?:-[\w.]+)?(?:@sha256:[0-9a-f]{64})?$/, `${file}: ${image}`);
    }
  }
});

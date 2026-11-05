// Structural checks for the release pipeline: .github/workflows/release.yml
// builds the assets that ci/github-action/action.yml and the two installers in
// install/ download. All three must agree on the targets and the asset names.
// Dependency-free on purpose (plain text assertions), like ci-config.test.mjs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (rel) => readFileSync(join(repoRoot, rel), 'utf8');

const TARGETS = {
  'x86_64-unknown-linux-gnu': 'ubuntu-24.04',
  'aarch64-unknown-linux-gnu': 'ubuntu-24.04-arm',
  'x86_64-apple-darwin': 'macos-13',
  'aarch64-apple-darwin': 'macos-14',
  'x86_64-pc-windows-msvc': 'windows-latest',
};
const TARGET_RE = /\b(?:x86_64|aarch64)-(?:unknown-linux-gnu|apple-darwin|pc-windows-msvc)\b/g;
const targetsIn = (source) => [...new Set(source.match(TARGET_RE) ?? [])].sort();
const sorted = (list) => [...list].sort();

/** The `target: ... os: ...` pairs of the release build matrix. */
function matrixPairs(release) {
  return Object.fromEntries(
    [...release.matchAll(/-\s*target:\s*(\S+)\s*\n\s*os:\s*(\S+)/g)].map((m) => [m[1], m[2]]),
  );
}

test('release.yml runs on v* tags only', () => {
  const release = read('.github/workflows/release.yml');
  assert.match(release, /^on:\n\s+push:\n\s+tags:\s*\[\s*['"]v\*['"]\s*\]/m);
  assert.doesNotMatch(release, /pull_request|branches:/);
});

test('release.yml builds every target on its runner', () => {
  const release = read('.github/workflows/release.yml');
  assert.deepEqual(matrixPairs(release), TARGETS);
  assert.match(release, /cargo build -p guard-cli --bin guard --release --locked --target \$\{\{ matrix\.target \}\}/);
  assert.match(release, /fail-fast:\s*false/);
});

test('release.yml, action.yml and both installers know exactly the same targets', () => {
  const expected = sorted(Object.keys(TARGETS));
  assert.deepEqual(targetsIn(read('.github/workflows/release.yml')), expected);
  assert.deepEqual(targetsIn(read('ci/github-action/action.yml')), expected);
  assert.deepEqual(targetsIn(read('install/install.sh')), expected);
  // PowerShell is the Windows installer: it knows the one Windows target.
  assert.deepEqual(targetsIn(read('install/install.ps1')), ['x86_64-pc-windows-msvc']);
});

test('every downloader and the release use the same asset and checksum names', () => {
  const release = read('.github/workflows/release.yml');
  assert.match(release, /asset="guard-\$\{\{ matrix\.target \}\}\.tar\.gz"/);
  assert.match(release, /"\$\{asset\}\.sha256"/);
  assert.match(read('ci/github-action/action.yml'), /asset="guard-\$\{target\}\.tar\.gz"/);
  assert.match(read('install/install.sh'), /asset="guard-\$\{target\}\.tar\.gz"/);
  assert.match(read('install/install.ps1'), /\$asset = "guard-\$target\.tar\.gz"/);
  for (const file of ['ci/github-action/action.yml', 'install/install.sh', 'install/install.ps1']) {
    assert.match(read(file), /\.sha256/, `${file} downloads the checksum`);
  }
});

test('the archive holds the binary at its root and the checksum is sha256sum format', () => {
  const release = read('.github/workflows/release.yml');
  // tar -C <dir> <exe>: the binary is the only entry, at the archive root.
  assert.match(release, /tar -czf "\$\{asset\}" -C "target\/\$\{\{ matrix\.target \}\}\/release" "\$\{exe\}"/);
  // One "<hex>  <asset>" line on every runner, the format `sha256sum -c` checks.
  assert.match(release, /sha256sum "\$\{asset\}" \| cut -d' ' -f1/);
  assert.match(release, /shasum -a 256 "\$\{asset\}" \| cut -d' ' -f1/);
  assert.match(release, /printf '%s  %s\\n' "\$\{hash\}" "\$\{asset\}" > "\$\{asset\}\.sha256"/);
});

test('release.yml checks the tag against the crate version and publishes with gh', () => {
  const release = read('.github/workflows/release.yml');
  assert.match(release, /GITHUB_REF_NAME#v/);
  assert.match(release, /Cargo\.toml/);
  assert.match(release, /gh release create "\$GITHUB_REF_NAME"/);
  assert.match(release, /--verify-tag/);
  assert.match(release, /contents:\s*write/);
  assert.match(release, /GH_TOKEN:\s*\$\{\{ secrets\.GITHUB_TOKEN \}\}/);
});

test('release.yml pins every remote action to a full commit SHA', () => {
  const release = read('.github/workflows/release.yml');
  const uses = [...release.matchAll(/^\s*-?\s*uses:\s*(\S+)/gm)].map((m) => m[1]);
  assert.ok(uses.length > 0);
  for (const ref of uses) assert.match(ref, /^[\w.-]+\/[\w.-]+(?:\/[\w.-]+)*@[0-9a-f]{40}$/, ref);
});

test('both installers share defaults, auth and the install location rules', () => {
  const sh = read('install/install.sh');
  const ps = read('install/install.ps1');
  for (const [name, src] of [['install.sh', sh], ['install.ps1', ps]]) {
    assert.match(src, /GUARD_VERSION/, `${name}: GUARD_VERSION`);
    assert.match(src, /GUARD_REPO/, `${name}: GUARD_REPO`);
    assert.match(src, /CarlosBranty\/journeygate/, `${name}: default repo`);
    assert.match(src, /latest/, `${name}: default version`);
    assert.match(src, /GITHUB_TOKEN/, `${name}: GITHUB_TOKEN`);
    assert.match(src, /GH_TOKEN/, `${name}: GH_TOKEN`);
    // Private repositories: assets come from the REST API with the token.
    assert.match(src, /api\.github\.com\/repos\//, `${name}: REST API`);
    assert.match(src, /application\/octet-stream/, `${name}: asset download`);
    assert.match(src, /releases\/latest/, `${name}: latest release`);
    assert.match(src, /releases\/tags\/v/, `${name}: tagged release`);
    assert.match(src, /PATH/, `${name}: PATH hint`);
  }
  assert.match(sh, /\$HOME\/\.local\/bin/);
  assert.match(sh, /GUARD_INSTALL_DIR/);
  assert.match(ps, /LOCALAPPDATA/);
  assert.match(ps, /guard\\bin/);
  assert.match(ps, /GUARD_INSTALL_DIR/);
});

test('both installers verify the checksum before extracting', () => {
  const sh = read('install/install.sh');
  assert.match(sh, /^set -eu/m);
  assert.doesNotMatch(sh, /curl[^\n]*\|\s*(?:tar|sh)/, 'never pipe a download into tar or sh');
  const verifySh = sh.search(/sha256sum|shasum -a 256/);
  assert.ok(verifySh > 0 && verifySh < sh.indexOf('tar -xzf'), 'install.sh: verify, then extract');

  const ps = read('install/install.ps1');
  assert.match(ps, /\$ErrorActionPreference = 'Stop'/);
  const verifyPs = ps.indexOf('Get-FileHash');
  assert.ok(verifyPs > 0 && verifyPs < ps.indexOf('tar -xzf'), 'install.ps1: verify, then extract');
});

test('the token is sent as a header, never put in a URL or printed', () => {
  for (const file of ['install/install.sh', 'install/install.ps1']) {
    const src = read(file);
    assert.match(src, /Authorization/, `${file} sends Authorization`);
    assert.doesNotMatch(src, /https:\/\/[^\s"']*\$\{?(?:token|GITHUB_TOKEN|GH_TOKEN)/i, `${file}: token in a URL`);
    assert.doesNotMatch(src, /(?:echo|printf|Write-Host|Write-Output)[^\n]*\$\{?(?:token|env:GITHUB_TOKEN|env:GH_TOKEN|GITHUB_TOKEN|GH_TOKEN)\b/i, `${file}: token printed`);
  }
});

test('install.sh keeps the token out of argv and off redirected hosts', () => {
  const sh = read('install/install.sh');
  // A -H "Authorization: ..." argument is visible to every local user (ps, /proc).
  assert.doesNotMatch(sh, /-H\s+["']?Authorization/, 'no Authorization header in argv');
  assert.match(sh, /-H\s+@-/, 'the Authorization header is read from stdin');
  // The authenticated request never follows redirects itself (curl < 7.58
  // re-sent custom headers to the storage host); the redirect target is
  // fetched without the token.
  const auth = sh.slice(sh.indexOf('fetch() {'), sh.indexOf('\n}\n', sh.indexOf('fetch() {')));
  const authCurl = auth.slice(0, auth.indexOf('Authorization'));
  assert.doesNotMatch(authCurl.slice(authCurl.lastIndexOf('curl')), /--location|-L\b/, 'authenticated curl does not follow redirects');
  assert.match(auth, /%\{redirect_url\}/, 'the redirect target is read and fetched without the token');
});

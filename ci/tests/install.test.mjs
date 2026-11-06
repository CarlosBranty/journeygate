// Checks for the one-line installers in install/ and their Pages workflow:
// static rules on install.sh, install.ps1 and .github/workflows/pages.yml, and
// runtime runs of install.sh (dry-run, offline asset) in throwaway directories.
// Dependency-free on purpose, like release.test.mjs. No test touches the
// network: every run gets a dead proxy, and offline installs use a local
// archive built here.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, statSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (rel) => readFileSync(join(repoRoot, rel), 'utf8');
const installSh = join(repoRoot, 'install', 'install.sh');
const installPs1 = join(repoRoot, 'install', 'install.ps1');

const hasSh = process.platform !== 'win32' && spawnSync('sh', ['-c', 'exit 0']).status === 0;
const skipSh = hasSh ? false : 'needs a POSIX sh (not run on Windows)';
const TARGET = 'x86_64-unknown-linux-gnu';
const ASSET = `guard-${TARGET}.tar.gz`;

/** A fresh sandbox: HOME, install dir, empty Playwright browser dir. */
function sandbox() {
  const root = mkdtempSync(join(tmpdir(), 'guard-install-test-'));
  const dirs = { root, home: join(root, 'home'), install: join(root, 'bin'), browsers: join(root, 'browsers'), stubs: join(root, 'stubs') };
  for (const d of Object.values(dirs)) mkdirSync(d, { recursive: true });
  return dirs;
}

/** The environment of one installer run: no token, no inherited GUARD_*, a dead proxy. */
function envFor(box, extra = {}) {
  const env = { ...process.env };
  for (const key of Object.keys(env)) {
    if (/^GUARD_/.test(key) || ['GITHUB_TOKEN', 'GH_TOKEN', 'NO_COLOR', 'PLAYWRIGHT_BROWSERS_PATH'].includes(key)) delete env[key];
  }
  return {
    ...env,
    HOME: box.home,
    XDG_CACHE_HOME: join(box.home, '.cache'),
    GUARD_INSTALL_DIR: box.install,
    PLAYWRIGHT_BROWSERS_PATH: box.browsers,
    HTTPS_PROXY: 'http://127.0.0.1:9',
    HTTP_PROXY: 'http://127.0.0.1:9',
    https_proxy: 'http://127.0.0.1:9',
    http_proxy: 'http://127.0.0.1:9',
    ...extra,
  };
}

function runSh(box, args, extra = {}) {
  const r = spawnSync('sh', [installSh, ...args], { env: envFor(box, extra), encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'], timeout: 120000 });
  return { status: r.status, out: `${r.stdout}${r.stderr}`, stdout: r.stdout };
}

/** guard-<target>.tar.gz holding a sh `guard` that prints a fake version. */
function fixture(box) {
  const dir = join(box.root, 'fixture');
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, 'guard'), '#!/bin/sh\necho "guard 0.0.0-test"\n');
  chmodSync(join(dir, 'guard'), 0o755);
  const archive = join(box.root, ASSET);
  const tar = spawnSync('tar', ['-czf', archive, '-C', dir, 'guard']);
  assert.equal(tar.status, 0, String(tar.stderr));
  const sha = createHash('sha256').update(readFileSync(archive)).digest('hex');
  return { archive, sha };
}

function stub(box, name, body) {
  const file = join(box.stubs, name);
  writeFileSync(file, `#!/bin/sh\n${body}\n`);
  chmodSync(file, 0o755);
}

const linux = { GUARD_OS: 'Linux', GUARD_ARCH: 'x86_64' };
const withStubs = (box) => `${box.stubs}${delimiter}${process.env.PATH}`;

test('install.sh --help lists the options', { skip: skipSh }, () => {
  const box = sandbox();
  const r = runSh(box, ['--help']);
  assert.equal(r.status, 0, r.out);
  for (const flag of ['--profile', '--yes', '--dry-run', '--from-source', '--allow-sudo']) assert.match(r.out, new RegExp(flag));
});

test('install.sh --dry-run prints the plan and writes nothing', { skip: skipSh }, () => {
  const box = sandbox();
  const r = runSh(box, ['--dry-run', '--profile', 'cli'], linux);
  assert.equal(r.status, 0, r.out);
  assert.match(r.out, /dry run/);
  assert.match(r.out, new RegExp(TARGET));
  assert.deepEqual(readdirSync(box.install), []);
});

test('install.sh installs an offline asset with GUARD_ASSET_SHA256', { skip: skipSh }, () => {
  const box = sandbox();
  const { archive, sha } = fixture(box);
  const r = runSh(box, [], { ...linux, GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: sha });
  assert.equal(r.status, 0, r.out);
  const bin = join(box.install, 'guard');
  assert.ok(existsSync(bin), r.out);
  assert.ok(statSync(bin).mode & 0o111, 'guard is executable');
  assert.match(r.out, /guard 0\.0\.0-test/);
  assert.match(r.out, /guard init/);
});

test('install.sh refuses an offline asset with the wrong checksum', { skip: skipSh }, () => {
  const box = sandbox();
  const { archive } = fixture(box);
  const r = runSh(box, [], { ...linux, GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: '0'.repeat(64) });
  assert.notEqual(r.status, 0, r.out);
  assert.match(r.out, /checksum mismatch/);
  assert.deepEqual(readdirSync(box.install), []);
});

test('install.sh takes the checksum from the .sha256 next to the asset', { skip: skipSh }, () => {
  const box = sandbox();
  const { archive, sha } = fixture(box);
  writeFileSync(`${archive}.sha256`, `${sha}  ${ASSET}\n`);
  const r = runSh(box, [], { ...linux, GUARD_ASSET_FILE: archive });
  assert.equal(r.status, 0, r.out);
  assert.ok(existsSync(join(box.install, 'guard')));
});

test('install.sh prints no color escapes when stdout is not a terminal', { skip: skipSh }, () => {
  const box = sandbox();
  const { archive, sha } = fixture(box);
  for (const extra of [{}, { NO_COLOR: '1' }]) {
    const dry = runSh(box, ['--dry-run'], { ...linux, ...extra });
    assert.equal(dry.status, 0, dry.out);
    assert.ok(!dry.out.includes('\x1b['), 'dry run output has no escapes');
    const real = runSh(sandbox(), [], { ...linux, ...extra, GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: sha });
    assert.equal(real.status, 0, real.out);
    assert.ok(!real.out.includes('\x1b['), 'install output has no escapes');
  }
});

test('install.sh --profile qa checks the Node.js major version', { skip: skipSh }, () => {
  const box = sandbox();
  stub(box, 'node', 'echo v20.11.0');
  const env = { ...linux, PATH: withStubs(box), GUARD_OS_RELEASE: join(box.root, 'no-os-release') };
  const old = runSh(box, ['--profile', 'qa', '--dry-run'], env);
  assert.equal(old.status, 0, old.out);
  assert.match(old.out, /needs >= 24/);
  assert.match(old.out, /^\s+plan\s/m);
  stub(box, 'node', 'echo v24.1.0');
  const current = runSh(box, ['--profile', 'qa', '--dry-run'], env);
  assert.equal(current.status, 0, current.out);
  assert.match(current.out, /ok\s+node 24\.1\.0/);
});

/**
 * A Debian host with Node.js 20: stubs for sudo, apt-get, apt-cache, node,
 * npm and npx. Each stub that would change the system appends its argv (and
 * whether it saw a token) to `log`, and prints to stdout like the real one.
 */
function debianHost(box) {
  const log = join(box.root, 'commands.log');
  const osRelease = join(box.root, 'os-release');
  writeFileSync(osRelease, 'ID=ubuntu\nID_LIKE=debian\n');
  const seen = 'token=${GITHUB_TOKEN:-unset}${GH_TOKEN:+,gh}';
  stub(box, 'sudo', `echo "sudo $* ${seen}" >>'${log}'\nexec "$@"`);
  stub(box, 'apt-get', `echo "apt-get $* ${seen}" >>'${log}'\necho "Reading package lists... Done"`);
  stub(box, 'apt-cache', 'echo "  Candidate: 24.1.0-1"');
  stub(box, 'node', 'echo v20.11.0');
  // `npm prefix -g` names a directory nobody can write, so pnpm is not offered.
  stub(box, 'npm', 'case "$1" in prefix) echo /nonexistent ;; *) echo 10.9.0 ;; esac');
  stub(box, 'npx', `echo "npx $* ${seen}" >>'${log}'\necho "Downloading Chromium 140.0"`);
  return { log, osRelease, lines: () => (existsSync(log) ? readFileSync(log, 'utf8').split('\n').filter(Boolean) : []) };
}

const needsNonRoot = process.getuid?.() === 0 ? 'root needs no sudo' : false;

test('install.sh --yes never runs sudo without --allow-sudo', { skip: skipSh || needsNonRoot }, () => {
  const box = sandbox();
  const { archive, sha } = fixture(box);
  const host = debianHost(box);
  const r = runSh(box, ['--profile', 'qa', '--yes'], { ...linux, PATH: withStubs(box), GUARD_OS_RELEASE: host.osRelease, GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: sha });
  assert.equal(r.status, 0, r.out);
  const ran = host.lines();
  assert.deepEqual(ran.filter((l) => /^(sudo|apt-get) /.test(l)), [], `no sudo, no apt-get:\n${ran.join('\n')}`);
  assert.match(r.out, /skipped: needs sudo; rerun with --allow-sudo/);
  assert.match(r.out, /apt package lists: sudo apt-get update/);
  assert.match(r.out, /Node\.js: sudo apt-get install -y nodejs/);
  // A command that needs no sudo still runs under --yes, and its stdout does
  // not turn a success into a failure.
  assert.ok(ran.some((l) => /^npx -y playwright@1\.64\.1 install chromium /.test(l)), ran.join('\n'));
  assert.doesNotMatch(r.out, /installing Playwright Chromium failed/);
});

test('install.sh --yes --allow-sudo refreshes apt once, then installs; no token reaches a command', { skip: skipSh || needsNonRoot }, () => {
  const box = sandbox();
  const { archive, sha } = fixture(box);
  const host = debianHost(box);
  const secret = 'test-secret-token-value';
  const r = runSh(box, ['--profile', 'qa', '--yes', '--allow-sudo'], {
    ...linux, PATH: withStubs(box), GUARD_OS_RELEASE: host.osRelease, GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: sha,
    GITHUB_TOKEN: secret, GH_TOKEN: secret,
  });
  assert.equal(r.status, 0, r.out);
  const ran = host.lines();
  const apt = ran.filter((l) => l.startsWith('apt-get '));
  assert.equal(apt.filter((l) => l.startsWith('apt-get update ')).length, 1, ran.join('\n'));
  assert.ok(apt[0].startsWith('apt-get update '), 'apt-get update runs before the first install');
  assert.ok(apt.some((l) => l.startsWith('apt-get install -y nodejs ')), ran.join('\n'));
  assert.ok(ran.some((l) => l.startsWith('sudo apt-get install -y nodejs ')), ran.join('\n'));
  // Every stub printed to stdout and exited 0: all are successes.
  assert.doesNotMatch(r.out, /installing .* failed/);
  for (const line of ran) assert.match(line, /token=unset$/, `the token reached a command: ${line.replace(secret, '<token>')}`);
  assert.ok(!r.out.includes(secret), 'the token is never printed');
});

test('install.sh offers npm when node is current but npm is missing', { skip: skipSh || (existsSync('/usr/bin/npm') ? 'a system npm in /usr/bin' : false) }, () => {
  const box = sandbox();
  const host = debianHost(box);
  stub(box, 'node', 'echo v24.1.0');
  for (const name of ['npm', 'npx']) unlinkSync(join(box.stubs, name));
  // Only the stubs and the base system: no npm from a Node.js install.
  const r = runSh(box, ['--profile', 'qa', '--dry-run'], { ...linux, PATH: `${box.stubs}${delimiter}/usr/bin${delimiter}/bin`, GUARD_OS_RELEASE: host.osRelease });
  assert.equal(r.status, 0, r.out);
  assert.match(r.out, /npm: missing \(required/);
  assert.match(r.out, /apt-get install -y npm/);
});

test('install.sh replaces an existing install by rename', { skip: skipSh }, () => {
  const box = sandbox();
  const { archive, sha } = fixture(box);
  const env = { ...linux, GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: sha };
  const first = runSh(box, [], env);
  assert.equal(first.status, 0, first.out);
  const bin = join(box.install, 'guard');
  const before = statSync(bin).ino;
  const second = runSh(box, [], env);
  assert.equal(second.status, 0, second.out);
  assert.match(second.out, /guard 0\.0\.0-test/);
  assert.deepEqual(readdirSync(box.install), ['guard'], 'no staged .guard.new is left behind');
  // A new inode: a running guard keeps its old file instead of seeing it rewritten.
  if (before) assert.notEqual(statSync(bin).ino, before);
});

test('install.sh exits on Ctrl-C instead of carrying on', () => {
  const sh = read('install/install.sh');
  assert.match(sh, /^\s*trap '[^']*' EXIT$/m);
  assert.match(sh, /^\s*trap '[^']*exit 130' INT$/m);
  assert.match(sh, /^\s*trap '[^']*exit 143' TERM$/m);
});

test('install.sh on a platform without a prebuilt binary points at --from-source', { skip: skipSh }, () => {
  const box = sandbox();
  const r = runSh(box, [], { GUARD_OS: 'FreeBSD', GUARD_ARCH: 'x86_64' });
  assert.equal(r.status, 1, r.out);
  assert.match(r.out, /--from-source/);
  assert.match(r.out, /rustup/);
});

test('install.sh --from-source --dry-run plans cargo and rustup, never a pipe to sh', { skip: skipSh }, () => {
  const box = sandbox();
  // Keep Rust off PATH so the plan includes rustup.
  const path = process.env.PATH.split(delimiter).filter((p) => !/\.cargo|rustup/.test(p)).join(delimiter);
  const cargoLeft = spawnSync('sh', ['-c', 'command -v cargo'], { env: { ...process.env, PATH: path } }).status === 0;
  const r = runSh(box, ['--from-source', '--dry-run'], { GUARD_OS: 'FreeBSD', GUARD_ARCH: 'x86_64', PATH: path });
  assert.equal(r.status, 0, r.out);
  assert.match(r.out, /cargo install --locked/);
  if (!cargoLeft) assert.match(r.out, /rustup-init/);
  assert.doesNotMatch(r.out, /\|\s*sh\b/);
  assert.deepEqual(readdirSync(box.install), []);
});

test('install.sh is safe to pipe into sh', () => {
  const sh = read('install/install.sh');
  const lines = sh.split('\n').filter((l) => l.trim() !== '');
  assert.equal(lines.at(-1), 'main "$@"', 'the script runs only after it is fully parsed');
  for (const line of sh.split('\n').filter((l) => /\bread\b/.test(l))) {
    assert.match(line, /<\/dev\/tty/, `answers come from the terminal, not the pipe: ${line}`);
  }
  assert.doesNotMatch(sh, /curl[^\n]*\|\s*(?:ba)?sh/);
  assert.equal(sh.split('fetch() {').length - 1, 1, 'one fetch() {');
  assert.doesNotMatch(sh, /github\.io/, 'the Pages host lives in the docs only');
});

test('install.ps1 works under irm | iex', () => {
  const ps = read('install/install.ps1');
  assert.doesNotMatch(ps, /^\s*param\(/im, 'iex passes no parameters');
  assert.match(ps, /function Fail[^}]*throw/, 'Fail throws instead of exit');
  for (const s of ['GUARD_PROFILE', 'GUARD_YES', 'GUARD_DRY_RUN', 'GUARD_ASSET_FILE', 'NO_COLOR', 'OpenJS.NodeJS.LTS', 'Rustlang.Rustup']) {
    assert.ok(ps.includes(s), `install.ps1 mentions ${s}`);
  }
  assert.doesNotMatch(ps, /github\.io/, 'the Pages host lives in the docs only');
});

const powershell = process.platform === 'win32'
  ? 'powershell'
  : (spawnSync('pwsh', ['-NoProfile', '-Command', 'exit 0']).status === 0 ? 'pwsh' : null);

test('install.ps1 under iex never closes the host session', { skip: powershell ? false : 'needs PowerShell' }, () => {
  const box = sandbox();
  const quoted = installPs1.replace(/'/g, "''");
  const command = `Get-Content -Raw '${quoted}' | iex; Write-Host HOST-ALIVE`;
  const base = { GUARD_DRY_RUN: '1', ...(process.platform === 'win32' ? {} : { GUARD_OS: 'Linux' }) };
  const run = (extra) => {
    const r = spawnSync(powershell, ['-NoProfile', '-NonInteractive', '-Command', command], { env: envFor(box, { ...base, ...extra }), encoding: 'utf8', timeout: 120000 });
    return { status: r.status, out: `${r.stdout}${r.stderr}` };
  };
  const ok = run({});
  assert.equal(ok.status, 0, ok.out);
  assert.match(ok.out, /Summary/);
  assert.match(ok.out, /HOST-ALIVE/);
  // A failure is reported, the session that ran iex keeps going, and a batch
  // host (-Command, as CI runs it) still exits non-zero.
  const failed = run({ GUARD_ASSET_FILE: join(box.root, 'missing.tar.gz') });
  assert.match(failed.out, /error: GUARD_ASSET_FILE does not exist/);
  assert.match(failed.out, /HOST-ALIVE/);
  assert.notEqual(failed.status, 0, failed.out);
  assert.deepEqual(readdirSync(box.install), []);
});

test('install.ps1 replaces a running guard.exe', { skip: process.platform === 'win32' ? false : 'needs Windows' }, () => {
  const box = sandbox();
  // A copy of cmd.exe stands in for guard.exe: `guard.exe --version` exits 0
  // (its stdin is empty), and with an open stdin it keeps running while the
  // second install replaces it.
  const dir = join(box.root, 'fixture');
  mkdirSync(dir, { recursive: true });
  copyFileSync(join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'cmd.exe'), join(dir, 'guard.exe'));
  const asset = 'guard-x86_64-pc-windows-msvc.tar.gz';
  const tar = spawnSync(join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe'), ['-czf', join('..', asset), 'guard.exe'], { cwd: dir });
  assert.equal(tar.status, 0, String(tar.stderr));
  const archive = join(box.root, asset);
  const sha = createHash('sha256').update(readFileSync(archive)).digest('hex');
  const run = () => {
    const r = spawnSync('powershell', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', installPs1], {
      env: envFor(box, { GUARD_ASSET_FILE: archive, GUARD_ASSET_SHA256: sha, GUARD_PROFILE: 'cli', GUARD_ARCH: 'AMD64' }), encoding: 'utf8', timeout: 120000,
    });
    return { status: r.status, out: `${r.stdout}${r.stderr}` };
  };
  const first = run();
  assert.equal(first.status, 0, first.out);
  const installed = join(box.install, 'guard.exe');
  const running = spawn(installed, [], { stdio: ['pipe', 'ignore', 'ignore'] });
  try {
    const second = run();
    assert.equal(second.status, 0, second.out);
    assert.match(second.out, /installed /);
    assert.ok(existsSync(installed));
    assert.ok(!existsSync(`${installed}.new`), 'no staged guard.exe.new is left behind');
  } finally {
    running.kill();
  }
});

test('pages.yml publishes both installers from main with pinned actions', () => {
  const pages = read('.github/workflows/pages.yml');
  assert.match(pages, /^on:\n\s+push:\n\s+branches:\s*\[\s*main\s*\]\n\s+paths:\s*\[[^\]]*['"]install\/\*\*['"]/m);
  assert.match(pages, /workflow_dispatch/);
  assert.match(pages, /pages:\s*write/);
  assert.match(pages, /id-token:\s*write/);
  const uses = [...pages.matchAll(/^\s*-?\s*uses:\s*(\S+)/gm)].map((m) => m[1]);
  assert.ok(uses.length >= 4);
  for (const ref of uses) assert.match(ref, /^[\w.-]+\/[\w.-]+(?:\/[\w.-]+)*@[0-9a-f]{40}$/, ref);
  assert.match(pages, /cp install\/install\.sh install\/install\.ps1 site\//);
  assert.doesNotMatch(pages, /secrets\./);
});

test('README and getting-started show the one-line installers', () => {
  for (const file of ['README.md', 'docs/getting-started.md']) {
    const doc = read(file);
    assert.ok(doc.includes('carlosbranty.github.io/journeygate/install.sh'), `${file}: install.sh short URL`);
    assert.ok(doc.includes('install.ps1 | iex'), `${file}: install.ps1 under iex`);
  }
});

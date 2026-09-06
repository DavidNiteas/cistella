#!/usr/bin/env node
// Release packaging pipeline for cistella. This is ONE of two independent
// pipelines — do not merge them:
//
//   A. Hand-test (hot frontend):  pnpm build:bin-test  -> self-contained bin_test/
//      Frontend refreshes by copying dist/ over bin_test/cistella-frontend/,
//      no cargo rebuild. Smoke: pnpm test:headless.
//   B. Release (embedded):        pnpm package         -> this script
//      Frontend is embedded into the binary; artifacts go to release/<product>-<version>/.
//
// The two pipelines produce DIFFERENT executables (bin-test-frontend feature on
// vs off) and are chosen by the user per need; this script never builds bin_test.
//
// Stages:
//   1. build:release  -> embedded release binaries + portable zip (target/release/)
//   2. collect        -> copy artifacts into release/<product>-<version>/
//                        with SHA256SUMS.txt and manifest.json
//
// Toolchain note: this project ships no Python. Orchestration is plain Node.js
// (ESM); archiving goes through PowerShell + .NET ZipFile and hashing through
// certutil, both shipped with Windows — zero extra installs required.
//
// Usage:
//   pnpm package            # release pipeline
//   pnpm package --full     # additionally run release-smoke-test.js (all cargo tests)
import { execFileSync, execSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const crateDir = resolve(__dirname, '..');
const workspaceRoot = resolve(crateDir, '..', '..');
const tauriConf = JSON.parse(readFileSync(join(crateDir, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const productName = tauriConf.productName || 'cistella';
const version = tauriConf.version || '0.1.0';
const full = process.argv.includes('--full');

function stage(name) {
  console.log(`\n=== [package] ${name} ===`);
}

function run(command) {
  execSync(command, { cwd: crateDir, stdio: 'inherit', shell: true });
}

function gitCommit() {
  try {
    return execSync('git rev-parse --short HEAD', { cwd: workspaceRoot, encoding: 'utf8' }).trim();
  } catch {
    return null;
  }
}

function sha256(file) {
  const out = execFileSync('certutil', ['-hashfile', file, 'SHA256'], { encoding: 'utf8' });
  // certutil prints: "SHA256 hash of <file>:" / "<hex pairs with spaces>" / "CertUtil: -hashfile command completed successfully."
  const hex = out.split('\n')[1].replace(/[^0-9A-Fa-f]/g, '').toLowerCase();
  return hex;
}

stage(`1/2 build embedded release (${productName} ${version})`);
run('pnpm build:release');

if (full) {
  stage('1b/2 full release regression (cargo tests)');
  run('node scripts/release-smoke-test.js');
}

stage('2/2 collect artifacts');
const releaseDir = join(workspaceRoot, 'target', 'release');
const portableZip = join(releaseDir, `${productName}-${version}-portable.zip`);
const guiExe = join(releaseDir, `${productName}.exe`);
const headlessExe = join(releaseDir, `${productName}-headless.exe`);
for (const artifact of [portableZip, guiExe, headlessExe]) {
  if (!existsSync(artifact)) {
    throw new Error(`Expected artifact was not produced: ${artifact}`);
  }
}

const outDir = join(workspaceRoot, 'release', `${productName}-${version}`);
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

cpSync(portableZip, join(outDir, basename(portableZip)));
cpSync(guiExe, join(outDir, basename(guiExe)));
cpSync(headlessExe, join(outDir, basename(headlessExe)));

const artifacts = [basename(portableZip), basename(guiExe), basename(headlessExe)].map((file) => {
  const path = join(outDir, file);
  return { file, bytes: statSync(path).size, sha256: sha256(path) };
});

writeFileSync(
  join(outDir, 'SHA256SUMS.txt'),
  artifacts.map((a) => `${a.sha256}  ${a.file}`).join('\r\n') + '\r\n',
  'utf8'
);

writeFileSync(
  join(outDir, 'manifest.json'),
  JSON.stringify(
    {
      product: productName,
      version,
      channel: 'release',
      builtAt: new Date().toISOString(),
      gitCommit: gitCommit(),
      artifacts,
    },
    null,
    2
  ) + '\n',
  'utf8'
);

console.log(`\n[package] artifacts collected in ${outDir}`);
for (const a of artifacts) {
  console.log(`  ${a.file} (${(a.bytes / 1024 / 1024).toFixed(1)} MiB)`);
}
console.log('[package] PACKAGE OK');

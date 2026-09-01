#!/usr/bin/env node
// Final release build: GUI embeds frontend assets in the Tauri binary.
import { execSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const crateDir = resolve(__dirname, '..');
const workspaceRoot = resolve(crateDir, '..', '..');
const tauriConf = JSON.parse(readFileSync(join(crateDir, 'src-tauri', 'tauri.conf.json'), 'utf8'));
const productName = tauriConf.productName || 'cistella';
const releaseDir = join(workspaceRoot, 'target', 'release');
const guiExePath = join(releaseDir, `${productName}.exe`);
const headlessExePath = join(releaseDir, `${productName}-headless.exe`);

function run(command) {
  execSync(command, { cwd: crateDir, stdio: 'inherit', shell: true });
}

// No bin-test feature here: this is the embedded frontend release binary.
run('pnpm exec tauri build --no-bundle');
run('cargo build --release --bin cistella-headless');

for (const artifact of [guiExePath, headlessExePath]) {
  if (!existsSync(artifact)) {
    throw new Error(`Expected release artifact was not produced: ${artifact}`);
  }
}

run('node scripts/make-portable.js');
console.log(`Release GUI: ${guiExePath}`);
console.log(`Release headless: ${headlessExePath}`);

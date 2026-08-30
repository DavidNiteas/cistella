#!/usr/bin/env node
// M3 regression / release smoke test for cistella.
// This script does NOT run `tauri build` (it is too slow), but it checks that
// release artifacts already exist after a previous build.
import { execSync } from 'node:child_process';
import { existsSync, globSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(__dirname, '../../..');

let failed = false;
let currentStep = '';

function step(name) {
  currentStep = name;
  console.log(`\n[STEP] ${name}`);
}

function pass() {
  console.log(`  PASS: ${currentStep}`);
}

function fail(message) {
  failed = true;
  console.error(`  FAIL: ${currentStep}${message ? ` — ${message}` : ''}`);
}

function run(command, options = {}) {
  try {
    execSync(command, { stdio: 'inherit', cwd: workspaceRoot, ...options });
    return true;
  } catch (error) {
    failed = true;
    console.error(`  FAIL: ${currentStep} — command failed: ${command}`);
    if (error.stderr) console.error(error.stderr.toString());
    return false;
  }
}

function checkExists(pattern, description) {
  const matches = globSync(pattern, { cwd: workspaceRoot });
  if (matches.length > 0) {
    console.log(`  Found ${description}: ${matches.join(', ')}`);
    return true;
  }
  failed = true;
  console.error(`  FAIL: ${currentStep} — no ${description} found at ${pattern}`);
  return false;
}

step('cargo fmt --check');
if (run('cargo fmt --check')) pass();

step('cargo test -p cistella-core --lib');
if (run('cargo test -p cistella-core --lib')) pass();

step('cargo test -p cistella-core --test "工单14_m1_*"');
if (run('cargo test -p cistella-core --test "工单14_m1_*"')) pass();

step('cargo test -p cistella-core --test "工单14_m2_*"');
if (run('cargo test -p cistella-core --test "工单14_m2_*"')) pass();

step('cargo test -p cistella-core --test "工单14_m3_*"');
if (run('cargo test -p cistella-core --test "工单14_m3_*"')) pass();

step('cargo test -p cistella-core');
if (run('cargo test -p cistella-core')) pass();

step('cargo test -p cistella-desktop');
if (run('cargo test -p cistella-desktop')) pass();

step('cargo check -p cistella-desktop');
if (run('cargo check -p cistella-desktop')) pass();

step('pnpm build');
if (run('pnpm build')) pass();

step('artifact existence check');
checkExists('target/release/bundle/msi/*.msi', 'MSI installer');
checkExists('target/release/bundle/nsis/*-setup.exe', 'NSIS setup executable');
checkExists('target/release/cistella-*.zip', 'portable zip');
if (!failed) pass();

console.log(`\n${failed ? 'SMOKE TEST FAILED' : 'SMOKE TEST PASSED'}`);
process.exit(failed ? 1 : 0);

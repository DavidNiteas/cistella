#!/usr/bin/env node
// Work order 16 / M3: smoke-test the headless entrypoint without requiring a GUI.
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(__dirname, '../../..');
// Run against the self-contained bin_test bundle instead of target/ so the
// smoke test does not depend on a debug build of the workspace.
const exe = process.platform === 'win32'
  ? join(workspaceRoot, 'bin_test', 'cistella-headless.exe')
  : join(workspaceRoot, 'bin_test', 'cistella-headless');
const tmpVault = join(workspaceRoot, 'target', 'headless-smoke-vault');

function run(args, options = {}) {
  const output = execFileSync(exe, args, {
    cwd: workspaceRoot,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    ...options,
  });
  console.log(`$ ${exe} ${args.join(' ')}`);
  console.log(output.trim());
  return output;
}

function json(args) {
  return JSON.parse(run(args));
}

if (!existsSync(exe)) {
  console.error(`Missing headless binary: ${exe}`);
  console.error('Run `pnpm build:bin-test` first.');
  process.exit(1);
}

rmSync(tmpVault, { recursive: true, force: true });
mkdirSync(tmpVault, { recursive: true });
writeFileSync(join(tmpVault, 'manifest.json'), JSON.stringify({
  format_version: '0.1.0',
  vault_id: 'headless-smoke',
  logical_schema_version: '0.1.0',
  created_at: '2026-09-01T00:00:00Z',
  source: {
    name: 'headless-smoke',
    entity: 'sources',
    snapshot_date: null,
    input_path: tmpVault,
  },
  tables: {},
}, null, 2));

json(['doctor', '--json']);
json(['vault', 'open', '--path', tmpVault]);
json(['literature', 'list', '--vault', tmpVault, '--limit', '5']);
json(['reading', 'sessions', '--vault', tmpVault]);
json(['notes', 'list', '--vault', tmpVault]);
json(['settings', 'info']);
json(['search', '--vault', tmpVault, '--query', 'test']);

let sourceFailedAsExpected = false;
try {
  json(['source', 'rank', '--vault', tmpVault, '--metric', 'h-index', '--type', 'journal', '--limit', '5']);
} catch (error) {
  sourceFailedAsExpected = true;
  const stderr = error.stderr?.toString() ?? '';
  console.log('source rank skipped as expected for smoke vault without sources table');
  if (!stderr.includes('sources') && !stderr.includes('table')) {
    throw error;
  }
}
if (!sourceFailedAsExpected) {
  console.log('source rank passed');
}

console.log('HEADLESS SMOKE TEST PASSED');

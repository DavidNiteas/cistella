#!/usr/bin/env node
// Post-build script for work order 14 / M2: assemble a portable zip bundle
// after `cargo tauri build` has produced the release executable.
import { execSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const workspaceRoot = resolve(__dirname, '../../..');
const crateDir = resolve(__dirname, '..');
const tauriConfPath = join(crateDir, 'src-tauri', 'tauri.conf.json');
const tauriConf = JSON.parse(readFileSync(tauriConfPath, 'utf8'));
const version = tauriConf.version || '0.1.0';
const productName = tauriConf.productName || 'cistella';

// M3: warn if the version cannot be determined or drifts from package.json.
const packageJsonPath = join(crateDir, 'package.json');
const packageVersion = existsSync(packageJsonPath)
  ? JSON.parse(readFileSync(packageJsonPath, 'utf8')).version
  : undefined;
if (!tauriConf.version) {
  console.warn('WARNING: tauri.conf.json has no "version" field; falling back to 0.1.0.');
}
if (packageVersion && tauriConf.version !== packageVersion) {
  console.warn(
    `WARNING: tauri.conf.json version (${tauriConf.version ?? 'missing'}) does not match package.json version (${packageVersion}).`
  );
}

const releaseDir = join(workspaceRoot, 'target', 'release');
const exeName = `${productName}.exe`;
const exePath = join(releaseDir, exeName);

if (!existsSync(exePath)) {
  console.error(`Release executable not found: ${exePath}`);
  console.error('Run `cargo tauri build` first.');
  process.exit(1);
}

const stagingDir = join(releaseDir, `${productName}-${version}-portable`);
const portableMarkerDir = join(stagingDir, 'cistella-portable');
const readmePath = join(stagingDir, 'README-portable.txt');
const zipName = `${productName}-${version}-portable.zip`;
const zipPath = join(releaseDir, zipName);

// Clean up any previous staging or zip from a prior run.
rmSync(stagingDir, { recursive: true, force: true });
if (existsSync(zipPath)) {
  rmSync(zipPath);
}

mkdirSync(stagingDir, { recursive: true });
mkdirSync(portableMarkerDir, { recursive: true });

// Copy the executable into the staging directory.
execSync(`copy "${exePath}" "${join(stagingDir, exeName)}"`, { stdio: 'inherit', shell: 'cmd.exe' });

writeFileSync(
  readmePath,
  `${productName} portable\r\n` +
    `Version: ${version}\r\n\r\n` +
    `Usage:\r\n` +
    `1. Copy this entire folder to any location (USB drive, cloud sync folder, etc.).\r\n` +
    `2. Double-click ${exeName} to run.\r\n` +
    `3. Your settings, recent vaults, and cache live inside the included\r\n` +
    `   cistella-portable/ folder, so the bundle stays self-contained.\r\n\r\n` +
    `Do not delete the cistella-portable/ folder while the app is running.\r\n`,
  'utf8'
);

// Use PowerShell + .NET ZipFile to create a true ZIP archive.
// Windows 10/11 ships with .NET Framework / PowerShell 5.1+, so no extra
// installation is required. ZipFile::CreateFromDirectory preserves empty
// directories, keeping the `cistella-portable/` marker folder in the bundle.
const psEscape = (s) => s.replace(/'/g, "''");
const zipCommand = [
  'powershell.exe',
  '-NoProfile',
  '-ExecutionPolicy',
  'Bypass',
  '-Command',
  `"Add-Type -AssemblyName System.IO.Compression.FileSystem; ` +
    `[System.IO.Compression.ZipFile]::CreateFromDirectory('${psEscape(stagingDir)}', '${psEscape(zipPath)}', [System.IO.Compression.CompressionLevel]::Optimal, \$true)"`,
].join(' ');

execSync(zipCommand, { stdio: 'inherit' });

console.log(`Portable bundle created: ${zipPath}`);

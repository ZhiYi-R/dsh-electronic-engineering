#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { promises as fs } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const PLATFORMS = new Set(['linux', 'darwin', 'win32']);
const ARCHES = new Set(['x64', 'arm64']);

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  console.log(`Usage: node scripts/package.mjs [options]

Options:
  --binary <path>     scpi executable (default: backend/target/release/scpi[.exe], else bin/scpi[.exe])
  --platform <name>   linux | darwin | win32 (default: current platform)
  --arch <name>       x64 | arm64 (default: current arch)
  --out <dir>         output directory (default: dist)
  --keep-stage        keep the staging directory
  --help              show this help
`);
  process.exit(0);
}

const platform = args.platform ?? process.platform;
const arch = args.arch ?? process.arch;
if (!PLATFORMS.has(platform)) fail(`unsupported platform '${platform}'`);
if (!ARCHES.has(arch)) fail(`unsupported arch '${arch}'`);

const extension = platform === 'win32' ? '.exe' : '';
const builtBinary = path.join('backend', 'target', 'release', `scpi${extension}`);
const bundledBinary = path.join('bin', `scpi${extension}`);
let binary = path.resolve(ROOT, args.binary ?? builtBinary);
if (args.binary === undefined && !(await exists(binary))) {
  binary = path.resolve(ROOT, bundledBinary);
}
const outputDir = path.resolve(ROOT, args.out ?? 'dist');
const packageJson = JSON.parse(await fs.readFile(path.join(ROOT, 'package.json'), 'utf8'));
const version = packageJson.version;
const archiveName = `ee-fat-whale-v${version}-${platform}-${arch}.${platform === 'win32' ? 'zip' : 'tar.gz'}`;
const archivePath = path.join(outputDir, archiveName);
const stageRoot = path.join(outputDir, `.stage-${platform}-${arch}-${process.pid}`);
const stagedRoot = path.join(stageRoot, 'ee-fat-whale');

if (!(await exists(binary))) fail(`binary not found: ${binary}`);
await fs.mkdir(outputDir, { recursive: true });
await fs.rm(stageRoot, { recursive: true, force: true });
await fs.mkdir(stagedRoot, { recursive: true });

try {
  const files = [
    'index.js',
    'client.js',
    'package.json',
    'cordis.patch.yml',
    'icon.png',
    'README.md',
  ];
  for (const file of files) {
    await copyFile(path.join(ROOT, file), path.join(stagedRoot, file));
  }
  for (const directory of ['host', 'locale', 'bundles', 'audit']) {
    await copyDirectory(path.join(ROOT, directory), path.join(stagedRoot, directory));
  }

  const binDir = path.join(stagedRoot, 'bin');
  await fs.mkdir(binDir, { recursive: true });
  const stagedBinary = path.join(binDir, `scpi${extension}`);
  await copyFile(binary, stagedBinary);
  if (platform !== 'win32') await fs.chmod(stagedBinary, 0o755);

  if (platform === 'win32') {
    const script = `Compress-Archive -LiteralPath ${psQuote(stagedRoot)} -DestinationPath ${psQuote(archivePath)} -Force`;
    run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script]);
  } else {
    run('tar', ['-czf', archivePath, '-C', stageRoot, 'ee-fat-whale']);
  }

  const buffer = await fs.readFile(archivePath);
  if (!buffer.length) fail(`archive is empty: ${archivePath}`);
  const digest = createHash('sha256').update(buffer).digest('hex');
  const checksumPath = `${archivePath}.sha256`;
  await fs.writeFile(checksumPath, `${digest}  ${archiveName}\n`);
  console.log(JSON.stringify({
    ok: true,
    archive: archivePath,
    checksum: checksumPath,
    platform,
    arch,
    bytes: buffer.length,
    sha256: digest,
  }, null, 2));
} finally {
  if (!args.keepStage) await fs.rm(stageRoot, { recursive: true, force: true });
}

function parseArgs(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (token === '--help' || token === '-h') result.help = true;
    else if (token === '--keep-stage') result.keepStage = true;
    else if (token.startsWith('--')) {
      const key = token.slice(2).replace(/-([a-z])/g, (_, char) => char.toUpperCase());
      const value = argv[index + 1];
      if (value === undefined || value.startsWith('--')) fail(`missing value for ${token}`);
      result[key] = value;
      index += 1;
    } else {
      fail(`unexpected argument '${token}'`);
    }
  }
  return result;
}

async function exists(target) {
  try {
    await fs.access(target);
    return true;
  } catch {
    return false;
  }
}

async function copyFile(source, destination) {
  await fs.mkdir(path.dirname(destination), { recursive: true });
  await fs.copyFile(source, destination);
}

async function copyDirectory(source, destination) {
  const entries = await fs.readdir(source, { withFileTypes: true });
  for (const entry of entries) {
    if (entry.name === '.DS_Store' || entry.name === 'node_modules') continue;
    const from = path.join(source, entry.name);
    const to = path.join(destination, entry.name);
    if (entry.isDirectory()) await copyDirectory(from, to);
    else if (entry.isFile()) await copyFile(from, to);
  }
}

function run(command, commandArgs) {
  const result = spawnSync(command, commandArgs, { stdio: 'inherit' });
  if (result.error) fail(`${command} failed: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} exited with status ${result.status}`);
}

function psQuote(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

function fail(message) {
  console.error(`package: ${message}`);
  process.exit(1);
}

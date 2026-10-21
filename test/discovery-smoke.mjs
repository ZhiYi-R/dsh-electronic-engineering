// End-to-end check for host/discover.js: a fake SCPI socket is probed through
// the real `scpi discover` binary, then matched against an installed profile.
//
// The test is skipped when no `scpi` binary is available, so it stays useful
// locally without breaking a checkout that has not built the backend yet.

import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, symlinkSync, mkdirSync } from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { clearDiscoveryCache, discover, scanOptions } from '../host/discover.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function findBinary() {
  const candidates = [
    process.env.SCPI_BINARY,
    path.join(root, 'backend', 'target', 'debug', 'scpi'),
    path.join(root, 'backend', 'target', 'release', 'scpi'),
    path.join(root, 'bin', 'scpi'),
  ].filter(Boolean);
  return candidates.find((candidate) => existsSync(candidate)) ?? null;
}

function fakeInstrument(idn) {
  const server = net.createServer((socket) => {
    socket.once('data', () => socket.write(`${idn}\n`));
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port }));
  });
}

const stores = {
  dm3058: {
    async list() {
      return [
        {
          id: 'rigol.dm3058',
          name: 'Rigol DM3058/DM3058E',
          valid: true,
          usb: { vid: '1ab1', pid: '09c4' },
          idn_match: ['DM3058'],
          network: { ports: [5025], idn_match: ['DM3058'] },
        },
        {
          id: 'rigol.mso5000',
          name: 'Rigol MSO5000',
          valid: true,
          usb: { vid: '1ab1', pid: '0515' },
          idn_match: ['MSO'],
          network: { ports: [5555], idn_match: ['MSO'] },
        },
      ];
    },
  },
};

const binary = findBinary();
if (!binary) {
  console.log('discovery smoke skipped: build the backend first (cargo build)');
  process.exit(0);
}

// resolveBinary() looks under <pluginRoot>/bin, so point it at a temp root.
const pluginRoot = mkdtempSync(path.join(os.tmpdir(), 'ee-fat-whale-'));
mkdirSync(path.join(pluginRoot, 'bin'));
symlinkSync(binary, path.join(pluginRoot, 'bin', 'scpi'));

const { server, port } = await fakeInstrument('Rigol Technologies,DM3058E,DM3R251000551,00.01.00');
try {
  const result = await discover({
    pluginRoot,
    store: stores.dm3058,
    ...scanOptions({ usb: false, mdns: false, sweep: false, hosts: [`127.0.0.1:${port}`] }),
  });

  assert.equal(result.instruments.length, 1, 'exactly one instrument');
  const [instrument] = result.instruments;
  assert.equal(instrument.resource, `tcp://127.0.0.1:${port}`);
  assert.equal(instrument.transport, 'lan');
  assert.equal(instrument.source, 'manual');
  assert.equal(instrument.model, 'DM3058E');
  assert.equal(instrument.profile_id, 'rigol.dm3058', 'matched by *IDN?');

  const again = await discover({
    pluginRoot,
    store: stores.dm3058,
    ...scanOptions({ usb: false, mdns: false, sweep: false, hosts: [`127.0.0.1:${port}`] }),
  });
  assert.equal(again, result, 'repeat scan is cached');

  clearDiscoveryCache();
  const fresh = await discover({
    pluginRoot,
    store: stores.dm3058,
    ...scanOptions({ usb: false, mdns: false, sweep: false, hosts: [`127.0.0.1:${port}`] }),
  });
  assert.notEqual(fresh, result, 'explicit invalidation bypasses the cache');

  console.log('discovery smoke ok');
} finally {
  server.close();
  rmSync(pluginRoot, { recursive: true, force: true });
}

import { spawn } from 'node:child_process';
import { constants as fsConstants, promises as fs } from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';

export async function resolveBinary(pluginRoot) {
  const extension = process.platform === 'win32' ? '.exe' : '';
  const candidates = [
    path.join(pluginRoot, 'bin', `scpi-${process.platform}-${process.arch}${extension}`),
    path.join(pluginRoot, 'bin', `scpi${extension}`),
    path.join(pluginRoot, 'bin', `scpi-${process.platform}-${process.arch}`),
    path.join(pluginRoot, 'bin', 'scpi'),
  ];
  const accessMode = process.platform === 'win32' ? fsConstants.F_OK : fsConstants.X_OK;
  for (const candidate of candidates) {
    try {
      await fs.access(candidate, accessMode);
      return candidate;
    } catch {
      // Try next candidate.
    }
  }
  throw new Error('bundled SCPI executable is missing from bin/');
}

export class SessionManager {
  constructor({ store, pluginRoot }) {
    this.store = store;
    this.pluginRoot = pluginRoot;
    this.sessions = new Map();
    this.counter = 0;
    this.closedListeners = new Set();
  }

  onClosed(listener) {
    this.closedListeners.add(listener);
    return () => this.closedListeners.delete(listener);
  }

  notifyClosed(id) {
    for (const listener of this.closedListeners) {
      try {
        listener(id);
      } catch {
        // Ignore listener errors during teardown.
      }
    }
  }

  async open({ profileId, transport }) {
    if (typeof profileId !== 'string' || !profileId) {
      throw new Error('profileId is required');
    }
    const config = await this.store.settings();
    if (this.sessions.size >= config.maxSessions) {
      throw new Error(
        `maximum number of SCPI sessions reached (${config.maxSessions}); close one first`,
      );
    }
    const profile = await this.store.get(profileId);
    const binary = await resolveBinary(this.pluginRoot);
    const spec = transport;
    if (!spec) {
      throw new Error('transport is required: pass the resource/transport explicitly');
    }
    const id = `scpi-${++this.counter}`;
    const session = await Session.spawn({
      id,
      binary,
      profile,
      transport: spec,
      verifyIdn: config.verifyIdn !== false,
      timeoutMs: config.requestTimeoutMs,
      onExit: (sessionId) => {
        this.sessions.delete(sessionId);
        this.notifyClosed(sessionId);
      },
    });
    this.sessions.set(id, session);
    return session.info();
  }

  list() {
    return [...this.sessions.values()].map((session) => session.info());
  }

  get(id) {
    const session = this.sessions.get(id);
    if (!session || session.closed) {
      throw new Error(`SCPI session '${id}' is not open`);
    }
    return session;
  }

  async request(id, op, params, options) {
    return this.get(id).request(op, params, options);
  }

  async close(id) {
    const session = this.sessions.get(id);
    if (!session) return { id, closed: false };
    await session.close();
    this.sessions.delete(id);
    return { id, closed: true };
  }

  isProfileActive(profileId) {
    return [...this.sessions.values()].some(
      (session) => !session.closed && session.profile.id === profileId,
    );
  }

  async closeAll() {
    await Promise.allSettled([...this.sessions.values()].map((session) => session.close()));
    this.sessions.clear();
  }
}

class Session {
  static async spawn({ id, binary, profile, transport, verifyIdn, timeoutMs, onExit }) {
    const args = ['serve', '--profile', profile.path, '--transport', transport];
    if (!verifyIdn) args.push('--no-idn-check');
    const child = spawn(binary, args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: process.env,
    });
    const session = new Session({ id, child, profile, transport, timeoutMs, onExit });
    await session.start();
    return session;
  }

  constructor({ id, child, profile, transport, timeoutMs, onExit }) {
    this.id = id;
    this.child = child;
    this.profile = profile;
    this.transport = transport;
    this.timeoutMs = timeoutMs;
    this.onExit = onExit;
    this.closed = false;
    this.closing = false;
    this.seq = 0;
    this.pending = new Map();
    this.stderr = '';
    this.profileInfo = null;
    this.spawned = new Promise((resolve, reject) => {
      child.once('spawn', resolve);
      child.once('error', reject);
    });
    this.rl = readline.createInterface({ input: child.stdout });
    this.rl.on('line', (line) => this.handleLine(line));
    child.stdin.on('error', (error) => {
      this.finalize(new Error(`SCPI backend stdin failed: ${error.message}`));
    });
    child.stderr.on('data', (chunk) => {
      this.stderr = `${this.stderr}${chunk.toString()}`.slice(-16 * 1024);
    });
    child.on('error', (error) => {
      this.finalize(new Error(`SCPI backend failed to start: ${error.message}`));
    });
    child.on('exit', (code, signal) => {
      const detail = this.stderr.trim();
      this.finalize(
        new Error(
          `SCPI backend exited (code ${code}, signal ${signal})${detail ? `: ${detail}` : ''}`,
        ),
      );
    });
  }

  async start() {
    try {
      await this.spawned;
      const response = await this.request('info');
      this.profileInfo = response.profile ?? null;
    } catch (error) {
      await this.close().catch(() => {});
      throw error;
    }
  }

  info() {
    return {
      session_id: this.id,
      pid: this.child.pid,
      profile_id: this.profile.id,
      profile_name: this.profile.name,
      transport: this.transport,
      closed: this.closed,
      idn: this.profileInfo?.idn,
      commands: this.profileInfo?.commands,
      io: this.profileInfo?.io,
    };
  }

  async request(op, params = {}, options = {}) {
    if (this.closed || this.closing) {
      throw new Error(`SCPI session '${this.id}' is closed`);
    }
    const id = ++this.seq;
    const timeoutMs = options.timeoutMs ?? this.timeoutMs;
    const closeOnTimeout = options.closeOnTimeout !== false;
    const message = `${JSON.stringify({ id, op, ...params })}\n`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`SCPI '${op}' timed out after ${timeoutMs} ms`));
        if (closeOnTimeout) void this.close();
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdin.write(message, (error) => {
        if (!error) return;
        const pending = this.pending.get(id);
        if (!pending) return;
        this.pending.delete(id);
        clearTimeout(pending.timer);
        pending.reject(new Error(`cannot write to SCPI backend: ${error.message}`));
      });
    });
  }

  handleLine(line) {
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      return;
    }
    const pending = this.pending.get(message.id);
    if (!pending) return;
    this.pending.delete(message.id);
    clearTimeout(pending.timer);
    if (message.ok) {
      pending.resolve(message);
      return;
    }
    const error = new Error(message.error?.message ?? 'SCPI request failed');
    error.kind = message.error?.kind;
    pending.reject(error);
  }

  async close() {
    if (this.closed || this.closing) return;
    this.closing = true;
    try {
      this.child.stdin.write(`${JSON.stringify({ op: 'shutdown' })}\n`);
    } catch {
      // Pipe already closed.
    }
    let exited = await waitForExit(this.child, 800);
    if (!exited) {
      this.child.kill('SIGTERM');
      exited = await waitForExit(this.child, 1500);
    }
    if (!exited) this.child.kill('SIGKILL');
    this.finalize(new Error('SCPI session closed'));
  }

  finalize(error) {
    if (this.closed) return;
    this.closed = true;
    this.closing = true;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    try {
      this.rl.close();
    } catch {
      // Already closed.
    }
    this.onExit?.(this.id);
  }
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(false), timeoutMs);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve(true);
    });
  });
}

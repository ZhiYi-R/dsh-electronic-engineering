import { spawn } from 'node:child_process';
import { constants as fsConstants, promises as fs } from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';

/**
 * 寻找随包附带的 SCPI 执行器二进制文件。
 * 支持两种命名规范：带架构后缀的独立分发包名称（如 scpi-linux-x64）或常规的 scpi / scpi.exe。
 * Windows 下检查文件是否存在（F_OK），类 Unix 系统下严格检查可执行权限（X_OK）。
 */
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
      // 当前候选路径不存在或无执行权限，继续尝试下一个备选路径
    }
  }
  throw new Error('bundled SCPI executable is missing from bin/');
}

/**
 * SCPI 后台会话管理器：维护活跃的底层进程实例，提供并发上限保护和状态广播
 */
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
        // 单个监听回调抛错不能中断会话整体清理流程
      }
    }
  }

  /**
   * 启动并建立一个新的仪器会话：
   * 1. 检查会话数量上限（防止过多子进程耗尽硬件总线资源）；
   * 2. 读取指定的 Profile 配置；
   * 3. 拉起 `scpi serve` 子进程并完成握手。
   */
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

/**
 * 单个 SCPI 执行器子进程会话：
 * 封装与 `scpi serve` 的 stdin/stdout JSON-lines 协议通信、超时熔断与优雅退出逻辑。
 */
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
    // 保留最近 16KB 的 stderr 输出，便于在底层崩溃时提供详尽的诊断信息
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

  /**
   * 初始化就绪检查：向后端发送 info 查询，获取仪器元数据和 *IDN? 信息
   */
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

  /**
   * 向执行器发送一条 JSON-lines 请求，并等待对应的响应
   * 每个请求附带自增序列号 id，通过 handleLine 匹配关联
   */
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
        // 硬件通信超时通常意味着总线死锁或设备无响应，安全起见主动终止该会话
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

  /**
   * 处理子进程 stdout 输出的一行 JSON 响应
   */
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

  /**
   * 优雅关闭子进程：
   * 1. 尝试发送 `shutdown` 操作通知 Rust 执行器安全释放硬件端点；
   * 2. 800ms 内未退出则发送 SIGTERM；
   * 3. 1500ms 内仍未退出则强制 SIGKILL。
   */
  async close() {
    if (this.closed || this.closing) return;
    this.closing = true;
    try {
      this.child.stdin.write(`${JSON.stringify({ op: 'shutdown' })}\n`);
    } catch {
      // 写入失败说明子进程已经退出或管道已断开，直接进入退出等待
    }
    let exited = await waitForExit(this.child, 800);
    if (!exited) {
      this.child.kill('SIGTERM');
      exited = await waitForExit(this.child, 1500);
    }
    if (!exited) this.child.kill('SIGKILL');
    this.finalize(new Error('SCPI session closed'));
  }

  /**
   * 会话清理终结：撤销所有挂起的 Promise 并触发退出回调
   */
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
      // readline 流可能已经关闭，静默忽略
    }
    this.onExit?.(this.id);
  }
}

/**
 * 辅助工具：在指定超时时间内等待子进程退出
 */
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

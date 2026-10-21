import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';

import { ProfileStore } from './host/profiles.js';
import { SessionManager } from './host/session.js';
import { registerTools } from './host/tools.js';
import { InstrumentTools } from './host/instruments.js';
import { registerRoutes } from './host/routes.js';

// 计算插件根目录与用户数据存储目录
// 优先遵循 DSH_PROFILE_DIR 环境变量，默认存放在 ~/.dsh/ 下，确保升级插件时用户自定义档案不会丢失
const pluginRoot = path.dirname(fileURLToPath(import.meta.url));
const baseDir = process.env.DSH_PROFILE_DIR || path.join(os.homedir(), '.dsh');

export const name = 'ee-fat-whale';
export const inject = ['tools'];

/**
 * 插件主入口：组装 Profile 存储、会话调度器和动态工具管理器
 */
export function apply(ctx, config = {}) {
  // 1. 初始化核心状态管理器
  const store = new ProfileStore({ pluginRoot, baseDir, config });
  const sessions = new SessionManager({ store, pluginRoot });
  const instruments = new InstrumentTools({ ctx, store, sessions });

  // 2. 注册生命周期钩子：插件卸载或重启时，妥善清理所有动态注册的工具，并优雅终止与底层的硬件子进程
  ctx.effect(() => () => {
    instruments.dispose();
    void sessions.closeAll();
  });

  // 3. 注册 5 个全局基础管理工具（profiles / scan / load / unload / session）
  registerTools(ctx, { store, sessions, pluginRoot, instruments });

  // 4. 可选的 Web 设置页路由：
  // 如果宿主环境启用了 webServer，则注册前端 API 接口供 DSH 设置面板使用；
  // 即使在纯命令行无 Web 环境下，核心的 5 个管理工具与 LLM 调用也能独立正常工作。
  ctx.inject(['webServer'], (webCtx) => {
    const dispose = registerRoutes(webCtx, { store, sessions, pluginRoot });
    webCtx.effect(() => dispose);
  });
}

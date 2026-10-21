import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

import { resolveBinary } from './session.js';

const execFileAsync = promisify(execFile);

/**
 * 自动发现 USBTMC 仪器：
 * 1. 运行底层执行器的 `scpi discover` 命令，直接枚举系统中符合 USBTMC 类规范的 USB 设备；
 * 2. 对比本地已安装的 Profile 列表：优先使用十六进制 VID/PID 精确匹配，或根据 IDN 关键字模糊匹配；
 * 3. 匹配成功后在返回结果中标记对应的 profile_id 与 profile_name，方便上层一键 load。
 */
export async function discover({ pluginRoot, store }) {
  const binary = await resolveBinary(pluginRoot);
  const { stdout } = await execFileAsync(
    binary,
    ['discover'],
    { timeout: 15000, maxBuffer: 2 * 1024 * 1024 },
  );
  let instruments;
  try {
    instruments = JSON.parse(stdout);
  } catch (error) {
    throw new Error(`invalid discovery output: ${error.message}`);
  }
  if (!Array.isArray(instruments)) {
    throw new Error('invalid discovery output: expected an array');
  }
  const profiles = (await store.list()).filter((profile) => profile.valid !== false);
  for (const instrument of instruments) {
    const idn = typeof instrument.idn === 'string' ? instrument.idn.toUpperCase() : null;
    const match = profiles.find((profile) => {
      // 优先通过 USB 硬件标识（VID/PID）进行确定性匹配
      if (profile.usb) {
        const vid = Number.parseInt(profile.usb.vid, 16);
        const pid = Number.parseInt(profile.usb.pid, 16);
        if (Number.isFinite(vid) && Number.isFinite(pid)) {
          if (instrument.vendor_id === vid && instrument.product_id === pid) return true;
        }
      }
      // 备选方案：通过 *IDN? 字符串子串比对
      return Boolean(
        idn &&
          (profile.idn_match ?? []).some((needle) => idn.includes(String(needle).toUpperCase())),
      );
    });
    if (match) {
      instrument.profile_id = match.id;
      instrument.profile_name = match.name;
    }
  }
  return {
    instruments,
    profiles: profiles.map((profile) => ({ id: profile.id, name: profile.name })),
  };
}

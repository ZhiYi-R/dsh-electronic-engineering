// SPDX-License-Identifier: AGPL-3.0-or-later
window.__ModuleLoader__.load({
  id: 'ee-fat-whale',
  factory(require) {
    const React = require('react');
    const h = React.createElement;
    const API = '/api/ee-fat-whale';

    const page = {
      display: 'flex',
      flexDirection: 'column',
      gap: '20px',
      padding: '28px',
      maxWidth: '760px',
      color: 'var(--dsw-alias-label-primary)',
      fontSize: '15px',
      lineHeight: 1.5,
    };
    const card = {
      display: 'flex',
      flexDirection: 'column',
      gap: '14px',
      padding: '18px',
      background: 'var(--dsw-alias-bg-layer-1)',
      border: '1px solid var(--dsw-alias-border-l1)',
      borderRadius: '12px',
    };
    const title = { margin: 0, fontSize: '18px', fontWeight: 600 };
    const sectionTitle = { margin: 0, fontSize: '16px', fontWeight: 600 };
    const input = {
      width: '100%',
      boxSizing: 'border-box',
      padding: '9px 11px',
      borderRadius: '8px',
      border: '1px solid var(--dsw-alias-border-l1)',
      background: 'var(--dsw-alias-bg-base)',
      color: 'var(--dsw-alias-label-primary)',
      font: 'inherit',
    };
    const button = {
      padding: '8px 14px',
      borderRadius: '8px',
      border: '1px solid var(--dsw-alias-border-l2)',
      background: 'var(--dsw-alias-bg-layer-2)',
      color: 'var(--dsw-alias-label-primary)',
      cursor: 'pointer',
      font: 'inherit',
    };
    const primary = {
      ...button,
      background: 'var(--dsw-alias-brand-primary)',
      color: '#ffffff',
      borderColor: 'transparent',
    };
    const danger = {
      ...button,
      color: 'var(--dsw-alias-state-error-primary)',
    };
    const field = { display: 'flex', flexDirection: 'column', gap: '6px' };
    const label = { fontSize: '14px', fontWeight: 500 };
    const secondary = { color: 'var(--dsw-alias-label-secondary)', fontSize: '14px' };
    const actions = { display: 'flex', gap: '8px', flexWrap: 'wrap' };
    const row = {
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'space-between',
      gap: '16px',
      paddingTop: '12px',
      borderTop: '1px solid var(--dsw-alias-border-l1)',
    };

    function Settings() {
      const [state, setState] = React.useState(null);
      const [error, setError] = React.useState('');
      const [notice, setNotice] = React.useState('');
      const [busy, setBusy] = React.useState(false);
      const [dir, setDir] = React.useState('');
      const [verifyIdn, setVerifyIdn] = React.useState(true);
      const [selected, setSelected] = React.useState(null);
      const bundleInput = React.useRef(null);
      const [found, setFound] = React.useState(null);

      const apply = React.useCallback((next) => {
        setState(next);
        setDir(next.profileDir ?? '');
        setVerifyIdn(next.settings?.verifyIdn !== false);
      }, []);

      const load = React.useCallback(async () => {
        try {
          setError('');
          const response = await fetch(`${API}/state`);
          const data = await response.json();
          if (!response.ok || data.ok === false) {
            throw new Error(data.error?.message ?? `HTTP ${response.status}`);
          }
          apply(data);
        } catch (cause) {
          setError(cause.message);
        }
      }, [apply]);

      React.useEffect(() => {
        void load();
      }, [load]);

      const run = async (operation) => {
        setBusy(true);
        setError('');
        setNotice('');
        try {
          const message = await operation();
          if (message) setNotice(message);
        } catch (cause) {
          setError(cause.message);
        } finally {
          setBusy(false);
        }
      };

      const post = async (suffix, body) => {
        const response = await fetch(`${API}${suffix}`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(body),
        });
        const data = await response.json();
        if (!response.ok || data.ok === false) {
          throw new Error(data.error?.message ?? `HTTP ${response.status}`);
        }
        apply(data);
      };

      const save = () =>
        run(async () => {
          await post('/settings', {
            profileDir: dir,
            verifyIdn,
          });
          return '已保存';
        });

      const installBundled = (name) =>
        run(async () => {
          await post('/profiles/install-bundled', { name });
          return '已安装';
        });

      const scan = () =>
        run(async () => {
          const response = await fetch(`${API}/discover`, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: '{}',
          });
          const data = await response.json();
          if (!response.ok || data.ok === false) {
            throw new Error(data.error?.message ?? `HTTP ${response.status}`);
          }
          const instruments = Array.isArray(data.instruments) ? data.instruments : [];
          setFound(instruments);
          return instruments.length ? `发现 ${instruments.length} 台仪器` : '未发现仪器';
        });

      const chooseBundle = async (event) => {
        const file = event.target.files?.[0];
        event.target.value = '';
        if (!file) return;
        if (file.size > 8 * 1024 * 1024) {
          setError('仪器包不能超过 8 MB');
          return;
        }
        try {
          const bytes = new Uint8Array(await file.arrayBuffer());
          let binary = '';
          for (let offset = 0; offset < bytes.length; offset += 0x8000) {
            binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
          }
          setSelected({ filename: file.name, data: btoa(binary), size: file.size });
          setError('');
        } catch (cause) {
          setError(`读取仪器包失败：${cause.message}`);
        }
      };

      const installSelected = () =>
        run(async () => {
          if (!selected) return undefined;
          await post('/profiles/import', {
            filename: selected.filename,
            data: selected.data,
          });
          setSelected(null);
          return '已安装';
        });

      const remove = (id) =>
        run(async () => {
          if (!window.confirm(`删除 ${id}？`)) return undefined;
          await post('/profiles/remove', { id });
          return '已删除';
        });

      if (!state) {
        return h(
          'div',
          { style: page },
          h('h2', { style: title }, '电子工程师大肥鱼'),
          error
            ? h('div', { style: { color: 'var(--dsw-alias-state-error-primary)' } }, error)
            : h('div', { style: secondary }, '加载中…'),
        );
      }

      const profiles = (state.profiles ?? []).map((profile) =>
        h(
          'div',
          { key: profile.id ?? profile.file, style: row },
          h(
            'div',
            { style: { display: 'flex', flexDirection: 'column', gap: '2px' } },
            h('div', { style: { fontWeight: 500 } }, profile.name ?? profile.file),
            h(
              'div',
              { style: secondary },
              profile.valid === false
                ? '无法读取'
                : `${profile.id} · ${profile.commands} 条命令`,
            ),
          ),
          profile.valid === false
            ? null
            : h(
                'button',
                {
                  type: 'button',
                  style: danger,
                  disabled: busy,
                  onClick: () => remove(profile.id),
                },
                '删除',
              ),
        ),
      );

      const instruments = (found ?? []).map((instrument, index) =>
        h(
          'div',
          { key: instrument.resource ?? index, style: row },
          h(
            'div',
            { style: { display: 'flex', flexDirection: 'column', gap: '2px', minWidth: 0 } },
            h(
              'div',
              { style: { fontWeight: 500, wordBreak: 'break-all' } },
              instrument.product || instrument.manufacturer || instrument.resource,
            ),
            h(
              'div',
              { style: secondary },
              instrument.error
                ? instrument.error
                : `${instrument.resource}${instrument.profile_id ? ` · ${instrument.profile_name}` : ''}`,
            ),
          ),
        ),
      );

      const bundled = (state.bundled ?? []).map((profile) =>
        h(
          'div',
          { key: profile.bundled_name ?? profile.file, style: row },
          h(
            'div',
            { style: { display: 'flex', flexDirection: 'column', gap: '2px' } },
            h('div', { style: { fontWeight: 500 } }, profile.name ?? profile.file),
            h(
              'div',
              { style: secondary },
              profile.valid === false ? '无法读取' : `${profile.commands ?? 0} 条命令`,
            ),
          ),
          profile.valid === false
            ? null
            : h(
                'button',
                {
                  type: 'button',
                  style: button,
                  disabled: busy,
                  onClick: () => installBundled(profile.bundled_name),
                },
                '安装',
              ),
        ),
      );

      return h(
        'div',
        { style: page },
        h('h2', { style: title }, '电子工程师大肥鱼'),
        error ? h('div', { style: { color: 'var(--dsw-alias-state-error-primary)' } }, error) : null,
        notice
          ? h('div', { style: { color: 'var(--dsw-alias-state-success-primary)' } }, notice)
          : null,

        h(
          'section',
          { style: card },
          h('h3', { style: sectionTitle }, '设置'),
          h(
            'label',
            { style: field },
            h('span', { style: label }, 'Profile 目录'),
            h('input', {
              style: input,
              value: dir,
              disabled: busy,
              onChange: (event) => setDir(event.target.value),
            }),
          ),
          state.binaryError
            ? h('div', { style: { color: 'var(--dsw-alias-state-error-primary)' } }, '未找到执行器')
            : null,
          h(
            'label',
            { style: { display: 'flex', alignItems: 'center', gap: '8px' } },
            h('input', {
              type: 'checkbox',
              checked: verifyIdn,
              disabled: busy,
              onChange: (event) => setVerifyIdn(event.target.checked),
            }),
            h('span', { style: label }, '打开会话时校验 *IDN?'),
          ),
          h(
            'div',
            { style: actions },
            h('button', { type: 'button', style: primary, disabled: busy, onClick: save }, '保存'),
            h('button', { type: 'button', style: button, disabled: busy, onClick: () => void load() }, '刷新'),
          ),
        ),

        h(
          'section',
          { style: card },
          h(
            'div',
            { style: { display: 'flex', alignItems: 'center', justifyContent: 'space-between' } },
            h('h3', { style: sectionTitle }, '发现仪器'),
            h('button', { type: 'button', style: button, disabled: busy, onClick: scan }, '扫描'),
          ),
          found === null
            ? h('div', { style: secondary }, '未扫描')
            : instruments.length
              ? instruments
              : h('div', { style: secondary }, '未发现仪器'),
        ),

        h(
          'section',
          { style: card },
          h('h3', { style: sectionTitle }, '已安装档案'),
          profiles.length ? profiles : h('div', { style: secondary }, '暂无'),
        ),

        h(
          'section',
          { style: card },
          h('h3', { style: sectionTitle }, '安装档案'),
          bundled.length ? bundled : h('div', { style: secondary }, '暂无内置档案'),
          h(
            'div',
            { style: actions },
            h(
              'button',
              {
                type: 'button',
                style: button,
                disabled: busy,
                onClick: () => bundleInput.current?.click(),
              },
              '选择仪器包',
            ),
            h('input', {
              ref: bundleInput,
              type: 'file',
              accept: '.tar.gz,.tgz,.zip',
              style: { display: 'none' },
              onChange: chooseBundle,
            }),
            selected
              ? h(
                  'button',
                  { type: 'button', style: primary, disabled: busy, onClick: installSelected },
                  '安装',
                )
              : null,
          ),
          h('div', { style: secondary }, selected?.filename || '未选择仪器包'),
        ),
      );
    }

    return {
      inject: ['slots'],
      apply(ctx) {
        ctx.slots.inject('settings.section', () =>
          ctx.slots.register(
            {
              name: 'settings.section',
              id: 'ee-fat-whale',
              order: 60,
              label: '电子工程师大肥鱼',
            },
            Settings,
          ),
        );
      },
    };
  },
});

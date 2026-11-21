// SPDX-License-Identifier: GPL-3.0-only
'use strict';
'require view';
'require rpc';

const status = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'status', expect: { '': {} }
});
const listFiles = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'listFiles', expect: { '': {} }
});
const readConfig = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'readConfig', params: [ 'file' ], expect: { '': {} }
});
const writeConfig = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'writeConfig', params: [ 'file', 'text' ], expect: { '': {} }
});

return view.extend({
    handleSaveApply: null,
    handleSave: null,
    handleReset: null,

    load: async function() {
        if (!L.hasViewPermission())
            return null;
        const [service, { files }] = await Promise.all([status(), listFiles()]);
        return { service, files: files || [], initial: await readConfig('config.json') };
    },

    render: function(data) {
        const heading = E('h2', {}, [ '代理编辑器' ]);
        if (!data?.files.length)
            return E('section', { class: 'cbi-section' }, [ heading, E('p', {}, [ '查看配置需要代理管理权限。' ]) ]);

        const state = E('span', {}, [ data.service.running ? '已运行' : '未运行' ]);
        const message = E('p', { role: 'status', style: 'white-space:pre-wrap;overflow-wrap:anywhere' });
        const input = E('textarea', {
            class: 'cbi-input-textarea', rows: 25,
            style: 'box-sizing:border-box;width:100%;min-height:24rem;height:60vh;font-family:monospace;resize:vertical',
            spellcheck: 'false', wrap: 'off', 'aria-label': '代理文件内容'
        });
        const files = new Map(data.files.map(entry => [entry.file, entry]));
        const drafts = new Map();
        let current = 'config.json';

        const selector = E('select', { class: 'cbi-input-select', 'aria-label': '选择配置文件',
            change: async () => {
                drafts.set(current, input.value);
                const next = selector.value;
                setBusy(true);
                try {
                    const reply = drafts.has(next) ? { ok: true, text: drafts.get(next) } : await readConfig(next);
                    show(reply);
                    current = next;
                } catch (error) {
                    selector.value = current;
                    message.textContent = error.message;
                } finally {
                    setBusy(false);
                }
            }
        }, data.files.map(entry => E('option', { value: entry.file }, [ entry.title ])));
        const save = E('button', { type: 'button', class: 'btn cbi-button cbi-button-apply',
            click: async () => {
                setBusy(true);
                message.textContent = '正在校验并应用配置…';
                try {
                    const reply = await writeConfig(current, input.value);
                    message.textContent = reply.ok ? '代理文件已保存并生效。' : (reply.error || '配置应用失败。');
                    state.textContent = (await status()).running ? '已运行' : '未运行';
                } catch (error) {
                    message.textContent = error.message;
                } finally {
                    setBusy(false);
                }
            }
        }, [ '保存并应用' ]);

        function show(reply) {
            if (!reply.ok)
                throw new Error(reply.error || '读取配置失败');
            input.value = reply.text;
            message.textContent = '';
        }
        function setBusy(busy) {
            selector.disabled = busy;
            save.disabled = input.readOnly = busy || !files.get(current)?.writable;
        }

        if (data.initial.ok)
            show(data.initial);
        else
            message.textContent = data.initial.error;
        setBusy(false);
        return E('section', { class: 'cbi-section' }, [
            heading,
            E('p', {}, [ 'sing-box · ', state ]),
            E('div', { class: 'control-group', style: 'margin-bottom:.75rem' }, [ selector ]),
            input,
            E('div', { class: 'control-group', style: 'margin-top:.75rem' }, [ save ]),
            message
        ]);
    }
});
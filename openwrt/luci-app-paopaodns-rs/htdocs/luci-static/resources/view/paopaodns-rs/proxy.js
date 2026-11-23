// SPDX-License-Identifier: GPL-3.0-only
'use strict';
'require view';
'require rpc';

const readConfig = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'readConfig', expect: { '': {} }
});
const writeConfig = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'writeConfig', params: [ 'text' ], expect: { '': {} }
});

return view.extend({
    handleSaveApply: null,
    handleSave: null,
    handleReset: null,

    load: function() {
        return L.hasViewPermission() ? readConfig() : null;
    },

    render: function(config) {
        const heading = E('h2', {}, [ '代理编辑器' ]);
        if (!config)
            return E('section', { class: 'cbi-section' }, [ heading, E('p', {}, [ '查看配置需要代理管理权限。' ]) ]);
        const message = E('p', { role: 'status', style: 'white-space:pre-wrap;overflow-wrap:anywhere' },
            config.ok ? [] : [config.error || '读取配置失败']);
        const input = E('textarea', {
            class: 'cbi-input-textarea', rows: 25,
            style: 'box-sizing:border-box;width:100%;min-height:24rem;height:60vh;font-family:monospace;resize:vertical',
            spellcheck: 'false', wrap: 'off', 'aria-label': 'sing-box config.json'
        });
        input.value = config.text || '';
        input.readOnly = !config.ok;
        const save = E('button', { type: 'button', class: 'btn cbi-button cbi-button-apply',
            click: async () => {
                save.disabled = input.readOnly = true;
                message.textContent = '正在校验并应用配置…';
                try {
                    const reply = await writeConfig(input.value);
                    message.textContent = reply.ok ? '代理配置已保存并生效。' : (reply.error || '配置应用失败。');
                } catch (error) {
                    message.textContent = error.message;
                } finally {
                    save.disabled = input.readOnly = false;
                }
            }
        }, [ '保存并应用' ]);
        save.disabled = !config.ok;
        return E('section', { class: 'cbi-section' }, [
            heading,
            input,
            E('div', { class: 'control-group', style: 'margin-top:.75rem' }, [ save ]),
            message
        ]);
    }
});

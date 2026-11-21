'use strict';
'require view';
'require rpc';

const readRule = rpc.declare({
    object: 'luci.paopaodns', method: 'readRule', params: [ 'rule' ], expect: { '': {} }
});
const writeRule = rpc.declare({
    object: 'luci.paopaodns', method: 'writeRule', params: [ 'rule', 'text' ], expect: { '': {} }
});

function button(label, handler, kind) {
    return E('button', {
        type: 'button',
        class: 'btn cbi-button cbi-button-' + (kind || 'action'),
        click: handler
    }, [ label ]);
}

function checkReply(reply) {
    if (!reply.ok)
        throw new Error(reply.error || '请求失败');
    return reply;
}

function renderRuleEditor(writable) {
    const files = [
        [ 'forward', '指定上游', 'force_forward_list.txt' ],
        [ 'encrypted', '强制加密', 'force_dnscrypt_list.txt' ],
        [ 'recursive', '强制递归', 'force_recurse_list.txt' ]
    ];
    const text = E('textarea', {
        class: 'cbi-input-textarea',
        rows: 25,
        style: 'box-sizing:border-box;width:100%;max-width:100%;height:64vh;min-height:24rem;font-family:monospace;resize:vertical',
        spellcheck: 'false',
        wrap: 'off',
        'aria-label': '规则文件内容'
    });
    const status = E('span', { role: 'status' });
    const drafts = new Map();
    let current;
    let busy = false;

    function update() {
        for (const [rule, choice] of choices) {
            const selected = rule === current;
            choice.disabled = busy;
            choice.setAttribute('aria-pressed', String(selected));
            choice.classList.toggle('cbi-button-apply', selected);
            choice.classList.toggle('cbi-button-action', !selected);
        }
        text.disabled = busy || !current;
        text.readOnly = !writable;
        save.disabled = !writable || busy || !current;
    }

    async function selectRule(rule) {
        if (busy || rule === current)
            return;
        if (current)
            drafts.set(current, text.value);
        current = null;
        busy = true;
        status.textContent = '';
        update();
        try {
            if (!drafts.has(rule))
                drafts.set(rule, checkReply(await readRule(rule)).text || '');
            current = rule;
            text.value = drafts.get(rule);
        } catch (e) {
            text.value = '';
            status.textContent = e.message;
        } finally {
            busy = false;
            update();
        }
    }

    async function saveRule() {
        busy = true;
        status.textContent = '';
        update();
        try {
            const reply = checkReply(await writeRule(current, text.value));
            drafts.set(current, text.value);
            status.textContent = reply.warning || '规则已保存';
        } catch (e) {
            status.textContent = e.message;
        } finally {
            busy = false;
            update();
        }
    }

    const choices = files.map(([rule, title, filename]) => [
        rule,
        E('button', {
            type: 'button',
            class: 'btn cbi-button',
            'aria-pressed': 'false',
            style: 'display:block;width:100%;min-width:0;min-height:4rem;text-align:left;white-space:normal;line-height:1.4;padding:.55rem .75rem',
            click: () => selectRule(rule)
        }, [
            E('strong', { style: 'display:block' }, [ title ]),
            E('code', { style: 'display:block;font-size:.9em;white-space:normal;overflow-wrap:anywhere' }, [ filename ])
        ])
    ]);
    const picker = E('div', {
        role: 'group',
        'aria-label': '选择规则文件',
        style: 'display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,10rem),1fr));gap:.7rem'
    }, choices.map(([, choice]) => choice));
    const save = button('保存规则', saveRule, 'apply');
    void selectRule(files[0][0]);

    return E('section', { class: 'cbi-section' }, [
        E('h2', { style: 'margin-bottom:1rem' }, [ '规则编辑器' ]),
        E('div', { class: 'cbi-section-node' }, [
            picker,
            E('p', { style: 'margin:.75rem 0;line-height:1.5' }, [
                '每行一条规则，支持 domain:、full:、keyword:、regexp: 和 # 注释。'
            ]),
            text,
            E('div', { class: 'control-group', style: 'margin-top:.75rem;display:flex;align-items:center;flex-wrap:wrap;gap:.75rem' }, [
                save, status
            ])
        ])
    ]);
}

return view.extend({
    handleSaveApply: null,
    handleSave: null,
    handleReset: null,

    render: function() {
        return renderRuleEditor(L.hasViewPermission());
    }
});

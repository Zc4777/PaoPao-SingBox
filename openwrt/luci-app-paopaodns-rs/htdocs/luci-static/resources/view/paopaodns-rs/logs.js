'use strict';
'require view';
'require rpc';
'require ui';
'require poll';

const readLogs = rpc.declare({
    object: 'luci.paopaodns',
    method: 'readLogs',
    params: [ 'kind' ],
    expect: { '': {} }
});

const readProxyLogs = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'readLogs',
    params: [ 'kind' ], expect: { '': {} }
});

function renderLogPane(kind, title, read = readLogs) {
    const output = E('textarea', {
        class: 'cbi-input-textarea',
        readonly: '',
        rows: 25,
        wrap: 'off',
        style: 'width:100%;height:64vh;min-height:24rem;font-family:monospace;resize:vertical',
        'aria-label': title
    });
    const status = E('span', { role: 'status' });
    let updating = false;

    function scrollToBottom() {
        output.scrollTop = output.scrollHeight;
    }

    async function refresh() {
        if (updating)
            return;
        updating = true;
        try {
            const reply = await read(kind);
            if (!reply.ok)
                throw new Error(reply.error || '读取日志失败');

            const scroll = output.scrollTop;
            const atBottom = output.scrollHeight - output.clientHeight - scroll < 4;
            if (output.value !== reply.text) {
                output.value = reply.text;
                if (atBottom)
                    scrollToBottom();
                else
                    output.scrollTop = scroll;
            }
            status.textContent = reply.total ? '最近 ' + reply.total + ' 条' : '暂无日志';
        } catch (e) {
            status.textContent = e.message;
        } finally {
            updating = false;
        }
    }

    const pane = E('div', {
        'data-tab': kind,
        'data-tab-title': title,
        class: 'cbi-section'
    }, [

        output,
        E('div', { class: 'control-group', style: 'display:flex;align-items:center;gap:.45rem;flex-wrap:wrap' }, [
            E('button', {
                type: 'button', class: 'btn cbi-button',
                click: scrollToBottom
            }, [ '滚动到底部' ]),
            status
        ])
    ]);
    pane.addEventListener('cbi-tab-active', () => void refresh());
    poll.add(() => pane.getAttribute('data-tab-active') === 'true'
        ? refresh()
        : Promise.resolve(), 3);
    return pane;
}

return view.extend({
    handleSaveApply: null,
    handleSave: null,
    handleReset: null,

    render: function() {
        const panes = [
            renderLogPane('queries', '客户端查询'),
            renderLogPane('service', 'DNS 服务日志'),
            renderLogPane('proxy-service', '代理服务日志', readProxyLogs),
            renderLogPane('proxy-core', '核心日志', readProxyLogs)
        ];
        const root = E('div', {}, [
            E('h2', {}, [ '日志' ]),
            E('div', { class: 'cbi-tabcontainer' }, panes)
        ]);
        ui.tabs.initTabGroup(panes);
        return root;
    }
});

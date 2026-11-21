'use strict';
'require view';
'require form';
'require rpc';
'require ui';
'require poll';

const callStatus = rpc.declare({
    object: 'luci.paopaodns', method: 'status', expect: { '': {} }
});
const callAction = rpc.declare({
    object: 'luci.paopaodns', method: 'serviceAction', params: [ 'action' ], expect: { '': {} }
});
const updateAddressDb = rpc.declare({
    object: 'luci.paopaodns', method: 'updateAddressDb', expect: { '': {} }
});

const callCoreStatus = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'status', expect: { '': {} }
});
const callCoreAction = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'coreAction', params: [ 'action' ], expect: { '': {} }
});

function button(label, handler, kind) {
    return E('button', {
        type: 'button',
        class: 'btn cbi-button cbi-button-' + (kind || 'action'),
        click: handler
    }, [ label ]);
}

function notify(text, kind) {
    ui.addNotification(null, E('p', {}, [ text ]), kind);
}

function checkReply(reply) {
    if (!reply.ok)
        throw new Error(reply.error || '请求失败');
    return reply;
}

function renderService(writable) {
    const label = E('p', { role: 'status' });
    const latencyLabel = E('p', {});
    const core = renderCore(writable);
    const updateLabel = E('span', {
        role: 'status',
        hidden: '',
        style: 'min-width:0;overflow-wrap:anywhere;line-height:1.4'
    });
    let pending = false;
    let requestingUpdate = false;
    let updateError = '';
    let state;

    function update() {
        label.textContent = state == null ? '无法读取服务状态' : !state.running ? '已停止' :
            [
                '运行中 · PID ' + state.pid,
                state.rss_kib != null ? '实际内存 ' + (state.rss_kib / 1024).toFixed(1) + ' MiB' : null,
                state.limit_bytes == null ? null : '虚拟内存上限 ' + (state.limit_bytes ?
                    (state.limit_bytes / 1048576).toFixed(0) + ' MiB' : '不限制')
            ].filter(Boolean).join(' · ');
        const latency = state?.latency;
        latencyLabel.textContent = '平均 DNS 响应时间 ' + (latency?.avg_us != null
            ? (latency.avg_us / 1000).toFixed(3) + ' ms（已回复 ' + latency.replies + ' 条）'
            : '暂无已回复请求');
        start.disabled = !writable || pending || !!state?.running;
        stop.disabled = !writable || pending || !state?.running;
        restart.disabled = stop.disabled;
        const dataUpdate = state?.data_update;
        updateData.disabled = !writable || requestingUpdate || dataUpdate?.state === 'running';
        updateLabel.textContent = requestingUpdate || dataUpdate?.state === 'running'
            ? '正在更新 CN 地址库和域名标记…'
            : (updateError || dataUpdate?.message || '');
        updateLabel.hidden = !updateLabel.textContent;
    }

    async function refresh() {
        try {
            state = await callStatus();
            if (typeof state.running !== 'boolean')
                state = null;
        } catch (e) {
            state = null;
        }
        update();
    }

    async function control(action) {
        if (pending)
            return;
        pending = true;
        update();
        try {
            checkReply(await callAction(action));
        } catch (e) {
            notify(e.message, 'error');
        } finally {
            pending = false;
            await refresh();
        }
    }

    const start = button('启动', () => control('start'), 'apply');
    const stop = button('停止', () => control('stop'), 'reset');
    const restart = button('重启', () => control('restart'));
    const updateData = button('更新分流数据', async () => {
        requestingUpdate = true;
        updateError = '';
        update();
        try {
            checkReply(await updateAddressDb());
        } catch (e) {
            updateError = e.message;
        } finally {
            requestingUpdate = false;
            await refresh();
        }
    });
    updateData.disabled = !writable;
    void refresh();
    poll.add(refresh, 3);

    return E('section', { class: 'cbi-section' }, [
        E('h2', {}, [ 'PaoPaoDNS-SingBox' ]),
        E('div', { class: 'cbi-section-node' }, [
            label, latencyLabel, core.label, E('div', {
                class: 'control-group',
                style: 'display:flex;align-items:center;flex-wrap:wrap;gap:.5rem'
            }, [ start, stop, restart, updateData, ...core.controls, updateLabel ])
        ])
    ]);
}

function renderCore(writable) {
    const label = E('p', { role: 'status' });
    const dashboard = E('a', {
        class: 'btn cbi-button cbi-button-action', target: '_blank', rel: 'noopener noreferrer', hidden: ''
    }, [ '打开 sing-box 监控面板' ]);
    let pending = false;
    let running;
    let dashboardPort;

    function update() {
        label.textContent = 'sing-box · ' + (running == null ? '状态未知' : running ? '运行中' : '已停止');
        start.disabled = !writable || pending || running !== false;
        stop.disabled = !writable || pending || running !== true;
        dashboard.hidden = !running || !Number.isInteger(dashboardPort);
        if (dashboard.hidden) {
            dashboard.removeAttribute('href');
        } else {
            const url = new URL('/dashboard/', window.location.href);
            url.protocol = 'http:';
            url.port = dashboardPort;
            dashboard.href = url.href;
        }
    }

    async function refresh() {
        try {
            const state = await callCoreStatus();
            running = typeof state.running === 'boolean' ? state.running : null;
            dashboardPort = state.dashboard_port;
        } catch (e) {
            running = null;
            dashboardPort = null;
        }
        update();
    }

    async function control(action) {
        if (pending)
            return;
        pending = true;
        update();
        try {
            checkReply(await callCoreAction(action));
        } catch (e) {
            notify(e.message, 'error');
        } finally {
            pending = false;
            await refresh();
        }
    }

    const start = button('启动 sing-box', () => control('start'), 'apply');
    const stop = button('停止 sing-box', () => control('stop'), 'reset');
    update();
    void refresh();
    poll.add(refresh, 3);
    return { label, controls: [ start, stop, dashboard ] };
}

function createSettings() {
    const map = new form.Map('paopaodns-rs');
    const section = map.section(form.NamedSection, 'main', 'paopaodns', '服务设置');
    const enabled = section.option(form.Flag, 'enabled', '开机自动启动');
    enabled.default = enabled.enabled;
    enabled.rmempty = false;

    const memory = section.option(form.Value, 'memory_limit_mib', '进程内存上限（MiB）',
        'Linux 对此进程的虚拟地址空间限制。填 0 表示不限制，其他值可填 32–4096。');
    memory.default = '0';
    memory.rmempty = false;
    memory.validate = function(sectionId, value) {
        return (/^(0|[1-9][0-9]{1,3})$/.test(value) && (+value === 0 || (+value >= 32 && +value <= 4096))) ||
            '请输入 0 或 32–4096 之间的整数';
    };

    const diskLimit = section.option(form.Value, 'cache_file_mib', '缓存文件大小上限（MiB）');
    diskLimit.default = '4';
    diskLimit.datatype = 'and(uinteger,range(1,32))';
    diskLimit.rmempty = false;
    const recursive = section.option(form.Value, 'recursive', '递归上游',
        '国内域名和强制递归规则使用此上游，未分类的查询也先在这里尝试。可填 127.0.0.1:53 使用路由器的 dnsmasq。');
    recursive.placeholder = '127.0.0.1:5301';
    const encrypted = section.option(form.Value, 'encrypted', '加密上游',
        '命中强制加密规则时使用此上游。自定义上游留空时，也用于已判定的非 CN 查询。直填 223.6.6.6:853 使用 DoT，填本地转发器地址时由转发器决定对外协议。');
    encrypted.placeholder = '127.0.0.1:5302';
    const custom = section.option(form.Value, 'custom_forward', '自定义上游（Fake-IP）',
        '集成 sing-box 填 127.0.0.1:1020。用于已判定为非 CN 的 A 和 AAAA 查询，以及命中指定上游规则的域名。留空时使用加密上游。');
    custom.placeholder = '127.0.0.1:1020';
    const familyChecks = new Map();
    custom.renderWidget = function(sectionId, optionIndex, value) {
        const address = form.Value.prototype.renderWidget.call(this, sectionId, optionIndex, value);
        const family = this.map.data.get(this.map.config, sectionId, 'custom_ip') || 'ipv4';
        const readOnly = this.readonly ?? this.map.readonly;
        const ipv4 = E('input', { type: 'checkbox', disabled: readOnly ? '' : null });
        const ipv6 = E('input', { type: 'checkbox', disabled: readOnly ? '' : null });
        ipv4.checked = family !== 'ipv6';
        ipv6.checked = family !== 'ipv4';
        for (const control of [ ipv4, ipv6 ]) {
            control.addEventListener('change', () => {
                if (!ipv4.checked && !ipv6.checked)
                    control.checked = true;
            });
        }
        familyChecks.set(sectionId, { ipv4, ipv6 });

        return E('div', { style: 'display:flex;flex-wrap:wrap;align-items:center;gap:.75rem' }, [
            address,
            E('label', {}, [ ipv4, ' IPv4' ]),
            E('label', {}, [ ipv6, ' IPv6' ])
        ]);
    };
    custom.parse = async function(sectionId) {
        await form.Value.prototype.parse.call(this, sectionId);
        const controls = familyChecks.get(sectionId);
        if (!controls || this.map.readonly)
            return;
        const family = controls.ipv4.checked
            ? (controls.ipv6.checked ? 'both' : 'ipv4')
            : 'ipv6';
        this.map.data.set(this.map.config, sectionId, 'custom_ip', family);
    };

    return map;
}

return view.extend({
    render: function() {
        const writable = L.hasViewPermission();
        return createSettings().render().then(node => E('div', {}, [
            renderService(writable), node
        ]));
    }
});

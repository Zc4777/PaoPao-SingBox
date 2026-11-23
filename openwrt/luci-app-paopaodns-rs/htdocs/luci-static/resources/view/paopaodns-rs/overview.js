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
const callTrafficHistory = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'trafficHistory', expect: { '': {} }
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

function metric(title) {
    const value = E('strong', { class: 'pp-metric-value' }, [ '—' ]);
    const detail = E('span', { class: 'pp-muted' });
    return {
        value, detail,
        node: E('div', { class: 'pp-metric' }, [
            E('span', { class: 'pp-muted' }, [ title ]), value, detail
        ])
    };
}

function renderService(writable) {
    const label = E('span', { class: 'pp-status', role: 'status' }, [ '读取状态…' ]);
    const latencyMetric = metric('平均 DNS 响应');
    const repliesMetric = metric('已回复请求');
    const memoryMetric = metric('实际内存');
    const number = new Intl.NumberFormat('zh-CN');
    const core = renderCore(writable);
    const updateLabel = E('span', {
        class: 'pp-update pp-muted',
        role: 'status',
        hidden: ''
    });
    let pending = false;
    let requestingUpdate = false;
    let updateError = '';
    let state;

    function update() {
        label.textContent = 'DNS · ' + (state == null ? '状态未知' : state.running ? '运行中' : '已停止');
        label.dataset.running = String(state?.running === true);
        const latency = state?.latency;
        latencyMetric.value.textContent = latency?.avg_us == null ? '—' :
            (latency.avg_us / 1000).toFixed(2) + ' ms';
        repliesMetric.value.textContent = latency?.replies == null ? '—' : number.format(latency.replies);
        memoryMetric.value.textContent = state?.rss_kib == null ? '—' :
            (state.rss_kib / 1024).toFixed(1) + ' MiB';
        memoryMetric.detail.textContent = state?.limit_bytes == null ? '' : '虚拟内存上限 ' +
            (state.limit_bytes ? (state.limit_bytes / 1048576).toFixed(0) + ' MiB' : '不限');
        label.title = state?.running && state.pid ? 'PID ' + state.pid : '';
        start.disabled = !writable || pending || state?.running !== false;
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

    const start = button('启动 DNS', () => control('start'), 'apply');
    const stop = button('停止 DNS', () => control('stop'), 'reset');
    const restart = button('重启 DNS', () => control('restart'));
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
    update();
    void refresh();
    poll.add(refresh, 3);

    return E('section', { class: 'pp-dashboard' }, [
        E('div', { class: 'pp-heading' }, [ E('h2', {}, [ 'PaoPaoDNS-SingBox' ]), label ]),
        E('div', { class: 'pp-card' }, [
            E('div', { class: 'pp-metrics' }, [ latencyMetric.node, repliesMetric.node, memoryMetric.node ]),
            E('div', { class: 'pp-actions' }, [
                E('div', { class: 'pp-action-group', role: 'group', 'aria-label': 'DNS 控制' },
                    [ start, stop, restart, updateData ]),
                E('div', { class: 'pp-action-group', role: 'group', 'aria-label': 'sing-box 控制' }, core)
            ]),
            updateLabel,
            renderTrafficHistory()
        ])
    ]);
}

function renderCore(writable) {
    const dashboard = E('a', {
        class: 'btn cbi-button cbi-button-action', target: '_blank', rel: 'noopener noreferrer', hidden: ''
    }, [ '打开监控面板 ↗' ]);
    let pending = false;
    let running;
    let dashboardPort;

    function update() {
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
    return [ start, stop, dashboard ];
}

function trafficSize(bytes) {
    const units = [ 'B', 'KiB', 'MiB', 'GiB', 'TiB' ];
    let unit = 0;
    while (bytes >= 1024 && unit < units.length - 1) {
        bytes /= 1024;
        unit++;
    }
    return bytes.toFixed(unit ? 1 : 0) + ' ' + units[unit];
}

function renderTrafficHistory() {
    const upload = E('span', { class: 'pp-upload' });
    const download = E('span', { class: 'pp-download' });
    const canvas = E('canvas', {
        width: 1200, height: 120, class: 'pp-traffic-chart',
        role: 'img', 'aria-label': '最近 24 小时的上传和下载速率曲线'
    });
    const message = E('span', { class: 'pp-muted', role: 'status' }, [ '正在读取…' ]);
    const chart = E('div', { class: 'pp-chart', hidden: '' }, [
        canvas,
        E('div', { class: 'pp-axis pp-muted', 'aria-hidden': 'true' }, [
            E('span', {}, [ '24 小时前' ]), E('span', {}, [ '现在' ])
        ])
    ]);

    function draw(samples, now) {
        const { width, height } = canvas;
        const context = canvas.getContext('2d');
        if (!context)
            return;
        const since = now - 86400;
        const x = timestamp => (timestamp - since) * width / 86400;
        const peak = Math.max(1, ...samples.map(point => Math.max(point[2], point[3]) / point[1]));
        context.clearRect(0, 0, width, height);
        context.fillStyle = 'rgba(127,127,127,.18)';
        for (let i = 1; i <= 4; i++)
            context.fillRect(0, i * (height - 2) / 4, width, 1);

        const style = getComputedStyle(canvas);
        const series = [[3, '--pp-download', '#087c64'], [2, '--pp-upload', '#2861bd']];
        for (const [index, color, fallback] of series) {
            context.strokeStyle = style.getPropertyValue(color).trim() || fallback;
            context.lineWidth = 3;
            context.beginPath();
            let previousEnd = 0;
            for (const point of samples) {
                const [end, seconds] = point;
                const start = Math.max(since, end - seconds);
                const y = height - 2 - (height - 4) * (point[index] / seconds) / peak;
                if (start !== previousEnd)
                    context.moveTo(x(start), y);
                else
                    context.lineTo(x(start), y);
                context.lineTo(x(end), y);
                previousEnd = end;
            }
            context.stroke();
        }

        canvas.onmousemove = event => {
            const time = since + (event.offsetX / canvas.clientWidth) * 86400;
            const point = samples.find(([end, seconds]) => time >= end - seconds && time <= end);
            canvas.title = point
                ? new Date(point[0] * 1000).toLocaleString() + ' · ↑ ' +
                    trafficSize(point[2] / point[1]) + '/s · ↓ ' + trafficSize(point[3] / point[1]) + '/s'
                : '';
        };
    }

    async function refresh() {
        let samples = [];
        let now;
        try {
            const history = await callTrafficHistory();
            if (!Array.isArray(history.samples) || !Number.isInteger(history.now))
                throw new Error();
            ({ samples, now } = history);
            message.textContent = '等待流量采样';
        } catch (e) {
            message.textContent = '无法读取流量记录';
        }
        const available = samples.length > 0;
        upload.hidden = download.hidden = chart.hidden = !available;
        message.hidden = available;
        if (available) {
            upload.textContent = '↑ ' + trafficSize(samples.reduce((sum, point) => sum + point[2], 0));
            download.textContent = '↓ ' + trafficSize(samples.reduce((sum, point) => sum + point[3], 0));
            draw(samples, now);
        }
    }

    upload.hidden = download.hidden = true;
    void refresh();
    poll.add(refresh, 30);
    return E('div', { class: 'pp-traffic' }, [
        E('div', { class: 'pp-traffic-info' }, [
            E('h3', {}, [ '代理流量' ]), E('span', { class: 'pp-muted' }, [ '近 24 小时' ]),
            E('div', { class: 'pp-traffic-legend' }, [ upload, download, message ])
        ]),
        chart
    ]);
}

const SettingsSection = form.NamedSection.extend({
    renderContents: function(data) {
        const section = this.super('renderContents', [ data ]);
        const fields = section.querySelector('.cbi-section-node');
        if (!fields)
            return section;
        const groups = [
            [ 'DNS 上游', [ 'recursive', 'encrypted', 'custom_forward' ] ],
            [ '运行与缓存', [ 'enabled', 'memory_limit_mib', 'cache_file_mib' ] ]
        ];
        const cards = groups.map(([title, options]) => E('div', { class: 'pp-card pp-settings-card' }, [
            E('h3', {}, [ title ]), ...options.map(name => fields.querySelector('[data-name="' + name + '"]'))
        ].filter(Boolean)));
        fields.prepend(...cards);
        fields.classList.add('pp-settings-grid');
        return section;
    }
});

function createSettings() {
    const map = new form.Map('paopaodns-rs');
    const section = map.section(SettingsSection, 'main', 'paopaodns');
    const enabled = section.option(form.Flag, 'enabled', '开机自动启动');
    enabled.default = enabled.enabled;
    enabled.rmempty = false;

    const memory = section.option(form.Value, 'memory_limit_mib', '进程内存上限（MiB）',
        '虚拟地址空间上限。填 0 表示不限，或填 32–4096。');
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
        '用于国内域名和强制递归规则。未分类查询也先走这里，填 127.0.0.1:53 使用本机 dnsmasq。');
    recursive.placeholder = '127.0.0.1:5301';
    const encrypted = section.option(form.Value, 'encrypted', '加密上游',
        '用于强制加密规则，或自定义上游留空时的非 CN 查询。填 223.6.6.6:853 使用 DoT。');
    encrypted.placeholder = '127.0.0.1:5302';
    const custom = section.option(form.Value, 'custom_forward', '自定义上游（Fake-IP）',
        '用于非 CN 地址查询和指定上游规则。填 127.0.0.1:1020 接入 sing-box，留空走加密上游。');
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

        return E('div', { class: 'pp-families' }, [
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
        return createSettings().render().then(node => E('div', { class: 'pp-overview' }, [
            E('link', { rel: 'stylesheet', href: L.resource('view/paopaodns-rs/overview.css') }),
            renderService(writable), node
        ]));
    }
});

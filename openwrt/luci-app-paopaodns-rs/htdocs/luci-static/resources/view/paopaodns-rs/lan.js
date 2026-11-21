// SPDX-License-Identifier: GPL-3.0-only
'use strict';
'require view';
'require form';
'require rpc';
'require ui';

const readDirectClients = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'readDirectClients', expect: { '': {} }
});
const writeDirectClients = rpc.declare({
    object: 'luci.paopaodns.proxy', method: 'writeDirectClients',
    params: [ 'addresses' ], expect: { '': {} }
});

return view.extend({
    handleSave: null,
    handleReset: null,

    load: readDirectClients,

    render: function(data) {
        if (!data.ok)
            throw new Error(data.error || _('Unable to read access control'));

        const clients = data.addresses.map(address => ({ address }));
        const map = this.map = new form.JSONMap({ client: clients }, '局域网代理');
        map.readonly = !L.hasViewPermission();

        // JSONMap otherwise derives unnamed IDs from the current row count.
        const add = map.data.add.bind(map.data);
        let next = 0;
        map.data.add = (config, type, name) => add(config, type, name ?? ('new_client_' + next++));

        const section = map.section(form.TableSection, 'client', '访问控制');
        section.anonymous = true;
        section.addremove = true;
        const ip = section.option(form.Value, 'address', '不代理的客户端 IP');
        ip.datatype = 'ipaddr';
        ip.rmempty = false;
        return map.render();
    },

    handleSaveApply: async function() {
        if (this.map.readonly)
            return;
        await this.map.save();
        const addresses = this.map.data.sections(this.map.config, 'client').map(client => client.address);
        const reply = await writeDirectClients(addresses);
        if (!reply.ok)
            throw new Error(reply.error || _('Unable to apply access control'));
        ui.addNotification(null, E('p', {}, [ '访问控制已应用。' ]), 'info');
    }
});
# SPDX-License-Identifier: GPL-3.0-only
include $(TOPDIR)/rules.mk

LUCI_TITLE:=LuCI dashboard for PaoPaoDNS-RS
LUCI_DEPENDS:=+luci-base +rpcd-mod-ucode +curl +xz +ip-full +kmod-nft-tproxy +paopaodns-rs
LUCI_PKGARCH:=all
PKG_VERSION:=0.1.0
PKG_RELEASE:=24
PKG_LICENSE:=GPL-3.0-only

define Package/luci-app-paopaodns-rs/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
/etc/init.d/paopaodns-traffic enable
/etc/init.d/paopaodns-traffic start
rm -f /tmp/luci-indexcache.*
rm -rf /tmp/luci-modulecache/
/etc/init.d/rpcd reload >/dev/null 2>&1 || :
endef

include $(TOPDIR)/feeds/luci/luci.mk

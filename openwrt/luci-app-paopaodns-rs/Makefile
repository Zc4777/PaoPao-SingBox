# SPDX-License-Identifier: GPL-3.0-only
include $(TOPDIR)/rules.mk

LUCI_TITLE:=LuCI dashboard for PaoPaoDNS-RS
LUCI_DEPENDS:=+luci-base +rpcd-mod-ucode +curl +xz +ip-full +kmod-nft-tproxy +paopaodns-rs
LUCI_PKGARCH:=all
PKG_VERSION:=0.1.0
PKG_RELEASE:=21
PKG_LICENSE:=GPL-3.0-only

include $(TOPDIR)/feeds/luci/luci.mk

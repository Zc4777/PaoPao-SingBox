# SPDX-License-Identifier: GPL-3.0-only
include $(TOPDIR)/rules.mk

PKG_NAME:=paopaodns-rs
PKG_VERSION:=0.1.0
PKG_RELEASE:=11
PKG_LICENSE:=GPL-3.0-only
PKG_LICENSE_FILES:=LICENSE
PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/paopaodns-rs
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=IP Addresses and Names
  TITLE:=Rust DNS forwarding and policy service
  DEPENDS:=$(RUST_ARCH_DEPENDS) +firewall4
endef

define Package/paopaodns-rs/description
  Native DNS split routing with Unbound and encrypted DNS upstreams.
endef

define Build/Prepare
	@test -f "$(PAOPAODNS_SOURCE_DIR)/Cargo.toml" -a -n "$(PAOPAODNS_SOURCE_DIR)" || { echo "Set PAOPAODNS_SOURCE_DIR to the absolute project directory"; exit 1; }
	$(INSTALL_DIR) $(PKG_BUILD_DIR)
	$(CP) "$(PAOPAODNS_SOURCE_DIR)/Cargo.toml" "$(PAOPAODNS_SOURCE_DIR)/Cargo.lock" "$(PAOPAODNS_SOURCE_DIR)/LICENSE" "$(PKG_BUILD_DIR)/"
	$(CP) "$(PAOPAODNS_SOURCE_DIR)/src" "$(PKG_BUILD_DIR)/"
endef

define Package/paopaodns-rs/conffiles
/etc/config/paopaodns-rs
/etc/paopaodns-rs/force_forward_list.txt
/etc/paopaodns-rs/force_dnscrypt_list.txt
/etc/paopaodns-rs/force_recurse_list.txt
endef

define Package/paopaodns-rs/install
	$(INSTALL_DIR) $(1)/usr/sbin $(1)/etc/init.d $(1)/etc/config $(1)/etc/paopaodns-rs
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/paopaodns-rs $(1)/usr/sbin/
	$(INSTALL_BIN) ./files/paopaodns-rs.init $(1)/etc/init.d/paopaodns-rs
	$(INSTALL_CONF) ./files/paopaodns-rs.config $(1)/etc/config/paopaodns-rs
	$(INSTALL_DATA) ./files/force_forward_list.txt ./files/force_dnscrypt_list.txt ./files/force_recurse_list.txt $(1)/etc/paopaodns-rs/
endef

$(eval $(call BuildPackage,paopaodns-rs))

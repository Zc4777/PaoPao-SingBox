use crate::config::Config;
use hickory_proto::rr::Name;
use maxminddb::{Reader, geoip2};
use regex::{RegexSet, RegexSetBuilder};
use std::{collections::HashSet, fs, io, net::IpAddr, path::Path};

#[derive(Default)]
struct DomainSet {
    exact: HashSet<String>,
    suffixes: HashSet<String>,
    keywords: Vec<String>,
    regexes: Option<RegexSet>,
}

impl DomainSet {
    fn load(path: &Path, optional: bool) -> io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if optional && err.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!("{}: {err}", path.display()),
                ));
            }
        };
        Self::parse(&text).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {err}", path.display()),
            )
        })
    }

    fn parse(text: &str) -> Result<Self, String> {
        let mut set = Self::default();
        let mut patterns = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim_start_matches('\u{feff}').trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (kind, value) = line.split_once(':').unwrap_or(("domain", line));
            let value = value.trim();
            if value.is_empty() {
                return Err(format!("empty rule at line {}", index + 1));
            }
            match kind {
                "regexp" | "regex" => patterns.push(value),
                "keyword" => set.keywords.push(value.to_ascii_lowercase()),
                "domain" | "full" => {
                    let value = value.split_whitespace().next().unwrap();
                    let name = Name::from_utf8(value)
                        .map_err(|err| format!("invalid name on line {}: {err}", index + 1))?;
                    let name = normalize(&name);
                    if name.is_empty() {
                        return Err(format!("root domain is unsupported on line {}", index + 1));
                    }
                    if kind == "full" {
                        set.exact.insert(name);
                    } else {
                        set.suffixes.insert(name);
                    }
                }
                _ => return Err(format!("unknown rule type {kind} on line {}", index + 1)),
            }
        }
        if !patterns.is_empty() {
            set.regexes = Some(
                RegexSetBuilder::new(patterns)
                    .case_insensitive(true)
                    .build()
                    .map_err(|err| err.to_string())?,
            );
        }
        Ok(set)
    }

    fn matches(&self, name: &str) -> bool {
        let mut suffix = name;
        loop {
            if self.suffixes.contains(suffix) {
                return true;
            }
            match suffix.split_once('.') {
                Some((_, parent)) => suffix = parent,
                None => break,
            }
        }
        self.exact.contains(name)
            || self.keywords.iter().any(|word| name.contains(word))
            || self.regexes.as_ref().is_some_and(|set| set.is_match(name))
    }
}

fn normalize(name: &Name) -> String {
    name.to_ascii().trim_end_matches('.').to_ascii_lowercase()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Recursive,
    Encrypted,
    Custom,
    Auto,
}

pub struct Rules {
    force_forward: DomainSet,
    force_encrypted: DomainSet,
    force_recursive: DomainSet,
    global_mark: DomainSet,
    cn_mark: DomainSet,
    geo: Option<Reader<Vec<u8>>>,
}

impl Rules {
    pub fn load(config: &Config) -> io::Result<Self> {
        let from_dir = |file: &str| match config.rules_dir.as_deref() {
            Some(dir) => DomainSet::load(&dir.join(file), true),
            None => Ok(DomainSet::default()),
        };
        let from_file = |file: Option<&Path>| match file {
            Some(path) => DomainSet::load(path, false),
            None => Ok(DomainSet::default()),
        };
        let geo = config
            .mmdb
            .as_deref()
            .map(|path| {
                Reader::open_readfile(path).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{}: {err}", path.display()),
                    )
                })
            })
            .transpose()?;
        Ok(Self {
            force_forward: from_dir("force_forward_list.txt")?,
            force_encrypted: from_dir("force_dnscrypt_list.txt")?,
            force_recursive: from_dir("force_recurse_list.txt")?,
            global_mark: from_file(config.global_mark.as_deref())?,
            cn_mark: from_file(config.cn_mark.as_deref())?,
            geo,
        })
    }

    pub fn route(&self, name: &Name, config: &Config) -> Route {
        if config.recursive_only {
            return Route::Recursive;
        }
        let name = normalize(name);
        if config.custom_forward.is_some() && self.force_forward.matches(&name) {
            Route::Custom
        } else if self.force_encrypted.matches(&name) {
            Route::Encrypted
        } else if self.force_recursive.matches(&name) || self.cn_mark.matches(&name) {
            Route::Recursive
        } else if self.global_mark.matches(&name) {
            self.foreign_route(config)
        } else {
            Route::Auto
        }
    }

    pub fn foreign_route(&self, config: &Config) -> Route {
        if config.custom_forward.is_some() {
            Route::Custom
        } else {
            Route::Encrypted
        }
    }

    pub fn has_geo(&self) -> bool {
        self.geo.is_some()
    }

    pub fn local_address(&self, address: IpAddr) -> bool {
        match address {
            IpAddr::V4(ip) if ip.is_private() || ip.is_loopback() || ip.is_link_local() => {
                return true;
            }
            IpAddr::V6(ip)
                if ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local() =>
            {
                return true;
            }
            _ => {}
        }
        self.geo
            .as_ref()
            .and_then(|db| db.lookup::<geoip2::Country>(address).ok())
            .and_then(|data| data.country)
            .and_then(|country| country.iso_code)
            .is_some_and(|code| matches!(code, "CN" | "PRIVATE"))
    }
}

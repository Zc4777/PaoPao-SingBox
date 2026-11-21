use clap::{Parser, ValueEnum};
use hickory_proto::rr::RecordType;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Clone, Copy, Debug, Hash, ValueEnum)]
pub enum Ipv6 {
    No,
    Yes,
    Only6,
    YesOnly6,
    Raw,
}

#[derive(Clone, Copy, Debug, Hash, ValueEnum)]
pub enum CustomIp {
    Ipv4,
    Ipv6,
    Both,
}

impl CustomIp {
    pub fn allows(self, kind: RecordType) -> bool {
        match kind {
            RecordType::A => !matches!(self, Self::Ipv6),
            RecordType::AAAA => !matches!(self, Self::Ipv4),
            _ => true,
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Config {
    #[arg(long, default_value = "127.0.0.1:5354")]
    pub listen: SocketAddr,
    #[arg(long, default_value = "127.0.0.1:5301")]
    pub recursive: SocketAddr,
    #[arg(long, default_value = "127.0.0.1:5302")]
    pub encrypted: SocketAddr,
    #[arg(long)]
    pub custom_forward: Option<SocketAddr>,
    #[arg(long, value_enum, default_value = "ipv4")]
    pub custom_ip: CustomIp,
    #[arg(long)]
    pub cn_fallback: Option<SocketAddr>,
    #[arg(long, default_value = "600", value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub cache_max_ttl: u32,
    #[arg(long, default_value = "1024", value_parser = clap::value_parser!(u32).range(0..=16384))]
    pub cache_entries: u32,
    #[arg(long, default_value = "2000", value_parser = clap::value_parser!(u64).range(100..=30000))]
    pub timeout_ms: u64,
    #[arg(long, default_value = "64", value_parser = clap::value_parser!(u32).range(1..=1024))]
    pub concurrency: u32,
    #[arg(long, value_enum, default_value = "no")]
    pub ipv6: Ipv6,
    #[arg(long)]
    pub rules_dir: Option<PathBuf>,
    #[arg(long)]
    pub mmdb: Option<PathBuf>,
    #[arg(long)]
    pub global_mark: Option<PathBuf>,
    #[arg(long)]
    pub cn_mark: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub auto_forward: bool,
    #[arg(long)]
    pub recursive_only: bool,
    #[arg(long)]
    pub allow_service_records: bool,
    #[arg(long)]
    pub log_queries: bool,
    #[arg(long)]
    pub cache_file: Option<PathBuf>,
    #[arg(long, default_value = "4", value_parser = clap::value_parser!(u32).range(1..=32))]
    pub cache_file_mib: u32,
    #[arg(long, alias = "cache-stats")]
    pub stats_file: Option<PathBuf>,
    #[arg(long)]
    pub check: bool,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.auto_forward && self.custom_forward.is_none() {
            return Err("--auto-forward requires --custom-forward".into());
        }
        if self.global_mark.is_some() && self.cn_mark.is_none() {
            return Err("--global-mark requires --cn-mark".into());
        }
        let targets = [
            Some(self.recursive),
            Some(self.encrypted),
            self.custom_forward,
            self.cn_fallback,
        ];
        for target in targets.into_iter().flatten() {
            if target.port() == 0 || target.ip().is_unspecified() {
                return Err(format!("invalid upstream {target}"));
            }
            if target.port() == self.listen.port()
                && (target.ip() == self.listen.ip()
                    || (self.listen.ip().is_unspecified()
                        && (!self.listen.is_ipv4() || target.is_ipv4())))
            {
                return Err(format!("upstream {target} loops back to the listener"));
            }
        }
        Ok(())
    }
}

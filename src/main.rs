mod cache;
mod config;
mod dns;
mod rules;

use cache::Cache;
use clap::Parser;
use config::{Config, Ipv6};
use dns::{Answer, MAX_QUERY_SIZE};
use hickory_proto::{
    op::{Message, MessageType, OpCode, ResponseCode},
    rr::{DNSClass, RecordType},
};
use rules::{Route, Rules};
use std::{
    cell::RefCell,
    io,
    net::SocketAddr,
    path::Path,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpStream, UdpSocket},
    sync::Semaphore,
    task::LocalSet,
    time::timeout,
};

struct Server {
    config: Config,
    rules: Rules,
    cache: RefCell<Cache>,
    latency: RefCell<ResponseLatency>,
    prefetch_slots: Arc<Semaphore>,
}

#[derive(Default)]
struct ResponseLatency {
    replies: u64,
    total_us: u128,
}

impl ResponseLatency {
    fn record(&mut self, elapsed: Duration) {
        self.replies += 1;
        self.total_us += elapsed.as_micros();
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let average = self
            .total_us
            .checked_div(u128::from(self.replies))
            .map_or_else(|| "null".into(), |value| value.to_string());
        let json = format!(
            "{{\"pid\":{},\"replies\":{},\"avg_us\":{average}}}",
            std::process::id(),
            self.replies
        );
        cache::atomic_write(path, json.as_bytes(), false)
    }
}

impl Server {
    async fn respond(self: &Rc<Self>, packet: &[u8], udp: bool, client: SocketAddr) -> Option<Vec<u8>> {
        let Ok(request) = Message::from_vec(packet) else {
            return dns::malformed(packet);
        };
        let header = &request.metadata;
        if header.message_type != MessageType::Query {
            return None;
        }
        let question = match request.queries.as_slice() {
            [question] if header.op_code == OpCode::Query => question,
            _ if header.op_code != OpCode::Query => {
                return dns::response(&request, ResponseCode::NotImp).to_vec().ok();
            }
            _ => return dns::response(&request, ResponseCode::FormErr).to_vec().ok(),
        };
        if request.signature.is_some()
            || question.query_class != DNSClass::IN
            || matches!(question.query_type, RecordType::AXFR | RecordType::IXFR)
        {
            return dns::response(&request, ResponseCode::Refused).to_vec().ok();
        }
        if request
            .edns
            .as_ref()
            .is_some_and(|edns| edns.version() != 0)
        {
            return dns::response(&request, ResponseCode::BADVERS).to_vec().ok();
        }

        let started = Instant::now();
        let key = self.cache.borrow().key(&request, packet);
        let allow_stale = header.recursion_desired && !header.checking_disabled
            && !request.edns.as_ref().is_some_and(|edns| edns.flags().dnssec_ok);
        let cached = key.and_then(|key| self.cache.borrow_mut().get(key, header.id, allow_stale));
        let (wire, source) = if let Some(hit) = cached {
            if let Some(key) = key && hit.prefetch {
                self.prefetch(key, &request);
            }
            (hit.bytes, hit.source)
        } else {
            let result = self.resolve(&request).await;
            let fallback = key.and_then(|key| {
                let mut cache = self.cache.borrow_mut();
                if allow_stale && resolution_failed(&result) {
                    cache.stale_after_failure(key, header.id)
                } else {
                    self.remember_answer(&mut cache, key, &result);
                    None
                }
            });
            if let Some(hit) = fallback {
                (hit.bytes, hit.source)
            } else {
                match result {
                    Ok((answer, route)) => {
                        let source = match route {
                            Route::Recursive => "recursive",
                            Route::Encrypted => "encrypted",
                            Route::Custom => "custom",
                            Route::Auto => "auto",
                        };
                        (answer.bytes, source)
                    }
                    Err(code) => {
                        let source = if code == ResponseCode::ServFail { "error" } else { "policy" };
                        (dns::response(&request, code).to_vec().ok()?, source)
                    }
                }
            }
        };
        let wire = if udp {
            dns::fit_udp(&request, wire)?
        } else {
            wire
        };

        if self.config.log_queries {
            let transport = if udp { "UDP" } else { "TCP" };
            let code = wire[3] & 0x0f;
            println!(
                "query client={} transport={transport} name={:?} type={} source={source} rcode={code} ms={}",
                client.ip().to_canonical(),
                question.name.to_ascii(),
                question.query_type,
                started.elapsed().as_millis()
            );
        }
        Some(wire)
    }

    fn prefetch(self: &Rc<Self>, key: &[u8], request: &Message) {
        let Ok(permit) = self.prefetch_slots.clone().try_acquire_owned() else {
            return;
        };
        let Some(started) = self.cache.borrow_mut().begin_prefetch(key) else {
            return;
        };
        let server = self.clone();
        let key = key.to_vec();
        let request = request.clone();
        tokio::task::spawn_local(async move {
            let _permit = permit;
            let result = server.resolve(&request).await;
            let mut cache = server.cache.borrow_mut();
            if cache.finish_prefetch(&key, started, resolution_failed(&result)) {
                server.remember_answer(&mut cache, &key, &result);
            }
        });
    }

    fn remember_answer(&self, cache: &mut Cache, key: &[u8], result: &Result<(Answer, Route), ResponseCode>) {
        if resolution_failed(result) {
            return;
        }
        match result {
            Ok((answer, route)) => {
                let persistent = *route == Route::Recursive && answer.upstream == Some(self.config.recursive);
                cache.insert(key, answer, persistent);
            }
            Err(_) => cache.invalidate(key),
        }
    }

    async fn resolve(&self, request: &Message) -> Result<(Answer, Route), ResponseCode> {
        let kind = request.queries[0].query_type;
        let apply_policy = !self.config.recursive_only;
        if policy_blocks_query(&self.config, kind) {
            return Err(ResponseCode::NoError);
        }
        let (answer, route) = self.query(request).await?;

        if apply_policy && kind == RecordType::AAAA && has_records(&answer, RecordType::AAAA) {
            let encrypted = route == Route::Encrypted;
            if matches!(self.config.ipv6, Ipv6::Yes) && encrypted {
                return Err(ResponseCode::NoError);
            }
            if matches!(self.config.ipv6, Ipv6::Only6)
                || (matches!(self.config.ipv6, Ipv6::YesOnly6) && encrypted)
            {
                let mut ipv4 = request.clone();
                ipv4.queries[0].query_type = RecordType::A;
                if let Ok((v4, _)) = self.query(&ipv4).await {
                    if has_records(&v4, RecordType::A) {
                        return Err(ResponseCode::NoError);
                    }
                }
            }
        }
        Ok((answer, route))
    }

    async fn query(&self, request: &Message) -> Result<(Answer, Route), ResponseCode> {
        let config = &self.config;
        let question = &request.queries[0];
        let route = self.rules.route(&question.name, config);
        let limit = Duration::from_millis(config.timeout_ms);
        let via = |addr: SocketAddr| async move {
            dns::exchange(addr, request, limit)
                .await
                .map_err(|_| ResponseCode::ServFail)
        };
        let custom = || async {
            if !config.custom_ip.allows(question.query_type) {
                return Err(ResponseCode::NoError);
            }
            let answer = via(config.custom_forward.unwrap()).await?;
            Ok((answer, Route::Custom))
        };
        match route {
            Route::Recursive => return Ok((via(config.recursive).await?, route)),
            Route::Encrypted => return Ok((via(config.encrypted).await?, route)),
            Route::Custom => return custom().await,
            Route::Auto => {}
        }

        let mut local = via(config.recursive).await;
        if let Some(fallback) = config.cn_fallback {
            if !local
                .as_ref()
                .is_ok_and(|answer| !failed(answer.message.metadata.response_code))
            {
                if let Ok(response) = via(fallback).await {
                    local = Ok(response);
                }
            }
        }

        let kind = question.query_type;
        let ip_query = matches!(kind, RecordType::A | RecordType::AAAA);
        if let Ok(answer) = &local {
            if keeps_auto_reply(&self.rules, request, &answer.message) {
                return Ok((local?, Route::Recursive));
            }
            if kind == RecordType::AAAA
                && answer.message.metadata.response_code == ResponseCode::NoError
                && !has_records(answer, RecordType::AAAA)
            {
                let mut ipv4 = request.clone();
                ipv4.queries[0].query_type = RecordType::A;
                if let Ok(a) = dns::exchange(config.recursive, &ipv4, limit).await
                    && a.message.metadata.response_code == ResponseCode::NoError
                    && has_local_address(&self.rules, &a.message)
                {
                    return Ok((local?, Route::Recursive));
                }
            }
        }

        let route = if ip_query {
            self.rules.foreign_route(config)
        } else {
            Route::Encrypted
        };
        if route == Route::Custom {
            return custom().await;
        }
        Ok((via(config.encrypted).await?, route))
    }

    async fn serve_tcp(self: &Rc<Self>, mut socket: TcpStream, client: SocketAddr) -> io::Result<()> {
        let deadline = Duration::from_secs(10);
        while let Ok(Ok(packet)) =
            timeout(deadline, dns::read_frame(&mut socket, MAX_QUERY_SIZE)).await
        {
            let received = Instant::now();
            if let Some(reply) = self.respond(&packet, false, client).await {
                timeout(deadline, dns::write_frame(&mut socket, &reply)).await??;
                self.latency.borrow_mut().record(received.elapsed());
            }
        }
        Ok(())
    }
}

fn resolution_failed(result: &Result<(Answer, Route), ResponseCode>) -> bool {
    match result {
        Ok((answer, _)) => answer.message.metadata.response_code == ResponseCode::ServFail,
        Err(code) => *code == ResponseCode::ServFail,
    }
}

fn policy_blocks_query(config: &Config, kind: RecordType) -> bool {
    !config.recursive_only
        && ((matches!(config.ipv6, Ipv6::No) && kind == RecordType::AAAA)
            || (!config.allow_service_records && matches!(kind, RecordType::SVCB | RecordType::HTTPS)))
}

fn has_local_address(rules: &Rules, answer: &Message) -> bool {
    answer
        .answers
        .iter()
        .filter_map(|record| record.data.ip_addr())
        .any(|address| rules.local_address(address))
}

fn keeps_auto_reply(rules: &Rules, request: &Message, answer: &Message) -> bool {
    let ip_query = matches!(request.queries[0].query_type, RecordType::A | RecordType::AAAA);
    !failed(answer.metadata.response_code)
        && (!rules.has_geo()
            || !request.metadata.recursion_desired
            || if ip_query {
                has_local_address(rules, answer)
            } else {
                !answer.answers.is_empty()
            })
}

fn can_restore_recursive(
    config: &Config,
    rules: &Rules,
    request: &Message,
    answer: &Message,
) -> bool {
    let question = &request.queries[0];
    // Only6 may need a fresh A lookup to decide whether to return an AAAA answer.
    if policy_blocks_query(config, question.query_type)
        || (!config.recursive_only
            && matches!(config.ipv6, Ipv6::Only6)
            && question.query_type == RecordType::AAAA)
    {
        return false;
    }
    match rules.route(&question.name, config) {
        Route::Recursive => true,
        Route::Auto => keeps_auto_reply(rules, request, answer),
        Route::Encrypted | Route::Custom => false,
    }
}

fn has_records(answer: &Answer, kind: RecordType) -> bool {
    answer.message.metadata.response_code == ResponseCode::NoError
        && answer
            .message
            .answers
            .iter()
            .any(|record| record.record_type() == kind)
}

fn failed(code: ResponseCode) -> bool {
    !matches!(code, ResponseCode::NoError | ResponseCode::NXDomain)
}

async fn shutdown() -> io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = terminate.recv() => Ok(()),
            interrupt = tokio::signal::ctrl_c() => interrupt,
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    config.validate()?;
    let rules = Rules::load(&config)?;
    if config.check {
        println!("configuration and rules OK");
        return Ok(());
    }
    let tcp = TcpListener::bind(config.listen).await?;
    let listen = tcp.local_addr()?;
    let udp = Arc::new(UdpSocket::bind(listen).await?);
    let udp_slots = Arc::new(Semaphore::new(config.concurrency as usize));
    let tcp_slots = Arc::new(Semaphore::new(config.concurrency as usize));
    let mut cache = Cache::new(config.cache_entries, config.cache_max_ttl);
    let cache_file = config
        .cache_file
        .clone()
        .filter(|_| config.cache_entries > 0);
    let cache_file_limit = config.cache_file_mib as usize * 1024 * 1024;
    let stats_file = config.stats_file.clone();
    let restored = match cache_file.as_ref().map(|path| {
        cache.restore(path, &config, cache_file_limit, |request, answer| {
            can_restore_recursive(&config, &rules, request, answer)
        })
    }) {
        Some(Ok(restored)) => restored,
        Some(Err(err)) if err.kind() != io::ErrorKind::NotFound => {
            eprintln!("无法读取磁盘 DNS 缓存。{err}");
            0
        }
        _ => 0,
    };
    println!("从硬盘加载了 {restored} 条 DNS 缓存");
    let server = Rc::new(Server {
        cache: RefCell::new(cache),
        latency: RefCell::default(),
        prefetch_slots: Arc::new(Semaphore::new((config.concurrency as usize / 4).clamp(1, 4))),
        config,
        rules,
    });
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_save = Instant::now();
    let mut buffer = [0u8; MAX_QUERY_SIZE + 1];
    println!("paopaodns-rs listening on {listen} (UDP/TCP)");
    let stop = shutdown();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            signal = &mut stop => {
                if let Some(path) = &cache_file
                    && let Err(err) = server.cache.borrow_mut().save(path, server.config.recursive, cache_file_limit)
                {
                    eprintln!("could not save DNS cache: {err}");
                }
                return Ok(signal?);
            }
            _ = maintenance.tick() => {
                dns::prune_idle_tls();
                if last_save.elapsed() >= Duration::from_secs(60) {
                    if let Some(path) = &cache_file
                        && let Err(err) = server.cache.borrow_mut().save(path, server.config.recursive, cache_file_limit)
                    {
                        eprintln!("could not save DNS cache: {err}");
                    }
                    last_save = Instant::now();
                }
                if let Some(path) = &stats_file {
                    let _ = server.latency.borrow().write(path);
                }
            }
            frame = udp.recv_from(&mut buffer) => {
                let (size, client) = frame?;
                let received = Instant::now();
                if size <= MAX_QUERY_SIZE {
                    if let Ok(permit) = udp_slots.clone().try_acquire_owned() {
                        let server = server.clone();
                        let udp = udp.clone();
                        let packet = buffer[..size].to_vec();
                        tokio::task::spawn_local(async move {
                            let _permit = permit;
                            if let Some(reply) = server.respond(&packet, true, client).await
                                && udp.send_to(&reply, client).await.is_ok()
                            {
                                server.latency.borrow_mut().record(received.elapsed());
                            }
                        });
                    }
                }
            }
            accepted = tcp.accept() => {
                let (socket, client) = accepted?;
                if let Ok(permit) = tcp_slots.clone().try_acquire_owned() {
                    let server = server.clone();
                    tokio::task::spawn_local(async move {
                        let _permit = permit;
                        let _ = server.serve_tcp(socket, client).await;
                    });
                }
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalSet::new().run_until(run(Config::parse())).await
}

#[cfg(test)]
mod tests;

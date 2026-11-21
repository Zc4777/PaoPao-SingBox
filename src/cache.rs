use crate::{
    config::Config,
    dns::{Answer, MAX_QUERY_SIZE},
};
use hickory_proto::op::{Message, ResponseCode};
use lru::LruCache;
use std::{
    fs::{self, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr},
    num::NonZeroUsize,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct Cache {
    items: Option<LruCache<Vec<u8>, Entry>>,
    max_ttl: u32,
    dirty: bool,
}

const MAX_STALE_AGE: Duration = Duration::from_secs(300);
const STALE_REPLY_TTL: Duration = Duration::from_secs(5);
const MIN_PREFETCH_TTL: Duration = Duration::from_secs(10);

pub struct CacheHit {
    pub bytes: Vec<u8>,
    pub source: &'static str,
    pub prefetch: bool,
}

struct Entry {
    inserted: Instant,
    ttl: Duration,
    message: Message,
    persistent: bool,
    restored: bool,
    refreshable: bool,
    prefetched: bool,
    retry_after: Option<Instant>,
}

impl Entry {
    fn prefetch_due(&self, elapsed: Duration) -> bool {
        self.refreshable && !self.prefetched && self.ttl >= MIN_PREFETCH_TTL
            && elapsed < self.ttl && self.ttl - elapsed <= self.ttl / 10
    }

    fn stale_answer(&self, id: u16, elapsed: Duration) -> Option<CacheHit> {
        if !self.refreshable || elapsed < self.ttl {
            return None;
        }
        let ttl = (self.ttl + MAX_STALE_AGE)
            .saturating_sub(elapsed)
            .min(STALE_REPLY_TTL)
            .as_secs() as u32;
        if ttl == 0 {
            return None;
        }
        let mut reply = self.message.clone();
        reply.metadata.id = id;
        for record in reply.answers.iter_mut()
            .chain(&mut reply.authorities)
            .chain(&mut reply.additionals)
        {
            record.ttl = ttl;
        }
        Some(CacheHit {
            bytes: reply.to_vec().ok()?,
            source: "stale-cache",
            prefetch: false,
        })
    }
}

impl Cache {
    pub fn new(entries: u32, max_ttl: u32) -> Self {
        Self {
            items: NonZeroUsize::new(entries as usize).map(LruCache::new),
            max_ttl,
            dirty: false,
        }
    }

    pub fn key<'a>(&self, request: &Message, packet: &'a [u8]) -> Option<&'a [u8]> {
        if self.items.is_none()
            || !request.additionals.is_empty()
            || request
                .edns
                .as_ref()
                .is_some_and(|edns| !edns.options().options.is_empty())
        {
            return None;
        }
        packet.get(2..)
    }

    pub fn get(&mut self, key: &[u8], id: u16, allow_stale: bool) -> Option<CacheHit> {
        let items = self.items.as_mut()?;
        let entry = items.get(key)?;
        let now = Instant::now();
        let elapsed = now.duration_since(entry.inserted);
        if elapsed >= entry.ttl {
            if !entry.refreshable || elapsed >= entry.ttl + MAX_STALE_AGE {
                items.pop(key);
                return None;
            }
            return (allow_stale && entry.retry_after.is_some_and(|retry| now < retry))
                .then(|| entry.stale_answer(id, elapsed))
                .flatten();
        }
        let mut reply = entry.message.clone();
        reply.metadata.id = id;
        let reply = age_records(reply, elapsed.as_secs() as u32);
        Some(CacheHit {
            bytes: reply.to_vec().ok()?,
            source: if entry.restored { "disk-cache" } else { "cache" },
            prefetch: entry.prefetch_due(elapsed),
        })
    }

    pub fn stale_after_failure(&mut self, key: &[u8], id: u16) -> Option<CacheHit> {
        let entry = self.items.as_mut()?.get_mut(key)?;
        let now = Instant::now();
        let answer = entry.stale_answer(id, now.duration_since(entry.inserted))?;
        entry.retry_after = Some(now + STALE_REPLY_TTL);
        Some(answer)
    }

    pub fn begin_prefetch(&mut self, key: &[u8]) -> Option<Instant> {
        let entry = self.items.as_mut()?.get_mut(key)?;
        if !entry.prefetch_due(entry.inserted.elapsed()) {
            return None;
        }
        entry.prefetched = true;
        Some(entry.inserted)
    }

    pub fn prefetch_is_current(&self, key: &[u8], started: Instant) -> bool {
        self.items.as_ref().and_then(|items| items.peek(key))
            .is_some_and(|entry| entry.inserted == started)
    }

    pub fn invalidate(&mut self, key: &[u8]) {
        if let Some(entry) = self.items.as_mut().and_then(|items| items.pop(key)) {
            self.dirty |= entry.persistent;
        }
    }

    pub fn insert(&mut self, key: &[u8], answer: &Answer, persistent: bool) {
        self.invalidate(key);
        if answer.bytes.len() <= MAX_QUERY_SIZE {
            self.put(key.to_vec(), answer.message.clone(), persistent, false);
        }
    }

    #[cfg(test)]
    pub fn set_entry_age(&mut self, key: &[u8], age: Duration) {
        self.items.as_mut().unwrap().get_mut(key).unwrap().inserted = Instant::now() - age;
    }

    fn put(&mut self, key: Vec<u8>, message: Message, persistent: bool, restored: bool) {
        let Some(items) = &mut self.items else { return };
        if message.metadata.message_type != hickory_proto::op::MessageType::Response
            || message.metadata.response_code != ResponseCode::NoError
            || message.metadata.truncation
            || message.answers.is_empty()
            || message
                .edns
                .as_ref()
                .is_some_and(|edns| !edns.options().options.is_empty())
        {
            return;
        }
        let ttl = message
            .answers
            .iter()
            .chain(&message.authorities)
            .chain(&message.additionals)
            .map(|record| record.ttl)
            .min()
            .unwrap_or_default()
            .min(self.max_ttl);
        if ttl > 0 {
            items.put(
                key,
                Entry {
                    inserted: Instant::now(),
                    ttl: Duration::from_secs(u64::from(ttl)),
                    refreshable: persistent && is_public_address_reply(&message),
                    message,
                    persistent,
                    restored,
                    prefetched: false,
                    retry_after: None,
                },
            );
            if persistent && !restored {
                self.dirty = true;
            }
        }
    }

    pub fn restore(
        &mut self,
        path: &Path,
        config: &Config,
        max_bytes: usize,
        accepts: impl Fn(&Message, &Message) -> bool,
    ) -> io::Result<usize> {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut bytes)?;
        let oversized = bytes.len() > max_bytes;
        bytes.truncate(max_bytes);
        let restored = self.restore_snapshot(&bytes, config, oversized, accepts)?;
        if oversized {
            self.dirty = true;
            self.save(path, config.recursive, max_bytes)?;
        }
        Ok(restored)
    }

    fn restore_snapshot(
        &mut self,
        bytes: &[u8],
        config: &Config,
        oversized: bool,
        accepts: impl Fn(&Message, &Message) -> bool,
    ) -> io::Result<usize> {
        let (magic, mut input) = bytes.split_at_checked(8).ok_or_else(invalid_snapshot)?;
        let legacy = magic == LEGACY_MAGIC;
        let legacy_stamp = if legacy {
            Some(take_u64(&mut input).ok_or_else(invalid_snapshot)?)
        } else if magic == MAGIC {
            let upstream = take_packet(&mut input)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .and_then(|addr| addr.parse::<SocketAddr>().ok())
                .ok_or_else(invalid_snapshot)?;
            if upstream != config.recursive {
                return Ok(0);
            }
            None
        } else {
            return Err(invalid_snapshot());
        };
        let saved_at = take_u64(&mut input).ok_or_else(invalid_snapshot)?;
        let Some(age) = unix_time()?.checked_sub(saved_at) else {
            return Ok(0);
        };
        if age >= u64::from(self.max_ttl)
            || legacy_stamp.is_some_and(|stamp| stamp != legacy_policy_stamp(config))
        {
            return Ok(0);
        }
        let persistent = !legacy || config.cn_fallback.is_none();
        let mut records = Vec::new();
        while !input.is_empty() {
            let Some((key, bytes)) = take_packet(&mut input)
                .and_then(|key| take_packet(&mut input).map(|bytes| (key, bytes)))
            else {
                if oversized {
                    break;
                }
                return Err(invalid_snapshot());
            };
            if let (Ok(query), Ok(answer)) = (Message::from_vec(key), Message::from_vec(bytes)) {
                if query.queries.len() == 1
                    && answer.queries == query.queries
                    && (legacy || accepts(&query, &answer))
                    && let Some(key) = self.key(&query, key)
                {
                    records.push((key.to_vec(), age_records(answer, age as u32)));
                }
            }
        }
        for (key, reply) in records.into_iter().rev() {
            self.put(key, reply, persistent, true);
        }
        let restored = self.items.as_ref().map_or(0, LruCache::len);
        if legacy && persistent && restored > 0 {
            self.dirty = true;
        }
        Ok(restored)
    }

    pub fn save(&mut self, path: &Path, upstream: SocketAddr, max_bytes: usize) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let Some(items) = &self.items else {
            return Ok(());
        };
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        append_packet(&mut data, upstream.to_string().as_bytes());
        data.extend_from_slice(&unix_time()?.to_be_bytes());
        for (key, entry) in items.iter() {
            let elapsed = entry.inserted.elapsed();
            let age = elapsed.as_secs() + u64::from(elapsed.subsec_nanos() > 0);
            if !entry.persistent || age >= entry.ttl.as_secs() {
                continue;
            }
            let mut reply = age_records(entry.message.clone(), age as u32);
            let remaining = (entry.ttl.as_secs() - age) as u32;
            for record in reply
                .answers
                .iter_mut()
                .chain(&mut reply.authorities)
                .chain(&mut reply.additionals)
            {
                record.ttl = record.ttl.min(remaining);
            }
            let Ok(reply) = reply.to_vec() else { continue };
            if reply.len() > MAX_QUERY_SIZE {
                continue;
            }
            if data.len() + key.len() + reply.len() + 6 > max_bytes {
                break;
            }
            data.extend_from_slice(&((key.len() + 2) as u16).to_be_bytes());
            data.extend_from_slice(&[0, 0]);
            data.extend_from_slice(key);
            append_packet(&mut data, &reply);
        }
        atomic_write(path, &data, true)?;
        self.dirty = false;
        Ok(())
    }
}

const LEGACY_MAGIC: &[u8; 8] = b"PPDNS001";
const MAGIC: &[u8; 8] = b"PPDNS002";

pub(crate) fn legacy_policy_stamp(config: &Config) -> u64 {
    let mut hash = DefaultHasher::new();
    (
        config.recursive,
        config.encrypted,
        config.custom_forward,
        config.cn_fallback,
        config.ipv6,
        config.recursive_only,
        config.allow_service_records,
        config.cache_max_ttl,
        config.custom_ip,
    )
        .hash(&mut hash);
    for path in [&config.mmdb, &config.global_mark, &config.cn_mark] {
        path.as_ref().and_then(|p| fs::read(p).ok()).hash(&mut hash);
    }
    for file in [
        "force_forward_list.txt",
        "force_dnscrypt_list.txt",
        "force_recurse_list.txt",
    ] {
        config
            .rules_dir
            .as_ref()
            .and_then(|dir| fs::read(dir.join(file)).ok())
            .hash(&mut hash);
    }
    hash.finish()
}

fn is_public_address_reply(message: &Message) -> bool {
    let Some(question) = message.queries.first() else {
        return false;
    };
    if !question.query_type.is_ip_addr()
        || question.name.num_labels() < 2
        || message.metadata.authentic_data
        || message.edns.as_ref().is_some_and(|edns| edns.flags().dnssec_ok)
    {
        return false;
    }
    let name = question.name.to_ascii().to_ascii_lowercase();
    if [".lan.", ".local.", ".internal.", ".home.arpa."].iter().any(|suffix| name.ends_with(suffix)) {
        return false;
    }
    let records = message.answers.iter().chain(&message.authorities).chain(&message.additionals);
    message.answers.iter().any(|record| record.record_type() == question.query_type)
        && records.clone().all(|record| !record.record_type().is_dnssec())
        && records.filter_map(|record| record.data.ip_addr()).all(public_ip)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, _, _] = ip.octets();
            // 7/8 and 198.18/15 are commonly configured as Fake-IP ranges.
            !ip.is_private() && !ip.is_loopback() && !ip.is_link_local()
                && !matches!(first, 0 | 7 | 224..=255)
                && !(first == 100 && (64..=127).contains(&second))
                && !(first == 198 && matches!(second, 18 | 19))
        }
        IpAddr::V6(ip) => ip.segments()[0] & 0xe000 == 0x2000,
    }
}

fn age_records(mut message: Message, age: u32) -> Message {
    for record in message
        .answers
        .iter_mut()
        .chain(&mut message.authorities)
        .chain(&mut message.additionals)
    {
        record.ttl = record.ttl.saturating_sub(age);
    }
    message
}

fn unix_time() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(io::Error::other)
}

fn take_u64(input: &mut &[u8]) -> Option<u64> {
    let (head, tail) = input.split_at_checked(8)?;
    *input = tail;
    Some(u64::from_be_bytes(head.try_into().ok()?))
}

pub(crate) fn append_packet(output: &mut Vec<u8>, packet: &[u8]) {
    output.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    output.extend_from_slice(packet);
}

fn take_packet<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (length, tail) = input.split_at_checked(2)?;
    let len = u16::from_be_bytes(length.try_into().ok()?) as usize;
    if len > MAX_QUERY_SIZE {
        return None;
    }
    let (packet, tail) = tail.split_at_checked(len)?;
    *input = tail;
    Some(packet)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        if durable {
            file.sync_all()?;
        }
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        if durable
            && let Some(directory) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::File::open(directory)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn invalid_snapshot() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid DNS cache snapshot")
}

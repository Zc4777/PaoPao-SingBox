use super::*;
use hickory_proto::{
    op::{Query, ResponseCode},
    rr::{Name, Record, rdata::A},
};
use std::{cell::Cell, net::Ipv4Addr};

async fn fake_upstream(ip: Ipv4Addr) -> (SocketAddr, Rc<RefCell<usize>>, Rc<Cell<ResponseCode>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let count = Rc::new(RefCell::new(0));
    let calls = count.clone();
    let status = Rc::new(Cell::new(ResponseCode::NoError));
    let response_code = status.clone();
    tokio::task::spawn_local(async move {
        let mut buf = [0u8; 4096];
        loop {
            let (bytes, peer) = socket.recv_from(&mut buf).await.unwrap();
            *calls.borrow_mut() += 1;
            let request = Message::from_vec(&buf[..bytes]).unwrap();
            let code = response_code.get();
            let mut reply = dns::response(&request, code);
            if code != ResponseCode::NoError {
                socket.send_to(&reply.to_vec().unwrap(), peer).await.unwrap();
                continue;
            }
            let question = &request.queries[0];
            let record = if question.query_type == RecordType::AAAA {
                hickory_proto::rr::RData::AAAA(hickory_proto::rr::rdata::AAAA(
                    "fd00::42".parse().unwrap(),
                ))
            } else {
                hickory_proto::rr::RData::A(A(ip))
            };
            reply
                .answers
                .push(Record::from_rdata(question.name.clone(), 60, record));
            socket
                .send_to(&reply.to_vec().unwrap(), peer)
                .await
                .unwrap();
        }
    });
    (addr, count, status)
}

fn request(name: &str, kind: RecordType, id: u16) -> Vec<u8> {
    let mut query = Message::new(id, MessageType::Query, OpCode::Query);
    query.metadata.recursion_desired = true;
    query
        .queries
        .push(Query::query(Name::from_ascii(name).unwrap(), kind));
    query.to_vec().unwrap()
}

#[tokio::test]
async fn forced_upstreams_cache_and_ipv6() {
    LocalSet::new()
        .run_until(async {
            let (recursive, local_calls, _) = fake_upstream(Ipv4Addr::new(192, 0, 2, 1)).await;
            let (encrypted, encrypted_calls, _) = fake_upstream(Ipv4Addr::new(192, 0, 2, 2)).await;
            let (custom, custom_calls, _) = fake_upstream(Ipv4Addr::new(192, 0, 2, 3)).await;
            let path = std::env::temp_dir().join(format!(
                "paopaodns-test-{}-{}",
                std::process::id(),
                recursive.port()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(
                path.join("force_forward_list.txt"),
                "full:www.example.net\n",
            )
            .unwrap();
            std::fs::write(path.join("force_dnscrypt_list.txt"), "domain:example.net\n").unwrap();
            let config = Config::parse_from([
                "paopaodns-rs",
                "--recursive",
                &recursive.to_string(),
                "--encrypted",
                &encrypted.to_string(),
                "--custom-forward",
                &custom.to_string(),
                "--rules-dir",
                path.to_str().unwrap(),
            ]);
            let rules = Rules::load(&config).unwrap();
            let server = Rc::new(Server {
                config,
                rules,
                cache: RefCell::new(Cache::new(16, 60)),
                latency: RefCell::default(),
                prefetch_slots: Arc::new(Semaphore::new(1)),
            });
            for (name, expected) in [
                ("www.example.net.", Ipv4Addr::new(192, 0, 2, 3)),
                ("test.example.net.", Ipv4Addr::new(192, 0, 2, 2)),
                ("badexample.net.", Ipv4Addr::new(192, 0, 2, 1)),
            ] {
                for id in [10, 20] {
                    let result = server
                        .respond(
                            &request(name, RecordType::A, id),
                            true,
                            ([127, 0, 0, 1], 0).into(),
                        )
                        .await
                        .unwrap();
                    let response = Message::from_vec(&result).unwrap();
                    assert_eq!(response.metadata.id, id);
                    assert_eq!(response.answers[0].data.ip_addr(), Some(expected.into()));
                }
            }
            let result = server
                .respond(
                    &request("example.net.", RecordType::AAAA, 30),
                    true,
                    ([127, 0, 0, 1], 0).into(),
                )
                .await
                .unwrap();
            let response = Message::from_vec(&result).unwrap();
            assert_eq!(response.metadata.response_code, ResponseCode::NoError);
            assert!(response.answers.is_empty());
            assert_eq!(*local_calls.borrow(), 1);
            assert_eq!(*encrypted_calls.borrow(), 1);
            assert_eq!(*custom_calls.borrow(), 1);
            for file in ["force_forward_list.txt", "force_dnscrypt_list.txt"] {
                std::fs::remove_file(path.join(file)).unwrap();
            }
            std::fs::remove_dir(path).unwrap();
        })
        .await;
}

#[tokio::test]
async fn refreshes_hot_and_expired_addresses_without_blocking_clients() {
    LocalSet::new().run_until(async {
        let (upstream, calls, status) = fake_upstream(Ipv4Addr::new(8, 8, 4, 4)).await;
        let config = Config::parse_from(["test", "--recursive", &upstream.to_string(), "--recursive-only"]);
        let server = Rc::new(Server {
            rules: Rules::load(&config).unwrap(),
            config,
            cache: RefCell::new(Cache::new(16, 600)),
            latency: RefCell::default(),
            prefetch_slots: Arc::new(Semaphore::new(1)),
        });
        let packet = request("cache.example.", RecordType::A, 41);
        let key = &packet[2..];
        let client = ([127, 0, 0, 1], 0).into();
        let resolve = || async {
            let reply = server.respond(&packet, true, client).await.unwrap();
            Message::from_vec(&reply).unwrap()
        };

        assert_eq!(resolve().await.answers[0].ttl, 60);
        server.cache.borrow_mut().set_entry_age(key, Duration::from_secs(56));
        for _ in 0..2 {
            assert!(resolve().await.answers[0].ttl <= 4);
        }
        assert_eq!(*calls.borrow(), 1);
        timeout(Duration::from_secs(1), async {
            while server.prefetch_slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert_eq!(*calls.borrow(), 2);
        assert!(resolve().await.answers[0].ttl >= 59);

        server.cache.borrow_mut().set_entry_age(key, Duration::from_secs(61));
        for _ in 0..2 {
            assert_eq!(resolve().await.answers[0].ttl, 5);
        }
        assert_eq!(*calls.borrow(), 2);
        let wait_for_refresh = || async {
            timeout(Duration::from_secs(1), async {
                while server.prefetch_slots.available_permits() == 0 {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
        };
        wait_for_refresh().await;
        assert_eq!(*calls.borrow(), 3);
        assert!(resolve().await.answers[0].ttl >= 59);

        status.set(ResponseCode::ServFail);
        server.cache.borrow_mut().set_entry_age(key, Duration::from_secs(61));
        for _ in 0..2 {
            let stale = resolve().await;
            assert_eq!(stale.metadata.response_code, ResponseCode::NoError);
            assert_eq!(stale.answers[0].ttl, 5);
        }
        assert_eq!(*calls.borrow(), 3);
        wait_for_refresh().await;
        assert_eq!(*calls.borrow(), 4);
        assert_eq!(resolve().await.answers[0].ttl, 5);
        assert_eq!(server.prefetch_slots.available_permits(), 1);

        server.cache.borrow_mut().set_entry_age(key, Duration::from_secs(361));
        assert_eq!(resolve().await.metadata.response_code, ResponseCode::ServFail);

        status.set(ResponseCode::NoError);
        resolve().await;
        server.cache.borrow_mut().set_entry_age(key, Duration::from_secs(61));
        status.set(ResponseCode::NXDomain);
        assert_eq!(resolve().await.answers[0].ttl, 5);
        wait_for_refresh().await;
        status.set(ResponseCode::ServFail);
        assert_eq!(resolve().await.metadata.response_code, ResponseCode::ServFail);
    }).await;
}

#[test]
fn local_and_fake_addresses_are_ineligible_for_stale_cache() {
    for ip in [Ipv4Addr::new(10, 10, 10, 1), Ipv4Addr::new(7, 0, 0, 22), Ipv4Addr::new(198, 18, 0, 1)] {
        let packet = request("cache.example.", RecordType::A, 41);
        let query = Message::from_vec(&packet).unwrap();
        let mut message = dns::response(&query, ResponseCode::NoError);
        message.answers.push(Record::from_rdata(
            query.queries[0].name.clone(), 60, hickory_proto::rr::RData::A(A(ip)),
        ));
        let answer = Answer { bytes: message.to_vec().unwrap(), message, upstream: None };
        let mut cache = Cache::new(16, 600);
        let key = cache.key(&query, &packet).unwrap();
        cache.insert(key, &answer, true);
        cache.set_entry_age(key, Duration::from_secs(56));
        assert!(!cache.get(key, 41, true).unwrap().prefetch);
        cache.set_entry_age(key, Duration::from_secs(61));
        assert!(cache.get(key, 41, true).is_none());
        assert!(cache.stale_after_failure(key, 41).is_none());
    }
}

#[tokio::test]
async fn udp_truncation_retries_over_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = listener.local_addr().unwrap();
    let udp = UdpSocket::bind(upstream).await.unwrap();
    let callback = async {
        let mut buf = [0u8; 4096];
        let (bytes, peer) = udp.recv_from(&mut buf).await.unwrap();
        let q = Message::from_vec(&buf[..bytes]).unwrap();
        let mut r = dns::response(&q, ResponseCode::NoError);
        r.metadata.truncation = true;
        udp.send_to(&r.to_vec().unwrap(), peer).await.unwrap();
        let (mut tcp, _) = listener.accept().await.unwrap();
        let q = Message::from_vec(&dns::read_frame(&mut tcp, 4096).await.unwrap()).unwrap();
        let mut r = dns::response(&q, ResponseCode::NoError);
        r.answers.push(Record::from_rdata(
            q.queries[0].name.clone(),
            60,
            hickory_proto::rr::RData::A(A(Ipv4Addr::new(192, 0, 2, 99))),
        ));
        dns::write_frame(&mut tcp, &r.to_vec().unwrap())
            .await
            .unwrap();
    };
    let query = Message::from_vec(&request("example.com.", RecordType::A, 42)).unwrap();
    let (answer, ()) = tokio::join!(
        dns::exchange(upstream, &query, Duration::from_secs(1)),
        callback
    );
    let answer = answer.unwrap().message;
    assert_eq!(answer.metadata.id, 42);
    assert!(!answer.metadata.truncation);
    assert_eq!(answer.answers.len(), 1);
}

#[test]
fn persistent_cache_respects_ttl_and_policy() {
    let directory = std::env::temp_dir().join(format!("paopaodns-cache-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("cache.bin");
    let query_wire = request("dns.example.", RecordType::A, 10);
    let query = Message::from_vec(&query_wire).unwrap();
    let mut response = dns::response(&query, ResponseCode::NoError);
    response.answers.push(Record::from_rdata(
        query.queries[0].name.clone(),
        120,
        hickory_proto::rr::RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
    ));
    let answer = Answer {
        bytes: response.to_vec().unwrap(),
        message: response,
        upstream: None,
    };
    let config = Config::parse_from(["test"]);
    let mut cache = Cache::new(16, 8).with_disk(4096, config.recursive);
    let key = cache.key(&query, &query_wire).unwrap();
    cache.insert(key, &answer, true);
    cache.save(&path, config.recursive).unwrap();

    let mut miss = Cache::new(16, 8);
    let changed_upstream = Config::parse_from(["test", "--recursive", "127.0.0.1:53"]);
    assert_eq!(miss.restore(&path, &changed_upstream, 4096, |_, _| true).unwrap(), 0);
    let mut data = std::fs::read(&path).unwrap();
    let time_offset = 10 + config.recursive.to_string().len();
    let saved_at = time_offset..time_offset + 8;
    let saved = u64::from_be_bytes(data[saved_at.clone()].try_into().unwrap());
    data[saved_at.clone()].copy_from_slice(&(saved - 3).to_be_bytes());
    std::fs::write(&path, &data).unwrap();
    let mut restored = Cache::new(16, 8).with_disk(4096, config.recursive);
    assert_eq!(restored.restore(&path, &config, 4096, |_, _| true).unwrap(), 1);
    let hit = restored.get(key, 22, true).unwrap();
    assert_eq!(hit.source, "disk-cache");
    let reply = Message::from_vec(&hit.bytes).unwrap();
    assert_eq!(reply.metadata.id, 22);
    assert!((1..=5).contains(&reply.answers[0].ttl));

    // Save an already expired record and verify its age survives a restart.
    cache.insert(key, &answer, true);
    cache.set_entry_age(key, Duration::from_secs(20));
    cache.save(&path, config.recursive).unwrap();
    let mut data = std::fs::read(&path).unwrap();
    let saved = u64::from_be_bytes(data[saved_at.clone()].try_into().unwrap());
    let mut optimistic = Cache::new(16, 8).with_disk(4096, config.recursive);
    assert_eq!(optimistic.restore(&path, &config, 4096, |_, _| true).unwrap(), 1);
    assert!(optimistic.get(key, 22, false).is_none());
    let hit = optimistic.get(key, 22, true).unwrap();
    assert_eq!(hit.source, "optimistic-cache");
    assert!(hit.prefetch);
    assert_eq!(Message::from_vec(&hit.bytes).unwrap().answers[0].ttl, 5);

    data[saved_at].copy_from_slice(&(saved - 290).to_be_bytes());
    std::fs::write(&path, &data).unwrap();
    let mut expired = Cache::new(16, 8);
    assert_eq!(expired.restore(&path, &config, 4096, |_, _| true).unwrap(), 0);
    assert!(expired.get(key, 22, true).is_none());
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn legacy_cache_migrates_and_rule_changes_only_discard_affected_entries() {
    let directory = std::env::temp_dir().join(format!("paopaodns-cache-migration-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("cache.bin");
    let config = Config::parse_from([
        "test", "--ipv6", "raw", "--rules-dir", directory.to_str().unwrap(),
    ]);
    let rules = Rules::load(&config).unwrap();
    let mut legacy = b"PPDNS001".to_vec();
    legacy.extend_from_slice(&cache::legacy_policy_stamp(&config).to_be_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    legacy.extend_from_slice(&now.to_be_bytes());

    for domain in ["keep.example.", "changed.example."] {
        let query_wire = request(domain, RecordType::A, 0);
        let query = Message::from_vec(&query_wire).unwrap();
        let mut answer = dns::response(&query, ResponseCode::NoError);
        answer.answers.push(Record::from_rdata(
            query.queries[0].name.clone(),
            120,
            hickory_proto::rr::RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
        ));
        cache::append_packet(&mut legacy, &query_wire);
        cache::append_packet(&mut legacy, &answer.to_vec().unwrap());
    }

    std::fs::write(&path, &legacy).unwrap();
    let mut migrated = Cache::new(16, 600).with_disk(4096, config.recursive);
    assert_eq!(migrated.restore(&path, &config, 4096, |q, a| can_restore_recursive(&config, &rules, q, a)).unwrap(), 2);
    migrated.save(&path, config.recursive).unwrap();
    assert!(std::fs::read(&path).unwrap().starts_with(b"PPDNS003"));

    let rule_file = directory.join("force_forward_list.txt");
    std::fs::write(&rule_file, "full:changed.example\n").unwrap();
    let new_config = Config::parse_from([
        "test", "--ipv6", "raw", "--rules-dir", directory.to_str().unwrap(),
        "--custom-forward", "127.0.0.1:1020", "--encrypted", "127.0.0.1:5304",
    ]);
    let new_rules = Rules::load(&new_config).unwrap();
    let mut restored = Cache::new(16, 600).with_disk(4096, new_config.recursive);
    assert_eq!(restored.restore(&path, &new_config, 4096, |q, a| can_restore_recursive(&new_config, &new_rules, q, a)).unwrap(), 1);

    for (domain, hit) in [("keep.example.", true), ("changed.example.", false)] {
        let query_wire = request(domain, RecordType::A, 42);
        let query = Message::from_vec(&query_wire).unwrap();
        let key = restored.key(&query, &query_wire).unwrap();
        assert_eq!(restored.get(key, 42, true).is_some(), hit);
    }
    std::fs::remove_file(rule_file).unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn persistent_cache_survives_hot_eviction_and_honors_byte_limit() {
    let path = std::env::temp_dir().join(format!("paopaodns-large-cache-{}.bin", std::process::id()));
    let config = Config::parse_from(["test"]);
    let budget = 1024 * 1024;
    let mut cache = Cache::new(1024, 600).with_disk(budget, config.recursive);
    let mut small_cache = Cache::new(2, 600).with_disk(4096, config.recursive);
    for i in 0..1500 {
        let packet = request(&format!("record-{i}.example."), RecordType::A, 1);
        let query = Message::from_vec(&packet).unwrap();
        let mut response = dns::response(&query, ResponseCode::NoError);
        response.answers.push(Record::from_rdata(
            query.queries[0].name.clone(),
            600,
            hickory_proto::rr::RData::A(A(Ipv4Addr::new(192, 0, 2, 1))),
        ));
        let answer = Answer { bytes: response.to_vec().unwrap(), message: response, upstream: None };
        let key = cache.key(&query, &packet).unwrap();
        cache.insert(key, &answer, true);
        small_cache.insert(key, &answer, true);
    }
    let early = request("record-0.example.", RecordType::A, 2);
    let latest = request("record-1499.example.", RecordType::A, 2);
    assert_eq!(cache.get(&early[2..], 2, false).unwrap().source, "disk-cache");
    assert!(small_cache.get(&early[2..], 2, false).is_none());
    assert!(small_cache.get(&latest[2..], 2, false).is_some());
    assert!(small_cache.snapshot(config.recursive).unwrap().unwrap().len() <= 4096);

    cache.save(&path, config.recursive).unwrap();
    let mut restored = Cache::new(2, 600).with_disk(budget, config.recursive);
    assert_eq!(restored.restore(&path, &config, budget, |_, _| true).unwrap(), 1500);
    assert!(restored.get(&early[2..], 2, false).is_some());
    restored.invalidate(&early[2..]);
    assert!(restored.get(&early[2..], 2, false).is_none());
    assert!(restored.get(&latest[2..], 2, false).is_some());
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn custom_ip_selection_is_limited_to_the_custom_upstream() {
    LocalSet::new()
        .run_until(async {
            let (recursive, recursive_calls, _) = fake_upstream(Ipv4Addr::new(192, 0, 2, 1)).await;
            let (custom, custom_calls, _) = fake_upstream(Ipv4Addr::new(192, 0, 2, 3)).await;
            let path = std::env::temp_dir().join(format!(
                "paopaodns-custom-ip-{}-{}",
                std::process::id(),
                recursive.port()
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("force_forward_list.txt"), "domain:forward.test\n").unwrap();

            for (mode, count_v4, count_v6) in [("ipv4", 1, 0), ("ipv6", 0, 1), ("both", 1, 1)] {
                let config = Config::parse_from([
                    "paopaodns-rs",
                    "--recursive",
                    &recursive.to_string(),
                    "--custom-forward",
                    &custom.to_string(),
                    "--custom-ip",
                    mode,
                    "--ipv6",
                    "raw",
                    "--rules-dir",
                    path.to_str().unwrap(),
                ]);
                let server = Rc::new(Server {
                    rules: Rules::load(&config).unwrap(),
                    config,
                    cache: RefCell::new(Cache::new(0, 600)),
                    latency: RefCell::default(),
                    prefetch_slots: Arc::new(Semaphore::new(1)),
                });
                let before = *custom_calls.borrow();
                for kind in [RecordType::A, RecordType::AAAA] {
                    for domain in ["forward.test.", "ordinary.test."] {
                        let reply = server
                            .respond(&request(domain, kind, 7), true, ([127, 0, 0, 1], 44).into())
                            .await
                            .unwrap();
                        let message = Message::from_vec(&reply).unwrap();
                        assert_eq!(message.metadata.response_code, ResponseCode::NoError);
                        let expected = if domain == "ordinary.test." {
                            1
                        } else if kind == RecordType::A {
                            count_v4
                        } else {
                            count_v6
                        };
                        assert_eq!(message.answers.len(), expected);
                    }
                }
                assert_eq!(*custom_calls.borrow() - before, count_v4 + count_v6);
            }
            assert_eq!(*recursive_calls.borrow(), 6);
            std::fs::remove_file(path.join("force_forward_list.txt")).unwrap();
            std::fs::remove_dir(path).unwrap();
        })
        .await;
}

use hickory_proto::op::{Edns, Message, MessageType, Metadata, OpCode, ResponseCode};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    time::timeout,
};

use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};

pub const MAX_QUERY_SIZE: usize = 4096;
pub const MAX_UDP_PAYLOAD: u16 = 1232;

pub fn response(request: &Message, code: ResponseCode) -> Message {
    let mut reply = Message::response(request.metadata.id, request.metadata.op_code);
    reply.metadata = Metadata::response_from_request(&request.metadata);
    reply.metadata.recursion_available = true;
    reply.metadata.response_code = code;
    reply.queries.clone_from(&request.queries);
    if request.edns.is_some() {
        let mut edns = Edns::new();
        edns.set_max_payload(MAX_UDP_PAYLOAD);
        reply.edns = Some(edns);
    }
    reply
}

pub fn malformed(request: &[u8]) -> Option<Vec<u8>> {
    let first = request.get(..4)?;
    if first[2] & 0x80 != 0 {
        return None;
    }
    let id = u16::from_be_bytes([first[0], first[1]]);
    Message::error_msg(id, OpCode::Query, ResponseCode::FormErr)
        .to_vec()
        .ok()
}

pub fn fit_udp(request: &Message, reply: Vec<u8>) -> Option<Vec<u8>> {
    let limit = usize::from(request.max_payload().clamp(512, MAX_UDP_PAYLOAD));
    if reply.len() <= limit {
        return Some(reply);
    }
    let answer = Message::from_vec(&reply).ok()?;
    let mut truncated = response(request, answer.metadata.response_code);
    truncated.metadata.truncation = true;
    truncated.to_vec().ok()
}

pub struct Answer {
    pub message: Message,
    pub bytes: Vec<u8>,
    pub(crate) upstream: Option<SocketAddr>,
}

impl Answer {
    fn decode(bytes: &[u8], id: u16, request: &Message) -> Option<Self> {
        let message = Message::from_vec(bytes).ok()?;
        if message.metadata.message_type != MessageType::Response
            || message.metadata.id != id
            || message.metadata.op_code != request.metadata.op_code
            || message.queries != request.queries
            || message.signature.is_some()
        {
            return None;
        }
        Some(Self {
            message,
            bytes: bytes.to_vec(),
            upstream: None,
        })
    }
}

pub async fn exchange(
    destination: SocketAddr,
    request: &Message,
    limit: Duration,
) -> io::Result<Answer> {
    timeout(limit, async {
        let mut query = request.clone();
        let id = Message::query().metadata.id;
        query.metadata.id = id;
        if let Some(edns) = &mut query.edns {
            edns.set_max_payload(edns.max_payload().clamp(512, MAX_UDP_PAYLOAD));
        }
        let query = query.to_vec().map_err(io::Error::other)?;
        let mut answer = if destination.port() == 853 {
            exchange_tls(destination, &query, id, request, limit).await?
        } else {
            exchange_udp(destination, &query, id, request).await?
        };
        let original = request.metadata.id;
        answer.message.metadata.id = original;
        answer.bytes[..2].copy_from_slice(&original.to_be_bytes());
        answer.upstream = Some(destination);
        Ok(answer)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "upstream timed out"))?
}

struct IdleTls {
    destination: SocketAddr,
    stream: TlsStream<TcpStream>,
    since: Instant,
}

static IDLE_TLS: Mutex<Vec<IdleTls>> = Mutex::new(Vec::new());
const TLS_IDLE_TIME: Duration = Duration::from_secs(20);
const MAX_IDLE_TLS: usize = 8;

pub fn prune_idle_tls() {
    let mut idle = IDLE_TLS.lock().unwrap();
    idle.retain(|entry| entry.since.elapsed() < TLS_IDLE_TIME);
}

async fn exchange_tls(
    destination: SocketAddr,
    query: &[u8],
    id: u16,
    request: &Message,
    limit: Duration,
) -> io::Result<Answer> {
    let pooled = {
        let mut idle = IDLE_TLS.lock().unwrap();
        idle.retain(|entry| entry.since.elapsed() < TLS_IDLE_TIME);
        idle.iter()
            .rposition(|entry| entry.destination == destination)
            .map(|index| idle.swap_remove(index).stream)
    };

    if let Some(mut stream) = pooled
        && let Ok(Ok(answer)) = timeout(
            (limit / 2).min(Duration::from_millis(750)),
            exchange_stream(&mut stream, query, id, request),
        )
        .await
    {
        put_idle_tls(destination, stream);
        return Ok(answer);
    }

    let tcp = TcpStream::connect(destination).await?;
    let mut stream = tls_connector()
        .connect(tls_server_name(destination), tcp)
        .await?;
    let answer = exchange_stream(&mut stream, query, id, request).await?;
    put_idle_tls(destination, stream);
    Ok(answer)
}

fn put_idle_tls(destination: SocketAddr, stream: TlsStream<TcpStream>) {
    let mut idle = IDLE_TLS.lock().unwrap();
    if idle.len() < MAX_IDLE_TLS {
        idle.push(IdleTls {
            destination,
            stream,
            since: Instant::now(),
        });
    }
}

fn tls_connector() -> &'static TlsConnector {
    static CONNECTOR: OnceLock<TlsConnector> = OnceLock::new();
    CONNECTOR.get_or_init(|| {
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        TlsConnector::from(Arc::new(config))
    })
}

fn tls_server_name(destination: SocketAddr) -> ServerName<'static> {
    match destination.ip() {
        std::net::IpAddr::V4(ip) if matches!(ip.octets(), [223, 5, 5, 5] | [223, 6, 6, 6]) => {
            ServerName::try_from("dns.alidns.com").expect("valid AliDNS certificate name")
        }
        addr => ServerName::IpAddress(addr.into()),
    }
}

async fn exchange_udp(
    destination: SocketAddr,
    query: &[u8],
    id: u16,
    request: &Message,
) -> io::Result<Answer> {
    let bind = if destination.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let udp = UdpSocket::bind(bind).await?;
    udp.connect(destination).await?;
    udp.send(query).await?;
    let mut bytes = vec![0u8; usize::from(u16::MAX)];
    loop {
        let size = udp.recv(&mut bytes).await?;
        if let Some(answer) = Answer::decode(&bytes[..size], id, request) {
            return if answer.message.metadata.truncation {
                let mut tcp = TcpStream::connect(destination).await?;
                exchange_stream(&mut tcp, query, id, request).await
            } else {
                Ok(answer)
            };
        }
        tokio::task::yield_now().await;
    }
}

async fn exchange_stream(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    query: &[u8],
    id: u16,
    request: &Message,
) -> io::Result<Answer> {
    write_frame(stream, query).await?;
    let bytes = read_frame(stream, usize::from(u16::MAX)).await?;
    Answer::decode(&bytes, id, request)
        .filter(|answer| !answer.message.metadata.truncation)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid upstream DNS stream response",
            )
        })
}

pub async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    limit: usize,
) -> io::Result<Vec<u8>> {
    let length = usize::from(stream.read_u16().await?);
    if !(12..=limit).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid DNS frame length",
        ));
    }
    let mut packet = vec![0u8; length];
    stream.read_exact(&mut packet).await?;
    Ok(packet)
}

pub async fn write_frame(stream: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> io::Result<()> {
    let length = u16::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DNS frame too large"))?;
    stream.write_u16(length).await?;
    stream.write_all(bytes).await
}

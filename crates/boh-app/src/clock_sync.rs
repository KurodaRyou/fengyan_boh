//! Node clock verification. Network waits are cancellable; durable publication is not.

use std::future::{Future, pending, poll_fn};
use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use boh_domain::UnixMillis;
use boh_domain::time::clock_signal_valid;
use boh_storage::{Diagnostic, StorageError, clock::Clock};
use sntpc::{KissOfDeathCode, NtpContext, NtpTimestampGenerator, NtpUdpSocket};
use tokio::sync::{oneshot, watch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtpServer {
    pub host: String,
    pub port: u16,
}

impl NtpServer {
    pub fn parse(value: &str) -> Result<Self, StorageError> {
        if value.is_empty() {
            return Err(StorageError::Message(
                "NTP server address must not be empty",
            ));
        }
        if value.chars().any(char::is_whitespace) {
            return Err(StorageError::Message(
                "NTP server address must not contain whitespace",
            ));
        }
        let (host, port) = if let Some(rest) = value.strip_prefix('[') {
            let (host, rest) = rest
                .split_once(']')
                .ok_or(StorageError::Message("IPv6 address requires a closing ']'"))?;
            host.parse::<Ipv6Addr>().map_err(|_| {
                StorageError::Message("bracketed host must be a valid IPv6 address")
            })?;
            let port = if rest.is_empty() {
                None
            } else {
                Some(
                    rest.strip_prefix(':')
                        .ok_or(StorageError::Message("only ':port' may appear after ']'"))?,
                )
            };
            (host, port)
        } else {
            if value.parse::<Ipv6Addr>().is_ok() {
                return Err(StorageError::Message(
                    "IPv6 addresses require square brackets",
                ));
            }
            let (host, port) = value
                .split_once(':')
                .map_or((value, None), |(h, p)| (h, Some(p)));
            if host.is_empty() {
                return Err(StorageError::Message("NTP host must not be empty"));
            }
            if host.contains(['[', ']', '/']) {
                return Err(StorageError::Message(
                    "NTP host must not contain '[', ']' or '/'",
                ));
            }
            (host, port)
        };
        let port = match port {
            None => 123,
            Some(port) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or(StorageError::Message("NTP port must be in 1..=65535"))?,
            Some(_) => {
                return Err(StorageError::Message(
                    "NTP port must be a decimal integer in 1..=65535",
                ));
            }
        };
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }
}

#[derive(Debug)]
pub enum NtpServersError {
    Empty,
    InvalidEntry {
        index: usize,
        value: String,
        source: StorageError,
    },
}

impl std::fmt::Display for NtpServersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "ntp_servers must not be empty"),
            Self::InvalidEntry {
                index,
                value,
                source,
            } => {
                write!(f, "invalid ntp_servers[{index}]: {value:?}: {source}")
            }
        }
    }
}

// Display includes the entry reason for serde; exposing it as source repeats it.
impl std::error::Error for NtpServersError {}

pub fn parse_ntp_servers(values: &[String]) -> Result<Vec<NtpServer>, NtpServersError> {
    if values.is_empty() {
        return Err(NtpServersError::Empty);
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            NtpServer::parse(value).map_err(|source| NtpServersError::InvalidEntry {
                index,
                value: value.clone(),
                source,
            })
        })
        .collect()
}

#[derive(Clone)]
pub struct ClockSignal {
    last_synced_at: watch::Sender<Option<UnixMillis>>,
}

impl ClockSignal {
    pub fn new(last_synced_at: Option<UnixMillis>) -> Self {
        Self {
            last_synced_at: watch::channel(last_synced_at).0,
        }
    }

    pub fn snapshot(&self) -> Option<UnixMillis> {
        *self.last_synced_at.borrow()
    }

    pub fn valid(&self, now: UnixMillis) -> bool {
        self.status(now).0
    }

    /// Validity and timestamp from one snapshot, even while a check publishes.
    pub fn status(&self, now: UnixMillis) -> (bool, Option<UnixMillis>) {
        let last = self.snapshot();
        (last.is_some_and(|last| clock_signal_valid(last, now)), last)
    }

    fn publish(&self, value: Option<UnixMillis>) {
        self.last_synced_at.send_replace(value);
    }

    pub async fn load(db_path: &Path) -> Self {
        let path = state_path(db_path);
        let loaded = tokio::task::spawn_blocking(move || read_state(&path)).await;
        let value = match loaded {
            Ok(value) => value,
            Err(error) => {
                let error = StorageError::join("读取节点校时状态文件", error);
                tracing::warn!(error = %Diagnostic(&error), "clock state unavailable");
                None
            }
        };
        Self::new(value)
    }
}

fn state_path(db_path: &Path) -> PathBuf {
    let mut path = db_path.as_os_str().to_owned();
    path.push("-clock-sync");
    PathBuf::from(path)
}

fn read_state(path: &Path) -> Option<UnixMillis> {
    let bytes = (|| {
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(32).read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    })();
    match bytes {
        Ok(bytes) => {
            let value = parse_state(&bytes);
            if value.is_none() {
                tracing::warn!(path = %path.display(), "invalid clock state");
            }
            value
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "no clock state");
            None
        }
        Err(error) => {
            let error = StorageError::io("读取节点校时状态文件", error);
            tracing::warn!(error = %Diagnostic(&error), path = %path.display(), "clock state unavailable");
            None
        }
    }
}

fn parse_state(bytes: &[u8]) -> Option<UnixMillis> {
    let text = std::str::from_utf8(bytes).ok()?;
    let number = text.strip_suffix('\n').unwrap_or(text);
    if number.is_empty()
        || !number.bytes().all(|b| b.is_ascii_digit())
        || (number.len() > 1 && number.starts_with('0'))
    {
        return None;
    }
    number.parse::<i64>().ok().map(UnixMillis)
}

fn directory(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    std::fs::File::open(directory(path))
        .and_then(|file| file.sync_all())
        .map_err(|error| StorageError::io("同步节点校时状态目录", error))
}

fn persist(path: &Path, time: UnixMillis) -> Result<(), StorageError> {
    // Checks are serial. Unlink a crash leftover so its owner/permissions cannot
    // prevent creating a fresh temporary file under the service user's identity.
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    let temp = PathBuf::from(name);
    remove_if_present(&temp)
        .map_err(|error| StorageError::io("删除残留节点校时临时文件", error))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| StorageError::io("创建节点校时临时文件", error))?;
    let result = (|| {
        writeln!(file, "{}", time.0)
            .map_err(|error| StorageError::io("写入节点校时临时文件", error))?;
        file.sync_all()
            .map_err(|error| StorageError::io("同步节点校时临时文件", error))?;
        std::fs::rename(&temp, path)
            .map_err(|error| StorageError::io("重命名节点校时状态文件", error))?;
        sync_directory(path)
    })();
    if let Err(error) = remove_if_present(&temp) {
        let error = StorageError::io("清理节点校时临时文件", error);
        tracing::error!(error = %Diagnostic(&error), "clock state cleanup failed");
    }
    result
}

fn remove_state(path: &Path) -> Result<(), StorageError> {
    remove_if_present(path).map_err(|error| StorageError::io("删除节点校时状态文件", error))?;
    sync_directory(path)
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[derive(Clone, Copy)]
struct Timestamp<'a> {
    clock: &'a Clock,
    value: UnixMillis,
}

impl NtpTimestampGenerator for Timestamp<'_> {
    fn init(&mut self) {
        self.value = self.clock.now();
    }
    fn timestamp_sec(&self) -> u64 {
        u64::try_from(self.value.0.div_euclid(1000)).unwrap_or(0)
    }
    fn timestamp_subsec_micros(&self) -> u32 {
        u32::try_from(self.value.0.rem_euclid(1000))
            .unwrap_or(0)
            .checked_mul(1000)
            .unwrap_or(0)
    }
    fn precision(&self) -> i8 {
        -10
    }
}

struct Socket<'a> {
    udp: tokio::net::UdpSocket,
    clock: &'a Clock,
    sent: AtomicI64,
    received: AtomicI64,
}

#[track_caller]
fn network_error(operation: &'static str, error: std::io::Error) -> sntpc::Error {
    let error = StorageError::io(operation, error);
    tracing::debug!(error = %Diagnostic(&error), "NTP network unavailable");
    sntpc::Error::Network
}

#[track_caller]
fn sent_datagram(sent: usize, expected: usize) -> sntpc::Result<usize> {
    if sent != expected {
        return Err(network_error(
            "发送完整 NTP 数据报",
            std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                format!("NTP datagram sent {sent} bytes; expected {expected}"),
            ),
        ));
    }
    Ok(sent)
}

impl NtpUdpSocket for Socket<'_> {
    async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> sntpc::Result<usize> {
        self.sent.store(self.clock.now().0, Ordering::Relaxed);
        self.udp
            .send_to(buf, addr)
            .await
            .map_err(|error| network_error("发送 NTP 查询", error))
            .and_then(|sent| sent_datagram(sent, buf.len()))
    }
    async fn recv_from(&self, buf: &mut [u8]) -> sntpc::Result<(usize, SocketAddr)> {
        let result = self
            .udp
            .recv_from(buf)
            .await
            .map_err(|error| network_error("接收 NTP 回复", error));
        self.received.store(self.clock.now().0, Ordering::Relaxed);
        result
    }
}

enum CheckResult {
    Synced(UnixMillis, i64),
    Incorrect(i64),
    NoReply,
}

#[tracing::instrument(skip_all, fields(host = %server.host, port = server.port))]
async fn query(server: &NtpServer, clock: &Clock) -> Result<CheckResult, sntpc::Error> {
    let addresses = tokio::net::lookup_host((server.host.as_str(), server.port))
        .await
        .map_err(|error| network_error("解析 NTP 服务器地址", error))?;
    query_addresses(addresses, clock).await
}

async fn query_addresses(
    addresses: impl Iterator<Item = SocketAddr>,
    clock: &Clock,
) -> Result<CheckResult, sntpc::Error> {
    // The caller's single five-second deadline covers DNS and every address.
    for addr in addresses {
        match query_address(addr, clock).await {
            Ok(CheckResult::NoReply) => {}
            Ok(result) => return Ok(result),
            // KoD applies to the server name, not just this resolved address.
            Err(error @ sntpc::Error::KissOfDeath(_)) => return Err(error),
            // The socket adapter records I/O failures and incomplete datagrams.
            Err(sntpc::Error::Network) => {}
            Err(error) => tracing::debug!(?error, %addr, "no valid NTP reply from address"),
        }
    }
    Ok(CheckResult::NoReply)
}

#[tracing::instrument(skip_all, fields(%addr))]
async fn query_address(addr: SocketAddr, clock: &Clock) -> Result<CheckResult, sntpc::Error> {
    let bind = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let udp = tokio::net::UdpSocket::bind(bind)
        .await
        .map_err(|error| network_error("打开 NTP UDP 套接字", error))?;
    let socket = Socket {
        udp,
        clock,
        sent: AtomicI64::new(0),
        received: AtomicI64::new(0),
    };
    let reply = sntpc::get_time(
        addr,
        &socket,
        NtpContext::new(Timestamp {
            clock,
            value: UnixMillis(0),
        }),
    )
    .await?;
    let received = socket.received.load(Ordering::Relaxed);
    let elapsed = i128::from(received).checked_sub(i128::from(socket.sent.load(Ordering::Relaxed)));
    if !elapsed.is_some_and(|elapsed| (0..=5000).contains(&elapsed)) {
        return Ok(CheckResult::NoReply);
    }
    Ok(if reply.offset.unsigned_abs() <= 60_000_000 {
        CheckResult::Synced(UnixMillis(received), reply.offset / 1000)
    } else {
        CheckResult::Incorrect(reply.offset / 1000)
    })
}

struct Checker {
    clock: Clock,
    servers: Vec<NtpServer>,
    retired: Vec<NtpServer>,
    path: PathBuf,
    signal: ClockSignal,
    last_check_succeeded: bool,
    offset_exceeded: bool,
    invalid_signal_logged: bool,
}

impl Checker {
    fn new(db_path: &Path, clock: Clock, servers: Vec<NtpServer>, signal: ClockSignal) -> Self {
        Self {
            clock,
            servers,
            retired: Vec::new(),
            path: state_path(db_path),
            signal,
            last_check_succeeded: false,
            offset_exceeded: false,
            invalid_signal_logged: false,
        }
    }

    // Only background verification observes transitions. Request and writer reads
    // stay free of logging, and status() releases its lock before any log output.
    fn observe_signal(&mut self, now: UnixMillis) -> bool {
        let (valid, last) = self.signal.status(now);
        if valid || last.is_none() {
            self.invalid_signal_logged = false;
        } else if !self.invalid_signal_logged {
            self.invalid_signal_logged = true;
            if let Some(last) = last {
                let reason = if now < last {
                    "clock precedes last synchronization"
                } else {
                    "expired"
                };
                tracing::info!(
                    reason,
                    last_synced_at = last.0,
                    now = now.0,
                    "node clock signal invalid"
                );
            }
        }
        valid
    }

    async fn check(&mut self, mut stop: Option<&mut watch::Receiver<bool>>) -> bool {
        self.observe_signal(self.clock.now());
        let mut outcome = None;
        for server in &self.servers {
            if stop.as_ref().is_some_and(|stop| *stop.borrow()) {
                return false;
            }
            if self.retired.contains(server) {
                continue;
            }
            let result = tokio::select! {
                biased;
                _ = wait_for_stop(stop.as_deref_mut()) => return false,
                _ = self.clock.sleep(Duration::from_secs(5)) => {
                    tracing::debug!(host = %server.host, port = server.port, "NTP query timed out");
                    continue;
                },
                result = query(server, &self.clock) => result,
            };
            match result {
                Ok(CheckResult::NoReply) => {}
                Ok(result) => {
                    outcome = Some((result, server.clone()));
                    break;
                }
                Err(sntpc::Error::KissOfDeath(
                    code @ (KissOfDeathCode::Deny | KissOfDeathCode::Rstr),
                )) => {
                    tracing::warn!(host = %server.host, port = server.port, code = code.as_str(), "NTP server retired for this process");
                    self.retired.push(server.clone());
                }
                // DNS I/O errors are recorded where their source is still available.
                Err(sntpc::Error::Network) => {}
                Err(error) => tracing::debug!(?error, host = %server.host, "no valid NTP reply"),
            }
        }
        let Some((outcome, server)) = outcome else {
            if self.last_check_succeeded {
                tracing::info!("all NTP servers returned no valid reply");
            } else {
                tracing::debug!("all NTP servers returned no valid reply");
            }
            self.last_check_succeeded = false;
            return true;
        };
        self.last_check_succeeded = false;
        let path = self.path.clone();
        let (new_signal, offset_ms) = match outcome {
            CheckResult::NoReply => return true,
            CheckResult::Synced(time, offset) => (Some(time), offset),
            CheckResult::Incorrect(offset_ms) => {
                let was_valid = self.signal.valid(self.clock.now());
                self.signal.publish(None);
                if was_valid || !self.offset_exceeded {
                    tracing::warn!(host = %server.host, port = server.port, offset_ms, "node clock offset exceeds one minute");
                } else {
                    tracing::debug!(host = %server.host, port = server.port, offset_ms, "node clock offset exceeds one minute");
                }
                self.offset_exceeded = true;
                (None, offset_ms)
            }
        };
        // Do not select on stop here: spawn_blocking cannot be cancelled once running.
        let result = tokio::task::spawn_blocking(move || match new_signal {
            Some(time) => persist(&path, time),
            None => remove_state(&path),
        })
        .await
        .map_err(|error| StorageError::join("等待节点校时状态持久化", error))
        .and_then(|result| result);
        match result {
            Ok(()) => {
                if new_signal.is_some() {
                    let now = self.clock.now();
                    let was_valid = self.signal.valid(now);
                    self.signal.publish(new_signal);
                    self.last_check_succeeded = true;
                    self.offset_exceeded = false;
                    self.invalid_signal_logged = false;
                    if !was_valid && self.signal.valid(now) {
                        tracing::info!(host = %server.host, port = server.port, offset_ms, "node clock signal restored");
                    } else {
                        tracing::debug!(host = %server.host, port = server.port, offset_ms, "node clock check succeeded");
                    }
                }
            }
            Err(error) => {
                tracing::error!(error = %Diagnostic(&error), "clock state persistence failed")
            }
        }
        true
    }
}

async fn wait_for_stop(stop: Option<&mut watch::Receiver<bool>>) {
    match stop {
        Some(stop) => {
            let _ = stop.changed().await;
        }
        None => pending().await,
    }
}

pub async fn ensure_synchronized(
    db_path: &Path,
    clock: Clock,
    servers: Vec<NtpServer>,
) -> Result<ClockSignal, StorageError> {
    let signal = ClockSignal::load(db_path).await;
    if !signal.valid(clock.now()) {
        let mut checker = Checker::new(db_path, clock.clone(), servers, signal.clone());
        checker.check(None).await;
    }
    if signal.valid(clock.now()) {
        Ok(signal)
    } else {
        Err(StorageError::Message("node clock is not synchronized"))
    }
}

#[derive(Clone)]
pub struct ClockStop(watch::Sender<bool>);
impl ClockStop {
    pub fn stop(&self) {
        self.0.send_replace(true);
    }
}

pub struct ClockTasks {
    stop: ClockStop,
    progress: watch::Receiver<(u64, u64)>,
    worker: tokio::task::JoinHandle<()>,
}

impl ClockTasks {
    pub async fn start(
        db_path: &Path,
        clock: Clock,
        servers: Vec<NtpServer>,
        signal: ClockSignal,
    ) -> Self {
        let (stop, mut stopped) = watch::channel(false);
        let (progress, counts) = watch::channel((0, 0));
        let (ready, started) = oneshot::channel();
        let mut checker = Checker::new(db_path, clock.clone(), servers, signal);
        let worker = tokio::spawn(async move {
            let completed = {
                let mut initial = std::pin::pin!(checker.check(Some(&mut stopped)));
                let mut ready = Some(ready);
                poll_fn(|cx| {
                    let result = initial.as_mut().poll(cx);
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                    result
                })
                .await
            };
            // Release the mutable checker/stop borrows before the scheduler loop.
            if !completed {
                return;
            }
            run_schedule(checker, stopped, progress, clock).await;
        });
        let _ = started.await;
        Self {
            stop: ClockStop(stop),
            progress: counts,
            worker,
        }
    }
    pub fn stop_handle(&self) -> ClockStop {
        self.stop.clone()
    }
    pub async fn wait_checks(&self, count: u64) {
        self.wait(count, false).await;
    }
    pub async fn wait_ticks(&self, count: u64) {
        self.wait(count, true).await;
    }
    async fn wait(&self, count: u64, ticks: bool) {
        let mut progress = self.progress.clone();
        loop {
            let (checks, tick_count) = *progress.borrow_and_update();
            if (if ticks { tick_count } else { checks }) >= count {
                return;
            }
            if progress.changed().await.is_err() {
                return;
            }
        }
    }
    pub async fn shutdown(self) -> Result<(), StorageError> {
        self.stop.stop();
        self.worker
            .await
            .map_err(|error| StorageError::join("停止节点校时核对", error))
    }
}

async fn run_schedule(
    mut checker: Checker,
    mut stopped: watch::Receiver<bool>,
    progress: watch::Sender<(u64, u64)>,
    clock: Clock,
) {
    let mut checks = 1_u64;
    let mut ticks = 0_u64;
    let mut last_check = clock.monotonic();
    loop {
        if *stopped.borrow() {
            return;
        }
        let mut sleep = std::pin::pin!(clock.sleep(Duration::from_secs(60)));
        let mut published = false;
        tokio::select! {
            biased;
            _ = stopped.changed() => return,
            _ = poll_fn(|cx| {
                let result = sleep.as_mut().poll(cx);
                if !published { progress.send_replace((checks, ticks)); published = true; }
                result
            }) => {},
        }
        ticks = ticks.saturating_add(1);
        if !checker.observe_signal(clock.now())
            || clock.monotonic().saturating_sub(last_check) >= Duration::from_secs(3600)
        {
            if !checker.check(Some(&mut stopped)).await {
                return;
            }
            checks = checks.saturating_add(1);
            last_check = clock.monotonic();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Capture as LogCapture;
    use tracing::instrument::WithSubscriber;

    async fn reply_once(
        kod: Option<[u8; 4]>,
        offset_secs: u32,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut request = [0_u8; 48];
            let (len, peer) = socket.recv_from(&mut request).await.unwrap();
            assert_eq!(len, 48);
            let mut reply = [0_u8; 48];
            reply[0] = (request[0] & 0b0011_1000) | 4;
            reply[1] = 2;
            reply[24..32].copy_from_slice(&request[40..48]);
            reply[32..40].copy_from_slice(&request[40..48]);
            reply[40..48].copy_from_slice(&request[40..48]);
            let seconds = u32::from_be_bytes(request[40..44].try_into().unwrap())
                .checked_add(offset_secs)
                .unwrap()
                .to_be_bytes();
            reply[32..36].copy_from_slice(&seconds);
            reply[40..44].copy_from_slice(&seconds);
            if let Some(code) = kod {
                reply[1] = 0;
                reply[12..16].copy_from_slice(&code);
            }
            socket.send_to(&reply, peer).await.unwrap();
        });
        (addr, task)
    }

    fn unused_address() -> std::net::UdpSocket {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        socket
    }

    fn assert_unused(socket: &std::net::UdpSocket) {
        let mut buf = [0; 48];
        assert_eq!(
            socket.recv_from(&mut buf).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn immediate_send_failure_tries_next_address_in_order() {
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let (working, reply) = reply_once(None, 0).await;
        let unused = unused_address();
        // SO_BROADCAST is disabled, so this send fails without a network wait.
        let blocked = "255.255.255.255:123".parse().unwrap();
        assert!(matches!(
            query_address(blocked, &clock.clock()).await,
            Err(sntpc::Error::Network)
        ));
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            query_addresses(
                [blocked, working, unused.local_addr().unwrap()].into_iter(),
                &clock.clock(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, CheckResult::Synced(time, _) if time == clock.clock().now()));
        reply.await.unwrap();
        assert_unused(&unused);
    }

    #[tokio::test]
    async fn first_valid_reply_decides_even_when_offset_is_wrong() {
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let (first, reply) = reply_once(None, 120).await;
        let unused = unused_address();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            query_addresses(
                [first, unused.local_addr().unwrap()].into_iter(),
                &clock.clock(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, CheckResult::Incorrect(_)));
        reply.await.unwrap();
        assert_unused(&unused);
    }

    #[tokio::test]
    async fn kiss_of_death_stops_the_entire_server_address_list() {
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        for code in [*b"DENY", *b"RSTR", *b"RATE"] {
            let (first, reply) = reply_once(Some(code), 0).await;
            let unused = unused_address();
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                query_addresses(
                    [first, unused.local_addr().unwrap()].into_iter(),
                    &clock.clock(),
                ),
            )
            .await
            .unwrap();
            assert!(matches!(result, Err(sntpc::Error::KissOfDeath(_))));
            reply.await.unwrap();
            assert_unused(&unused);
        }
    }

    #[test]
    fn persistence_replaces_an_unwritable_crash_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boh.db-clock-sync");
        let temp = dir.path().join("boh.db-clock-sync.tmp");
        std::fs::write(&path, "123456789\n").unwrap();
        std::fs::write(&temp, "interrupted incomplete write\n").unwrap();
        // Keep an alias to prove the old inode is replaced even in a root test run.
        let alias = dir.path().join("leftover-alias");
        std::fs::hard_link(&temp, &alias).unwrap();
        let mut permissions = std::fs::metadata(&temp).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&temp, permissions).unwrap();
        persist(&path, UnixMillis(42)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"42\n");
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            b"interrupted incomplete write\n"
        );
        std::fs::remove_file(alias).unwrap();
        assert!(!temp.exists());
        persist(&path, UnixMillis(43)).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn failed_leftover_removal_keeps_the_old_signal_and_logs_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let path = state_path(&db);
        let temp = dir.path().join("boh.db-clock-sync.tmp");
        let previous = UnixMillis(1_792_137_600_000);
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_601_000));
        let signal = ClockSignal::new(Some(previous));
        std::fs::write(&path, format!("{}\n", previous.0)).unwrap();
        std::fs::create_dir(&temp).unwrap();
        let (server, reply) = reply_once(None, 0).await;
        let mut checker = Checker::new(
            &db,
            clock.clock(),
            vec![NtpServer::parse(&server.to_string()).unwrap()],
            signal.clone(),
        );
        let capture = LogCapture::default();
        assert!(
            checker
                .check(None)
                .with_subscriber(capture.subscriber())
                .await
        );
        reply.await.unwrap();
        assert_eq!(signal.snapshot(), Some(previous));
        assert_eq!(read_state(&path), Some(previous));
        assert!(temp.is_dir());
        let text = capture.text();
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("caused by"))
                .count(),
            1,
            "{text}"
        );
        for part in [
            "ERROR",
            "删除残留节点校时临时文件",
            "caused by",
            "clock_sync.rs:",
        ] {
            assert!(text.contains(part), "missing {part}: {text}");
        }
    }

    #[tokio::test]
    async fn network_failures_emit_one_complete_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        // Broadcast sends fail immediately; a NUL fails lookup before external DNS.
        for (server, operation) in [
            ("255.255.255.255:123", "发送 NTP 查询"),
            ("bad\0host", "解析 NTP 服务器地址"),
        ] {
            let mut checker = Checker::new(
                &dir.path().join("boh.db"),
                clock.clock(),
                vec![NtpServer::parse(server).unwrap()],
                ClockSignal::new(None),
            );
            let capture = LogCapture::default();
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    checker.check(None).with_subscriber(capture.subscriber())
                )
                .await
                .unwrap()
            );
            let text = capture.text();
            assert_eq!(
                text.lines()
                    .filter(|line| line.contains("caused by"))
                    .count(),
                1,
                "{text}"
            );
            for part in [
                operation,
                "caused by",
                "clock_sync.rs:",
                "host=",
                "port=",
                "NTP network unavailable",
            ] {
                assert!(text.contains(part), "missing {part}: {text}");
            }
        }
    }

    fn assert_log_count(text: &str, level: &str, message: &str, count: usize) {
        assert_eq!(
            text.lines()
                .filter(|line| line.contains(level) && line.contains(message))
                .count(),
            count,
            "{text}"
        );
    }

    #[tokio::test]
    async fn recovery_logs_info_and_routine_success_logs_debug() {
        let dir = tempfile::tempdir().unwrap();
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let signal = ClockSignal::new(None);
        let mut checker = Checker::new(
            &dir.path().join("boh.db"),
            clock.clock(),
            vec![],
            signal.clone(),
        );
        let capture = LogCapture::default();
        for _ in 0..2 {
            let (addr, reply) = reply_once(None, 1).await;
            checker.servers = vec![NtpServer::parse(&addr.to_string()).unwrap()];
            assert!(
                checker
                    .check(None)
                    .with_subscriber(capture.subscriber())
                    .await
            );
            reply.await.unwrap();
            assert!(signal.valid(clock.clock().now()));
            let text = capture.text();
            assert!(text.contains(&format!("port={}", addr.port())), "{text}");
        }
        let text = capture.text();
        assert_log_count(&text, "INFO", "node clock signal restored", 1);
        assert_log_count(&text, "DEBUG", "node clock check succeeded", 1);
        for part in ["host=127.0.0.1", "offset_ms=1000"] {
            assert!(text.contains(part), "missing {part}: {text}");
        }
    }

    #[tokio::test]
    async fn excessive_offset_warns_once_until_successful_recovery() {
        for initially_valid in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
            let signal = ClockSignal::new(initially_valid.then(|| clock.clock().now()));
            let mut checker = Checker::new(
                &dir.path().join("boh.db"),
                clock.clock(),
                vec![],
                signal.clone(),
            );
            let capture = LogCapture::default();
            for offset in [120, 120, 0, 120, 120] {
                let (addr, reply) = reply_once(None, offset).await;
                checker.servers = vec![NtpServer::parse(&addr.to_string()).unwrap()];
                assert!(
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        checker.check(None).with_subscriber(capture.subscriber()),
                    )
                    .await
                    .unwrap()
                );
                reply.await.unwrap();
                assert_eq!(signal.valid(clock.clock().now()), offset == 0);
                if offset != 0 {
                    assert_eq!(signal.snapshot(), None);
                    assert!(!state_path(&dir.path().join("boh.db")).exists());
                }
                let text = capture.text();
                assert!(text.contains(&format!("port={}", addr.port())), "{text}");
            }
            let text = capture.text();
            assert_log_count(&text, "WARN", "node clock offset exceeds one minute", 2);
            assert_log_count(&text, "DEBUG", "node clock offset exceeds one minute", 2);
            assert_log_count(&text, "INFO", "node clock signal restored", 1);
            for part in ["host=127.0.0.1", "offset_ms=120000"] {
                assert!(text.contains(part), "missing {part}: {text}");
            }
        }
    }

    #[test]
    fn signal_reads_never_write_logs() {
        let last = UnixMillis(1_792_137_600_000);
        let signal = ClockSignal::new(Some(last));
        let capture = LogCapture::default();
        tracing::subscriber::with_default(capture.subscriber(), || {
            for now in [
                last,
                UnixMillis(last.0 - 1),
                UnixMillis(last.0 + 604_800_001),
            ] {
                let valid = clock_signal_valid(last, now);
                assert_eq!(signal.status(now), (valid, Some(last)));
                assert_eq!(signal.valid(now), valid);
            }
            signal.publish(None);
            assert_eq!(signal.snapshot(), None);
        });
        assert!(capture.text().is_empty(), "{}", capture.text());
    }

    #[test]
    fn background_observation_logs_expired_and_regressed_signals_once() {
        let dir = tempfile::tempdir().unwrap();
        let last = UnixMillis(1_792_137_600_000);
        for (now, reason) in [
            (UnixMillis(last.0 + 7 * 24 * 60 * 60 * 1000 + 1), "expired"),
            (
                UnixMillis(last.0 - 1),
                "clock precedes last synchronization",
            ),
        ] {
            let signal = ClockSignal::new(Some(last));
            let mut checker = Checker::new(
                &dir.path().join("boh.db"),
                boh_storage::clock::ManualClock::new(last).clock(),
                vec![],
                signal.clone(),
            );
            let capture = LogCapture::default();
            tracing::subscriber::with_default(capture.subscriber(), || {
                assert!(checker.observe_signal(last));
                for _ in 0..3 {
                    assert!(!checker.observe_signal(now));
                    assert!(!signal.clone().valid(now));
                    assert_eq!(signal.status(now), (false, Some(last)));
                }
                signal.publish(Some(now));
                assert!(checker.observe_signal(now));
                assert!(!checker.observe_signal(UnixMillis(now.0 - 1)));
            });
            let text = capture.text();
            assert_log_count(&text, "INFO", "node clock signal invalid", 2);
            assert!(text.contains(reason), "{text}");
        }
    }

    #[tokio::test]
    async fn success_to_no_reply_logs_info_once_until_next_success() {
        let dir = tempfile::tempdir().unwrap();
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let signal = ClockSignal::new(None);
        let mut checker = Checker::new(
            &dir.path().join("boh.db"),
            clock.clock(),
            vec![],
            signal.clone(),
        );
        let capture = LogCapture::default();
        for _ in 0..2 {
            let (addr, reply) = reply_once(None, 0).await;
            checker.servers = vec![
                NtpServer::parse("255.255.255.255:123").unwrap(),
                NtpServer::parse(&addr.to_string()).unwrap(),
            ];
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    checker.check(None).with_subscriber(capture.subscriber()),
                )
                .await
                .expect("NTP check must finish even if broadcast sends do not fail")
            );
            reply.await.unwrap();
            checker.servers.truncate(1);
            for _ in 0..3 {
                assert!(
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        checker.check(None).with_subscriber(capture.subscriber()),
                    )
                    .await
                    .expect("NTP check must finish even if broadcast sends do not fail")
                );
                assert!(signal.valid(clock.clock().now()));
            }
        }
        let text = capture.text();
        assert_log_count(&text, "INFO", "all NTP servers returned no valid reply", 2);
        assert_log_count(&text, "DEBUG", "all NTP servers returned no valid reply", 4);
    }

    #[test]
    fn server_entry_reason_is_not_repeated_in_full_diagnostic() {
        let error = parse_ntp_servers(&["host:abc".to_owned()]).unwrap_err();
        let error = StorageError::external("解析 NTP 服务器列表", error);
        let text = Diagnostic(&error).to_string();
        assert_eq!(
            text.matches("NTP port must be a decimal integer").count(),
            1,
            "{text}"
        );
    }
    #[test]
    fn network_diagnostic_points_to_the_call_site() {
        let capture = LogCapture::default();
        let caller_line = tracing::subscriber::with_default(capture.subscriber(), || {
            let cause = std::io::Error::from(std::io::ErrorKind::Other);
            let caller_line = line!() + 1;
            assert_eq!(network_error("测试网络错误", cause), sntpc::Error::Network);
            caller_line
        });
        let text = capture.text();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(
            text.contains(&format!("clock_sync.rs:{caller_line}:")),
            "{text}"
        );
    }

    #[test]
    fn incomplete_datagram_logs_one_complete_network_diagnostic() {
        let capture = LogCapture::default();
        tracing::subscriber::with_default(capture.subscriber(), || {
            assert_eq!(sent_datagram(47, 48), Err(sntpc::Error::Network));
            assert_eq!(sent_datagram(48, 48), Ok(48));
        });
        let text = capture.text();
        assert_eq!(text.lines().count(), 1, "{text}");
        for part in [
            "DEBUG",
            "发送完整 NTP 数据报",
            "caused by",
            "sent 47 bytes; expected 48",
            "clock_sync.rs:",
        ] {
            assert!(text.contains(part), "missing {part}: {text}");
        }
    }

    #[tokio::test]
    async fn initial_no_reply_stays_debug() {
        let dir = tempfile::tempdir().unwrap();
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let mut checker = Checker::new(
            &dir.path().join("boh.db"),
            clock.clock(),
            vec![NtpServer::parse("255.255.255.255:123").unwrap()],
            ClockSignal::new(None),
        );
        let capture = LogCapture::default();
        for _ in 0..2 {
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    checker.check(None).with_subscriber(capture.subscriber()),
                )
                .await
                .expect("NTP check must finish even if broadcast sends do not fail")
            );
        }
        let text = capture.text();
        assert_log_count(&text, "INFO", "all NTP servers returned no valid reply", 0);
        assert_log_count(&text, "DEBUG", "all NTP servers returned no valid reply", 2);
    }
    #[tokio::test]
    async fn failed_persistence_does_not_report_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let temp = dir.path().join("boh.db-clock-sync.tmp");
        std::fs::create_dir(&temp).unwrap();
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let signal = ClockSignal::new(None);
        let (addr, reply) = reply_once(None, 0).await;
        let mut checker = Checker::new(
            &db,
            clock.clock(),
            vec![NtpServer::parse(&addr.to_string()).unwrap()],
            signal.clone(),
        );
        let capture = LogCapture::default();
        assert!(
            checker
                .check(None)
                .with_subscriber(capture.subscriber())
                .await
        );
        reply.await.unwrap();
        assert_eq!(signal.snapshot(), None);
        assert_log_count(&capture.text(), "INFO", "node clock signal restored", 0);
        assert_log_count(
            &capture.text(),
            "ERROR",
            "clock state persistence failed",
            1,
        );
        checker.servers.clear();
        assert!(
            checker
                .check(None)
                .with_subscriber(capture.subscriber())
                .await
        );
        assert_log_count(
            &capture.text(),
            "INFO",
            "all NTP servers returned no valid reply",
            0,
        );
        assert_log_count(
            &capture.text(),
            "DEBUG",
            "all NTP servers returned no valid reply",
            1,
        );
        std::fs::remove_dir(&temp).unwrap();
        let (addr, reply) = reply_once(None, 0).await;
        checker.servers = vec![NtpServer::parse(&addr.to_string()).unwrap()];
        assert!(
            checker
                .check(None)
                .with_subscriber(capture.subscriber())
                .await
        );
        reply.await.unwrap();
        assert!(signal.valid(clock.clock().now()));
        assert_log_count(&capture.text(), "INFO", "node clock signal restored", 1);
    }

    #[tokio::test]
    async fn failed_refresh_does_not_count_as_a_success_before_no_reply() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let clock = boh_storage::clock::ManualClock::new(UnixMillis(1_792_137_600_000));
        let signal = ClockSignal::new(None);
        let mut checker = Checker::new(&db, clock.clock(), vec![], signal.clone());
        let capture = LogCapture::default();
        for fail_persistence in [false, true] {
            if fail_persistence {
                std::fs::create_dir(dir.path().join("boh.db-clock-sync.tmp")).unwrap();
            }
            let (addr, reply) = reply_once(None, 0).await;
            checker.servers = vec![NtpServer::parse(&addr.to_string()).unwrap()];
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    checker.check(None).with_subscriber(capture.subscriber()),
                )
                .await
                .unwrap()
            );
            reply.await.unwrap();
        }
        checker.servers.clear();
        assert!(
            checker
                .check(None)
                .with_subscriber(capture.subscriber())
                .await
        );
        assert!(signal.valid(clock.clock().now()));
        let text = capture.text();
        assert_log_count(&text, "ERROR", "clock state persistence failed", 1);
        assert_log_count(&text, "INFO", "all NTP servers returned no valid reply", 0);
        assert_log_count(&text, "DEBUG", "all NTP servers returned no valid reply", 1);
    }
}

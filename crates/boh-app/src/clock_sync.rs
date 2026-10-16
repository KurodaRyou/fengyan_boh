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
        let invalid = || StorageError::Message("invalid NTP server address");
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(invalid());
        }
        let (host, port) = if let Some(rest) = value.strip_prefix('[') {
            let (host, rest) = rest.split_once(']').ok_or_else(invalid)?;
            host.parse::<Ipv6Addr>().map_err(|_| invalid())?;
            let port = if rest.is_empty() {
                None
            } else {
                Some(rest.strip_prefix(':').ok_or_else(invalid)?)
            };
            (host, port)
        } else {
            let (host, port) = value
                .split_once(':')
                .map_or((value, None), |(h, p)| (h, Some(p)));
            if host.is_empty() || host.contains(['[', ']', '/', ':']) {
                return Err(invalid());
            }
            (host, port)
        };
        let port = match port {
            None => 123,
            Some(port) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(invalid)?,
            Some(_) => return Err(invalid()),
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
            Self::InvalidEntry { index, value, .. } => {
                write!(f, "invalid ntp_servers[{index}]: {value:?}")
            }
        }
    }
}

impl std::error::Error for NtpServersError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Empty => None,
            Self::InvalidEntry { source, .. } => Some(source),
        }
    }
}

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
pub struct ClockSignal(watch::Sender<Option<UnixMillis>>);

impl ClockSignal {
    pub fn new(last_synced_at: Option<UnixMillis>) -> Self {
        Self(watch::channel(last_synced_at).0)
    }

    pub fn snapshot(&self) -> Option<UnixMillis> {
        *self.0.borrow()
    }

    pub fn valid(&self, now: UnixMillis) -> bool {
        self.status(now).0
    }

    /// Validity and timestamp from one snapshot, even while a check publishes.
    pub fn status(&self, now: UnixMillis) -> (bool, Option<UnixMillis>) {
        let last = self.snapshot();
        (last.is_some_and(|last| clock_signal_valid(last, now)), last)
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
    // Checks are serial: reuse a crash leftover instead of accumulating PID files.
    let mut name = path.as_os_str().to_owned();
    name.push(".tmp");
    let temp = PathBuf::from(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
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
    if let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        let error = StorageError::io("清理节点校时临时文件", error);
        tracing::error!(error = %Diagnostic(&error), "clock state cleanup failed");
    }
    result
}

fn remove_state(path: &Path) -> Result<(), StorageError> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(StorageError::io("删除节点校时状态文件", error)),
    }
    sync_directory(path)
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

fn network_error(operation: &'static str, error: std::io::Error) -> sntpc::Error {
    let error = StorageError::io(operation, error);
    tracing::debug!(error = %Diagnostic(&error), "NTP network unavailable");
    sntpc::Error::Network
}

impl NtpUdpSocket for Socket<'_> {
    async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> sntpc::Result<usize> {
        self.sent.store(self.clock.now().0, Ordering::Relaxed);
        self.udp
            .send_to(buf, addr)
            .await
            .map_err(|error| network_error("发送 NTP 查询", error))
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
    Synced(UnixMillis),
    Incorrect,
    NoReply,
}

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
            Err(error) => tracing::debug!(?error, %addr, "no valid NTP reply from address"),
        }
    }
    Ok(CheckResult::NoReply)
}

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
        CheckResult::Synced(UnixMillis(received))
    } else {
        CheckResult::Incorrect
    })
}

struct Checker {
    clock: Clock,
    servers: Vec<NtpServer>,
    retired: Vec<NtpServer>,
    path: PathBuf,
    signal: ClockSignal,
}

impl Checker {
    fn new(db_path: &Path, clock: Clock, servers: Vec<NtpServer>, signal: ClockSignal) -> Self {
        Self {
            clock,
            servers,
            retired: Vec::new(),
            path: state_path(db_path),
            signal,
        }
    }

    async fn check(&mut self, mut stop: Option<&mut watch::Receiver<bool>>) -> bool {
        let mut outcome = CheckResult::NoReply;
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
                _ = self.clock.sleep(Duration::from_secs(5)) => continue,
                result = query(server, &self.clock) => result,
            };
            match result {
                Ok(CheckResult::NoReply) => {}
                Ok(result) => {
                    outcome = result;
                    break;
                }
                Err(sntpc::Error::KissOfDeath(
                    code @ (KissOfDeathCode::Deny | KissOfDeathCode::Rstr),
                )) => {
                    tracing::warn!(host = %server.host, port = server.port, code = code.as_str(), "NTP server retired for this process");
                    self.retired.push(server.clone());
                }
                Err(error) => tracing::debug!(?error, host = %server.host, "no valid NTP reply"),
            }
        }
        let path = self.path.clone();
        let new_signal = match outcome {
            CheckResult::NoReply => return true,
            CheckResult::Synced(time) => Some(time),
            CheckResult::Incorrect => {
                self.signal.0.send_replace(None);
                None
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
                    self.signal.0.send_replace(new_signal);
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
        if !checker.signal.valid(clock.now())
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
        assert!(matches!(result, CheckResult::Synced(time) if time == clock.clock().now()));
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
        assert!(matches!(result, CheckResult::Incorrect));
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
    fn persistence_reuses_a_crash_leftover_and_truncates_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boh.db-clock-sync");
        let temp = dir.path().join("boh.db-clock-sync.tmp");
        std::fs::write(&path, "123456789\n").unwrap();
        std::fs::write(&temp, "interrupted incomplete write\n").unwrap();
        persist(&path, UnixMillis(42)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"42\n");
        assert!(!temp.exists());
        persist(&path, UnixMillis(43)).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

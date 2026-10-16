//! Node clock verification. Network waits are cancellable; durable publication is not.

use std::future::{Future, poll_fn};
use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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
        self.snapshot()
            .is_some_and(|last| clock_signal_valid(last, now))
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

static TEMP_NUMBER: AtomicU64 = AtomicU64::new(0);

fn persist(path: &Path, time: UnixMillis) -> Result<(), StorageError> {
    let (temp, mut file) = loop {
        let number = TEMP_NUMBER.fetch_add(1, Ordering::Relaxed);
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".tmp-{}-{number}", std::process::id()));
        let temp = PathBuf::from(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StorageError::io("创建节点校时临时文件", error)),
        }
    };
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
    let addr = tokio::net::lookup_host((server.host.as_str(), server.port))
        .await
        .map_err(|error| network_error("解析 NTP 服务器地址", error))?
        .next()
        .ok_or(sntpc::Error::AddressResolve)?;
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
    async fn check(&mut self, stop: &mut watch::Receiver<bool>) -> bool {
        let mut outcome = CheckResult::NoReply;
        for server in &self.servers {
            if *stop.borrow() {
                return false;
            }
            if self.retired.contains(server) {
                continue;
            }
            let result = tokio::select! {
                biased;
                _ = stop.changed() => return false,
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

pub async fn ensure_synchronized(
    db_path: &Path,
    clock: Clock,
    servers: Vec<NtpServer>,
) -> Result<ClockSignal, StorageError> {
    let signal = ClockSignal::load(db_path).await;
    if !signal.valid(clock.now()) {
        let (_keep_sender, mut stop) = watch::channel(false);
        let mut checker = Checker {
            clock: clock.clone(),
            servers,
            retired: Vec::new(),
            path: state_path(db_path),
            signal: signal.clone(),
        };
        checker.check(&mut stop).await;
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
        let mut checker = Checker {
            clock: clock.clone(),
            servers,
            retired: Vec::new(),
            path: state_path(db_path),
            signal,
        };
        let worker = tokio::spawn(async move {
            let completed = {
                let mut initial = std::pin::pin!(checker.check(&mut stopped));
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
            if !checker.check(&mut stopped).await {
                return;
            }
            checks = checks.saturating_add(1);
            last_check = clock.monotonic();
        }
    }
}

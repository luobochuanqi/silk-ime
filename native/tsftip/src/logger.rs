//! 进程独立的 logfmt 日志。按键回调只计数，现有连接线程定期输出摘要。
//! 文件满时仅清空自己的文件；跨进程非阻塞文件锁协调目录配额，争用时丢日志。
//! 仅管理严格命名的普通 tsftip 文件；活跃句柄禁止删除，配额不足时拒写。

use std::cell::Cell;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Seek as _, Write as _};
use std::marker::PhantomData;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use log::kv::{Error as KvError, Key, Value, VisitSource};
use log::{LevelFilter, Log, Metadata, Record};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
};
use windows::Win32::System::IO::OVERLAPPED;

const MAX_BYTES: u64 = 1 << 20;
const TOTAL_BYTES: u64 = 16 << 20;
const MAX_FILES: usize = 64;
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SUMMARY_INTERVAL: u64 = 5;

thread_local! {
    static HOT_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// 覆盖整个回调及同步重入；日志门面在格式化、取锁和文件操作前拒绝输出。
pub struct HotPath(PhantomData<Rc<()>>);

pub fn hot_path() -> HotPath {
    HOT_DEPTH.with(|depth| depth.set(depth.get() + 1));
    HotPath(PhantomData)
}

fn is_hot() -> bool {
    HOT_DEPTH.with(|depth| depth.get() != 0)
}

impl Drop for HotPath {
    fn drop(&mut self) {
        HOT_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

#[derive(Clone, Copy)]
pub enum Event {
    Key,
    Pass,
    EditError,
    Timeout,
    Disconnect,
    Expired,
    Stale,
    BadFrame,
    HostError,
    ConnectError,
    Connected,
    Spawned,
    Panic,
}

static COUNTS: [AtomicU64; 13] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

pub fn count(event: Event) {
    COUNTS[event as usize].fetch_add(1, Ordering::Relaxed);
}

/// 固定数值快照，不保存输入。result：0 应答、1 超时、2 断连、3 过期、4 host 拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestSample {
    pub id: u64,
    pub elapsed_us: u64,
    pub result: u8,
    pub frames: usize,
}

struct RequestSamples(Mutex<Option<RequestSample>>);

impl RequestSamples {
    fn publish(&self, sample: RequestSample) {
        if let Ok(mut latest) = self.0.try_lock() {
            *latest = Some(sample);
        }
    }

    fn take(&self) -> Option<RequestSample> {
        self.0.try_lock().ok()?.take()
    }
}

static REQUESTS: RequestSamples = RequestSamples(Mutex::new(None));

/// worker 通知 TSF 应答后调用；争用即丢弃，不等待摘要线程，也不格式化或落盘。
pub fn sample_request(sample: RequestSample) {
    REQUESTS.publish(sample);
}

/// 只在现有连接线程调用；进程内多个 TIP 共用五秒窗口，不逐请求写文件。
pub fn summarize() {
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(SUMMARY_INTERVAL);
    let now = CLOCK.get_or_init(Instant::now).elapsed().as_secs();
    let next = NEXT.load(Ordering::Relaxed);
    if now < next
        || NEXT
            .compare_exchange(
                next,
                now + SUMMARY_INTERVAL,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_err()
    {
        return;
    }
    let counts: [u64; 13] = std::array::from_fn(|i| COUNTS[i].swap(0, Ordering::Relaxed));
    let n = |event: Event| counts[event as usize];
    if counts[2..10].iter().any(|&n| n != 0) || n(Event::Panic) != 0 {
        log::warn!(edit_errors = n(Event::EditError), timeouts = n(Event::Timeout),
            disconnects = n(Event::Disconnect), expired = n(Event::Expired),
            stale = n(Event::Stale), bad_frames = n(Event::BadFrame),
            host_errors = n(Event::HostError), connect_errors = n(Event::ConnectError),
            panics = n(Event::Panic); "IPC 与编辑故障摘要");
    }
    if n(Event::Connected) != 0 || n(Event::Spawned) != 0 {
        log::info!(connected = n(Event::Connected), spawned = n(Event::Spawned); "连接生命周期摘要");
    }
    if n(Event::Key) != 0 || n(Event::Pass) != 0 {
        log::debug!(keys = n(Event::Key), passthrough = n(Event::Pass); "按键处理摘要");
    }
    if let Some(sample) = REQUESTS.take() {
        // 取出完整快照后已释放锁；这不是完整逐请求轨迹。
        log::debug!(sampled = true, id = sample.id, elapsed_us = sample.elapsed_us,
            result = sample.result, frames = sample.frames; "最近请求抽样");
    }
}

struct Disk {
    coordinator: File,
    file: Option<File>,
}

struct Logger {
    path: PathBuf,
    origin: String,
    disk: Mutex<Disk>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// 不在 DllMain 或按键回调初始化。初始化失败时静默禁用日志。
pub fn init() {
    if is_hot() {
        return;
    }
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let level = parse_level(std::env::var("SILK_LOG").ok().as_deref());
        if level == LevelFilter::Off {
            return;
        }
        let Some(base) = std::env::var_os("LOCALAPPDATA") else {
            return;
        };
        let dir = PathBuf::from(base).join("Silk").join("logs");
        let Ok(logger) = Logger::new(&dir) else {
            return;
        };
        let logger = LOGGER.get_or_init(|| logger);
        if log::set_logger(logger).is_ok() {
            log::set_max_level(level);
            install_panic_hook();
            if std::env::var("SILK_LOG_TEXT").is_ok_and(|v| v == "1") {
                // 即使设为 error 也只在初始化时提醒一次；本 DLL 仍不记录输入文本。
                logger.emit(
                    &Record::builder()
                        .level(log::Level::Warn)
                        .target("logger")
                        .args(format_args!(
                            "SILK_LOG_TEXT=1 已开启，其他组件可能记录用户文本"
                        ))
                        .build(),
                );
            }
        }
    });
}

fn parse_level(value: Option<&str>) -> LevelFilter {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("off") => LevelFilter::Off,
        Some("error") => LevelFilter::Error,
        Some("warn" | "warning") => LevelFilter::Warn,
        Some("debug") => LevelFilter::Debug,
        Some("trace") => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        !is_hot() && metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.emit(record);
        }
    }

    fn flush(&self) {}
}

impl Logger {
    fn new(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let coordinator = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .share_mode(3)
            .open(dir.join("tsftip.lock"))?;
        // 系统时钟粒度较粗或回拨时，同进程的初始化仍使用唯一标识。
        static LAST_START: AtomicU64 = AtomicU64::new(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::other("系统时间无效"))?
            .as_nanos() as u64;
        let previous = LAST_START
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                Some(now.max(last + 1))
            })
            .expect("更新闭包始终返回 Some");
        let start = now.max(previous + 1);
        Ok(Self {
            path: dir.join(format!("tsftip-{}-{start:032x}.log", std::process::id())),
            origin: format!("pid={} app={}", std::process::id(), quote(&exe_name())),
            disk: Mutex::new(Disk {
                coordinator,
                file: None,
            }),
        })
    }

    fn emit(&self, record: &Record) {
        // 内部直调（一次性警告）也不能绕过热路径屏障。
        if is_hot() {
            return;
        }
        let mut line = String::with_capacity(192);
        let module = record
            .target()
            .rsplit("::")
            .next()
            .unwrap_or(record.target());
        let _ = write!(
            line,
            "time={} level={} {} tid={} mod={} msg={}",
            local_time(),
            record.level(),
            self.origin,
            thread_id(),
            quote(module),
            quote(&record.args().to_string())
        );
        let _ = record.key_values().visit(&mut Pairs(&mut line));
        line.push('\n');
        self.append(line.as_bytes());
    }

    fn append(&self, bytes: &[u8]) {
        if is_hot() || bytes.is_empty() || bytes.len() as u64 > MAX_BYTES {
            return;
        }
        // 不因其它线程或进程正在写日志而等待；按键线程更不会触及这把锁。
        let Ok(mut disk) = self.disk.try_lock() else {
            return;
        };
        let Disk { coordinator, file } = &mut *disk;
        let Some(_lock) = DirectoryLock::acquire(coordinator) else {
            return;
        };
        let _ = self.append_locked(file, bytes);
    }

    fn append_locked(&self, file: &mut Option<File>, bytes: &[u8]) -> io::Result<()> {
        let len = bytes.len() as u64;
        let old = match file.as_ref() {
            Some(file) => file.metadata()?.len(),
            None => 0,
        };
        let rotate = old.saturating_add(len) > MAX_BYTES;
        let mut entries = managed_files(self.path.parent().expect("日志路径有父目录"))?;
        let now = SystemTime::now();
        entries.retain(|entry| {
            let expired = now
                .duration_since(entry.modified)
                .is_ok_and(|age| age >= RETENTION);
            !(expired && entry.path != self.path && std::fs::remove_file(&entry.path).is_ok())
        });
        entries.sort_unstable_by_key(|entry| entry.modified);
        let mut total = entries
            .iter()
            .try_fold(0u64, |sum, entry| sum.checked_add(entry.len))
            .ok_or_else(|| io::Error::other("日志总量溢出"))?;
        let mut count = entries.len();
        let adding = usize::from(file.is_none());
        let growth = if rotate {
            len as i128 - old as i128
        } else {
            len as i128
        };
        for entry in &entries {
            if total as i128 + growth <= TOTAL_BYTES as i128 && count + adding <= MAX_FILES {
                break;
            }
            if entry.path != self.path && std::fs::remove_file(&entry.path).is_ok() {
                total -= entry.len;
                count -= 1;
            }
        }
        if total as i128 + growth > TOTAL_BYTES as i128 || count + adding > MAX_FILES {
            return Ok(());
        }
        if file.is_none() {
            // 不覆盖同名文件；持有句柄期间禁止其它进程写入或删除。
            *file = Some(
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .share_mode(1)
                    .open(&self.path)?,
            );
        }
        if let Some(file) = file {
            if rotate {
                file.set_len(0)?;
                file.rewind()?;
            }
            file.write_all(bytes)?;
        }
        Ok(())
    }
}

/// 锁文件大小恒为零，句柄不允许删除；崩溃时内核自动释放字节范围锁。
struct DirectoryLock<'a>(&'a File);

impl<'a> DirectoryLock<'a> {
    fn acquire(file: &'a File) -> Option<Self> {
        let mut offset = OVERLAPPED::default();
        // SAFETY: 同步文件句柄有效，锁操作立即成功或失败，不保留栈指针。
        unsafe {
            LockFileEx(
                HANDLE(file.as_raw_handle()),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                None,
                1,
                0,
                &mut offset,
            )
            .ok()?;
        }
        Some(Self(file))
    }
}

impl Drop for DirectoryLock<'_> {
    fn drop(&mut self) {
        let mut offset = OVERLAPPED::default();
        // SAFETY: 本守卫独占偏移零的一个字节，句柄仍然有效。
        unsafe {
            let _ = UnlockFileEx(HANDLE(self.0.as_raw_handle()), None, 1, 0, &mut offset);
        }
    }
}

struct Entry {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

fn managed_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix("tsftip-")
        .and_then(|s| s.strip_suffix(".log"))
    else {
        return false;
    };
    let Some((pid, start)) = body.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && pid.parse::<u32>().is_ok_and(|pid| pid != 0)
        && start.len() == 32
        && start.bytes().all(|b| b.is_ascii_hexdigit())
}

fn managed_files(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(managed_name) {
            continue;
        }
        let metadata = entry.metadata()?;
        // 不追踪符号链接或其它重解析点，也不删除同名目录。
        if !entry.file_type()?.is_file() || metadata.file_attributes() & 0x400 != 0 {
            continue;
        }
        entries.push(Entry {
            path: entry.path(),
            len: metadata.len(),
            modified: metadata.modified()?,
        });
    }
    Ok(entries)
}

/// 把 kv 对追加为 ` key=value`。
struct Pairs<'a>(&'a mut String);

impl<'kvs> VisitSource<'kvs> for Pairs<'_> {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), KvError> {
        let _ = write!(self.0, " {}={}", key.as_str(), quote(&value.to_string()));
        Ok(())
    }
}

/// logfmt 取值：含空白、引号或 `=` 的值加引号转义，与 Go slog TextHandler 一致。
fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '=' || c.is_control());
    if plain {
        return value.to_owned();
    }
    format!("{value:?}")
}

fn local_time() -> String {
    // SAFETY: GetLocalTime 无失败路径，只读系统时钟。
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

fn thread_id() -> u32 {
    // SAFETY: 无参数、无失败路径。
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// 不格式化 panic 载荷；热路径仅计数。宿主原有 hook 的行为不属于本日志输出。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_hot() {
            count(Event::Panic);
        } else if let Some(location) = info.location() {
            log::error!(file = location.file(), line = location.line(); "panic（载荷已省略）");
        } else {
            log::error!("panic（载荷已省略）");
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_matches_logfmt() {
        assert_eq!(quote("s-1"), "s-1");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("连接 建立"), "\"连接 建立\"");
        assert_eq!(quote("a=b"), "\"a=b\"");
        assert_eq!(quote("x\"y"), "\"x\\\"y\"");
    }

    #[test]
    fn level_defaults_to_info() {
        assert_eq!(parse_level(None), LevelFilter::Info);
        assert_eq!(parse_level(Some("DEBUG")), LevelFilter::Debug);
        assert_eq!(parse_level(Some("off")), LevelFilter::Off);
        assert_eq!(parse_level(Some("bogus")), LevelFilter::Info);
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "silk-log-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn seed(dir: &Path, id: usize, len: u64) -> PathBuf {
        let path = dir.join(format!("tsftip-123-{id:032x}.log"));
        File::create(&path).unwrap().set_len(len).unwrap();
        path
    }

    #[test]
    fn ownership_names_are_strict() {
        assert!(managed_name(
            "tsftip-123-0123456789abcdef0123456789abcdef.log"
        ));
        for name in [
            "tsftip.log",
            "tsftip.1.log",
            "silkhost.log",
            "tsftip-1-a.log",
            "tsftip-0-0123456789abcdef0123456789abcdef.log",
            "tsftip-123-0123456789abcdef0123456789abcdef.log.bak",
        ] {
            assert!(!managed_name(name), "{name}");
        }
    }

    #[test]
    fn hot_scope_never_formats_or_touches_disk() {
        struct Secret;
        impl std::fmt::Display for Secret {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("热路径不得格式化输入");
            }
        }
        let dir = TempDir::new();
        let logger = Logger::new(&dir.0).unwrap();
        {
            let _hot = hot_path();
            {
                let _nested = hot_path();
                logger.emit(&Record::builder().args(format_args!("{}", Secret)).build());
                logger.append(b"secret");
            }
            assert!(is_hot());
            assert!(managed_files(&dir.0).unwrap().is_empty());
        }
        assert!(!is_hot());
        logger.append(b"safe\n");
        assert_eq!(std::fs::read(&logger.path).unwrap(), b"safe\n");
    }

    #[test]
    fn exact_file_limit_rotates_and_rejects_oversized_record() {
        let dir = TempDir::new();
        let logger = Logger::new(&dir.0).unwrap();
        let full = vec![b'x'; MAX_BYTES as usize];
        logger.append(&full);
        assert_eq!(std::fs::metadata(&logger.path).unwrap().len(), MAX_BYTES);
        logger.append(b"latest\n");
        assert_eq!(std::fs::read(&logger.path).unwrap(), b"latest\n");
        logger.append(&vec![b'x'; MAX_BYTES as usize + 1]);
        assert_eq!(std::fs::read(&logger.path).unwrap(), b"latest\n");
    }

    #[test]
    fn total_quota_preserves_active_and_unrelated_files() {
        let dir = TempDir::new();
        let unrelated = dir.0.join("tsftip.log");
        std::fs::write(&unrelated, b"legacy").unwrap();
        let same_name_directory = dir
            .0
            .join("tsftip-123-ffffffffffffffffffffffffffffffff.log");
        std::fs::create_dir(&same_name_directory).unwrap();
        let mut active = Vec::new();
        for _ in 0..TOTAL_BYTES / MAX_BYTES {
            let logger = Logger::new(&dir.0).unwrap();
            logger.append(&vec![b'x'; MAX_BYTES as usize]);
            assert!(logger.path.exists());
            active.push(logger);
        }
        let denied = Logger::new(&dir.0).unwrap();
        denied.append(b"cannot fit\n");
        assert!(!denied.path.exists());
        assert_eq!(
            managed_files(&dir.0)
                .unwrap()
                .iter()
                .map(|e| e.len)
                .sum::<u64>(),
            TOTAL_BYTES
        );
        drop(active.pop());
        denied.append(b"can fit\n");
        assert!(denied.path.exists());
        assert_eq!(std::fs::read(unrelated).unwrap(), b"legacy");
        assert!(same_name_directory.is_dir());
    }

    #[test]
    fn file_count_reclaims_inactive_files_and_refuses_active_overflow() {
        let dir = TempDir::new();
        let mut active = Vec::new();
        for _ in 0..MAX_FILES {
            let logger = Logger::new(&dir.0).unwrap();
            logger.append(b"x");
            assert!(logger.path.exists());
            active.push(logger);
        }
        let denied = Logger::new(&dir.0).unwrap();
        denied.append(b"y");
        assert!(!denied.path.exists());
        drop(active.pop());
        denied.append(b"y");
        assert!(denied.path.exists());
        assert_eq!(managed_files(&dir.0).unwrap().len(), MAX_FILES);
    }

    #[test]
    fn directory_lock_contender_drops_record_without_creating_file() {
        let dir = TempDir::new();
        let first = Logger::new(&dir.0).unwrap();
        let second = Logger::new(&dir.0).unwrap();
        let disk = first.disk.lock().unwrap();
        let guard = DirectoryLock::acquire(&disk.coordinator).unwrap();
        second.append(b"contended\n");
        assert!(!second.path.exists());
        drop(guard);
        second.append(b"available\n");
        assert!(second.path.exists());
    }

    #[test]
    fn quota_child() {
        let Some(dir) = std::env::var_os("SILK_TEST_LOG_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let mut active = Vec::new();
        for _ in 0..8 {
            let logger = Logger::new(&dir).unwrap();
            for _ in 0..4 {
                logger.append(&vec![b'x'; MAX_BYTES as usize]);
                std::thread::yield_now();
            }
            active.push(logger);
        }
    }

    #[test]
    fn concurrent_processes_share_one_directory_budget() {
        let dir = TempDir::new();
        let mut children = Vec::new();
        for _ in 0..4 {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "logger::regression_tests::quota_child",
                        "--nocapture",
                    ])
                    .env("SILK_TEST_LOG_DIR", &dir.0)
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let entries = managed_files(&dir.0).unwrap();
        assert!(!entries.is_empty());
        assert!(entries.len() <= MAX_FILES);
        assert!(entries.iter().map(|entry| entry.len).sum::<u64>() <= TOTAL_BYTES);
    }

    #[test]
    fn inactive_over_quota_files_are_reclaimed() {
        let dir = TempDir::new();
        for id in 0..MAX_FILES + 1 {
            seed(&dir.0, id, MAX_BYTES);
        }
        let logger = Logger::new(&dir.0).unwrap();
        logger.append(b"new\n");
        let entries = managed_files(&dir.0).unwrap();
        assert!(logger.path.exists());
        assert!(entries.len() <= MAX_FILES);
        assert!(entries.iter().map(|entry| entry.len).sum::<u64>() <= TOTAL_BYTES);
    }

    #[test]
    fn retention_removes_only_expired_inactive_owned_files() {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::Storage::FileSystem::SetFileTime;
        let dir = TempDir::new();
        let expired = seed(&dir.0, 1, 1);
        let recent = seed(&dir.0, 2, 1);
        let file = OpenOptions::new().write(true).open(&expired).unwrap();
        // SAFETY: 句柄有效；时间常量远早于保留期限，调用不保留指针。
        let time = FILETIME {
            dwLowDateTime: 1,
            dwHighDateTime: 0,
        };
        unsafe {
            SetFileTime(HANDLE(file.as_raw_handle()), None, None, Some(&time)).unwrap();
        }
        drop(file);
        let logger = Logger::new(&dir.0).unwrap();
        logger.append(b"new\n");
        assert!(!expired.exists());
        assert!(recent.exists());
    }

    #[test]
    fn privacy_child() {
        if std::env::var_os("SILK_TEST_PRIVACY").is_none() {
            return;
        }
        init();
        init();
        let _ = std::panic::catch_unwind(|| panic!("PRIVATE_PANIC_8e6b"));
        {
            let _hot = hot_path();
            log::debug!(code = 65; "PRIVATE_KEY_8e6b");
            log::warn!("PRIVATE_EDIT_ERROR_8e6b");
            let _ = std::panic::catch_unwind(|| panic!("PRIVATE_HOT_PANIC_8e6b"));
        }
    }

    #[test]
    fn debug_and_text_opt_in_never_expose_panic_or_callback_payloads() {
        for text in ["0", "1"] {
            let dir = TempDir::new();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "logger::regression_tests::privacy_child",
                    "--nocapture",
                ])
                .env("LOCALAPPDATA", &dir.0)
                .env("SILK_LOG", "debug")
                .env("SILK_LOG_TEXT", text)
                .env("SILK_TEST_PRIVACY", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let entries = managed_files(&dir.0.join("Silk").join("logs")).unwrap();
            assert_eq!(entries.len(), 1);
            let content = std::fs::read_to_string(&entries[0].path).unwrap();
            assert!(content.contains("载荷已省略"));
            assert!(!content.contains("PRIVATE_"));
            assert!(!content.contains("code=65"));
            assert_eq!(
                content.matches("SILK_LOG_TEXT=1").count(),
                usize::from(text == "1")
            );
        }
    }

    #[test]
    fn request_sample_is_latest_complete_snapshot_and_nonblocking() {
        let samples = RequestSamples(Mutex::new(None));
        let first = RequestSample {
            id: 7,
            elapsed_us: 123,
            result: 0,
            frames: 2,
        };
        let latest = RequestSample {
            id: 8,
            elapsed_us: 456,
            result: 4,
            frames: 1,
        };
        samples.publish(first);
        {
            let held = samples.0.lock().unwrap();
            // 同线程持锁仍立即返回；不能等待锁或覆盖部分字段。
            samples.publish(latest);
            assert_eq!(samples.take(), None);
            assert_eq!(*held, Some(first));
        }
        samples.publish(latest);
        assert_eq!(samples.take(), Some(latest));
        assert_eq!(samples.take(), None);
    }
}

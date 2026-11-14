//! 进程内日志：`log` 门面 + logfmt 文件输出，与 silkhost 日志同格式。
//!
//! 本 DLL 进入每个有文本输入的进程，因此日志设计以「不拖累宿主」为先：
//! - 所有进程追加写同一个 `%LOCALAPPDATA%\Silk\logs\tsftip.log`，一行一次 WriteFile
//!   （追加模式下单次写入是原子的），行内带 pid/tid/app 区分来源；
//! - 同步写、不起线程：崩溃前最后几行才最有价值，异步队列会把它们丢掉；
//! - 默认 info 级别只记生命周期与异常，热路径逐键日志在 debug 级别，关闭时
//!   开销是 `log` 门面的一次整数比较；
//! - 超过 1 MiB 滚动为 `tsftip.1.log`（仅保留一份），多进程并发滚动以路径上的
//!   实际大小为准，避免互相覆盖。
//!
//! 级别取环境变量 `SILK_LOG`（debug/info/warn/error/off，与 host 共用），
//! `SILK_LOG_TEXT=1` 时记录用户文本，否则只记字符数（08 隐私约束）。

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, Once, OnceLock};

use log::kv::{Error as KvError, Key, Value, VisitSource};
use log::{LevelFilter, Log, Metadata, Record};

const ENV_LEVEL: &str = "SILK_LOG";
const ENV_TEXT: &str = "SILK_LOG_TEXT";
const MAX_BYTES: u64 = 1 << 20;

struct Logger {
    path: PathBuf,
    /// 行内固定前缀：`pid=… app=…`。
    origin: String,
    text: bool,
    file: Mutex<Option<File>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// 初始化日志（幂等）。不得在 DllMain 中调用：会读环境变量、建目录。
pub fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let level = parse_level(std::env::var(ENV_LEVEL).ok().as_deref());
        if level == LevelFilter::Off {
            return;
        }
        let Some(base) = std::env::var_os("LOCALAPPDATA") else {
            return;
        };
        let dir = PathBuf::from(base).join("Silk").join("logs");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let logger = LOGGER.get_or_init(|| Logger {
            path: dir.join("tsftip.log"),
            origin: format!("pid={} app={}", std::process::id(), quote(&exe_name())),
            text: std::env::var(ENV_TEXT).is_ok_and(|v| v == "1"),
            file: Mutex::new(None),
        });
        if log::set_logger(logger).is_ok() {
            log::set_max_level(level);
            install_panic_hook();
        }
    });
}

/// 用户文本的日志表示：开启 `SILK_LOG_TEXT` 时为原文，否则为字符数。
pub fn text(s: &str) -> String {
    if LOGGER.get().is_some_and(|l| l.text) {
        s.to_owned()
    } else {
        format!("<{}字>", s.chars().count())
    }
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
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
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
            module,
            quote(&record.args().to_string()),
        );
        let _ = record.key_values().visit(&mut Pairs(&mut line));
        line.push('\n');
        self.append(line.as_bytes());
    }

    fn flush(&self) {}
}

impl Logger {
    fn append(&self, bytes: &[u8]) {
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let full = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() >= MAX_BYTES);
        if full {
            // 句柄所指文件可能已被别的进程滚动走：路径上的文件仍超限才由本进程滚动。
            if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= MAX_BYTES) {
                let _ = std::fs::rename(&self.path, self.path.with_extension("1.log"));
            }
            *file = None;
        }
        if file.is_none() {
            *file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = file.as_mut() {
            let _ = f.write_all(bytes);
        }
    }
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

/// panic 发生在宿主进程里：先落一条带位置的 error 日志，再交还原有处理。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let location = info.location().map(ToString::to_string).unwrap_or_default();
        log::error!(location = location.as_str(); "panic: {payload}");
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

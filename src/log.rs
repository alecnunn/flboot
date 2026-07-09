use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn level_color(level: &str) -> &'static str {
    match level {
        "TRACE" => "\x1b[35m",
        "DEBUG" => "\x1b[34m",
        "INFO" => "\x1b[32m",
        "WARN" => "\x1b[33m",
        "ERROR" => "\x1b[31m",
        _ => "",
    }
}

#[cfg(windows)]
fn enable_windows_ansi() -> bool {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_windows_ansi() -> bool {
    true
}

fn supports_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if !std::io::stdout().is_terminal() {
        return false;
    }
    enable_windows_ansi()
}

/// Whether stdout should carry ANSI escapes: a terminal, `NO_COLOR` unset, and
/// (on Windows) virtual-terminal processing successfully enabled. Probed once.
pub fn color_enabled() -> bool {
    static COLOR: OnceLock<bool> = OnceLock::new();
    *COLOR.get_or_init(supports_color)
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's
/// `civil_from_days` algorithm (public domain), used instead of pulling in a
/// full date/time crate just for log timestamps.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn utc_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let days = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

pub fn log(message: &str, level: &str) {
    let ts = utc_timestamp();
    let lvl = format!("{:>5}", level);
    if color_enabled() {
        println!("{DIM}{ts}{RESET} {}{lvl}{RESET} {message}", level_color(level));
    } else {
        println!("{ts} {lvl} {message}");
    }
}

pub fn info(message: &str) {
    log(message, "INFO");
}
pub fn warn(message: &str) {
    log(message, "WARN");
}
pub fn error(message: &str) {
    log(message, "ERROR");
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn handles_leap_day() {
        // 2024-02-29 is 19782 days after the epoch.
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
    }

    #[test]
    fn handles_year_boundary() {
        // 2000-01-01 is 10957 days after the epoch.
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
    }
}

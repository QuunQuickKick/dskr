//! 更新日時の表示用フォーマット。

/// エポック秒をローカル時刻の `YYYY-MM-DD HH:MM` にする。
/// ローカル時刻を取得できない環境では UTC にフォールバックする。
pub fn local_datetime(secs: i64) -> String {
    match local_parts(secs) {
        Some((y, m, d, hh, mm)) => format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}"),
        None => utc_datetime(secs),
    }
}

/// エポック秒を UTC の `YYYY-MM-DD HH:MM` にする (テスト・フォールバック用)。
pub fn utc_datetime(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        sod / 3600,
        (sod % 3600) / 60
    )
}

#[cfg(unix)]
fn local_parts(secs: i64) -> Option<(i64, u32, u32, u32, u32)> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::localtime_r(&t, &mut tm) };
    if r.is_null() {
        return None;
    }
    Some((
        tm.tm_year as i64 + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
    ))
}

#[cfg(windows)]
fn local_parts(secs: i64) -> Option<(i64, u32, u32, u32, u32)> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

    // 1601-01-01 からの 100ns 単位。負の値は扱わない。
    if secs < 0 {
        return None;
    }
    let ticks = (secs as u128 + 11_644_473_600) * 10_000_000;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    if unsafe { FileTimeToSystemTime(&ft, &mut utc) } == 0 {
        return None;
    }
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } != 0;
    let st = if ok { local } else { utc };
    Some((
        st.wYear as i64,
        st.wMonth as u32,
        st.wDay as u32,
        st.wHour as u32,
        st.wMinute as u32,
    ))
}

#[cfg(not(any(unix, windows)))]
fn local_parts(_secs: i64) -> Option<(i64, u32, u32, u32, u32)> {
    None
}

/// 1970-01-01 からの日数を年月日へ変換する。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting() {
        assert_eq!(utc_datetime(0), "1970-01-01 00:00");
        assert_eq!(utc_datetime(1_704_067_200), "2024-01-01 00:00");
        assert_eq!(utc_datetime(1_704_067_200 + 3_661), "2024-01-01 01:01");
        assert_eq!(utc_datetime(-1), "1969-12-31 23:59");
    }

    #[test]
    fn local_format_has_expected_shape() {
        let s = local_datetime(1_704_067_200);
        assert_eq!(s.len(), 16, "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert_eq!(&s[10..11], " ");
        assert_eq!(&s[13..14], ":");
    }
}

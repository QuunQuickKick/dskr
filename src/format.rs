/// 人間が読めるサイズ・件数表記と、サイズ指定文字列の解析。
pub fn human_size(bytes: u64) -> String {
    SizeFormat::default().format(bytes)
}

/// 1024 基準 (IEC) か 1000 基準 (SI) か。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitSystem {
    Iec,
    Si,
}

impl UnitSystem {
    pub fn base(self) -> f64 {
        match self {
            UnitSystem::Iec => 1024.0,
            UnitSystem::Si => 1000.0,
        }
    }

    const fn prefix(self, exponent: u8) -> &'static str {
        const IEC: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
        const SI: [&str; 7] = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
        let table = match self {
            UnitSystem::Iec => IEC,
            UnitSystem::Si => SI,
        };
        let mut idx = exponent as usize;
        if idx >= table.len() {
            idx = table.len() - 1;
        }
        table[idx]
    }

    pub fn toggle(self) -> Self {
        match self {
            UnitSystem::Iec => UnitSystem::Si,
            UnitSystem::Si => UnitSystem::Iec,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UnitSystem::Iec => "IEC",
            UnitSystem::Si => "SI",
        }
    }
}

/// 自動単位か固定単位か。Fixed の値は 0=B, 1=KiB/KB, ... 6=EiB/EB。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitDisplay {
    Auto,
    Fixed(u8),
}

impl UnitDisplay {
    pub fn cycle(self) -> Self {
        match self {
            UnitDisplay::Auto => UnitDisplay::Fixed(0),
            UnitDisplay::Fixed(6) => UnitDisplay::Auto,
            UnitDisplay::Fixed(n) => UnitDisplay::Fixed(n + 1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeFormat {
    pub system: UnitSystem,
    pub display: UnitDisplay,
}

impl Default for SizeFormat {
    fn default() -> Self {
        SizeFormat {
            system: UnitSystem::Iec,
            display: UnitDisplay::Auto,
        }
    }
}

impl SizeFormat {
    pub fn format(&self, bytes: u64) -> String {
        match self.display {
            UnitDisplay::Auto => {
                let base = self.system.base();
                let mut value = bytes as f64;
                let mut exponent = 0u8;
                while value >= base && exponent < 6 {
                    value /= base;
                    exponent += 1;
                }
                if exponent == 0 {
                    format!("{bytes} B")
                } else {
                    format!("{value:.1} {}", self.system.prefix(exponent))
                }
            }
            UnitDisplay::Fixed(0) => format!("{} B", human_count(bytes)),
            UnitDisplay::Fixed(n) => {
                let base = self.system.base();
                let value = bytes as f64 / base.powi(n as i32);
                format!("{value:.1} {}", self.system.prefix(n))
            }
        }
    }

    /// 表示単位そのもののラベル (自動 / B / KiB / ...)。
    pub fn display_label(&self) -> String {
        match self.display {
            UnitDisplay::Auto => "自動".to_string(),
            UnitDisplay::Fixed(n) => self.system.prefix(n).to_string(),
        }
    }

    /// 単位系のラベル (IEC / SI)。
    pub fn system_label(&self) -> &'static str {
        self.system.label()
    }

    /// 単位系の説明 (1024基準 / 1000基準)。
    pub fn system_description(&self) -> &'static str {
        match self.system {
            UnitSystem::Iec => "1024基準",
            UnitSystem::Si => "1000基準",
        }
    }

    pub fn toggle_system(&mut self) {
        self.system = self.system.toggle();
    }

    pub fn cycle_fixed(&mut self) {
        self.display = self.display.cycle();
    }
}

pub fn human_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// "10M" / "512" / "2g" のような文字列をバイト数へ変換する。
pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (num, mul) = match t.as_bytes()[t.len() - 1].to_ascii_lowercase() {
        b'k' => (&t[..t.len() - 1], 1u64 << 10),
        b'm' => (&t[..t.len() - 1], 1u64 << 20),
        b'g' => (&t[..t.len() - 1], 1u64 << 30),
        b't' => (&t[..t.len() - 1], 1u64 << 40),
        b'b' | b'0'..=b'9' => (t, 1),
        _ => return None,
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .map(|n| n.saturating_mul(mul))
}

pub fn pct(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

/// 端末へ出力する文字列を無害化する。
///
/// ファイル名やエラーメッセージには ESC などの制御文字が含まれ得るため、
/// そのまま描画すると端末エスケープシーケンス注入 (画面改変、OSC 52 による
/// クリップボード操作など) が成立してしまう。表示直前は必ずこれを通す。
/// 併せて双方向制御文字 (RTL 上書き等) を除去し、表示偽装を防ぐ。
pub fn sanitize_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() {
            // タブは空白、その他の制御文字は可視の代替文字にする
            out.push(if c == '\t' { ' ' } else { '?' });
            continue;
        }
        if is_bidi_control(c) {
            continue;
        }
        out.push(c);
    }
    out
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(human_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn counts() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(1234), "1,234");
        assert_eq!(human_count(1_234_567), "1,234,567");
    }

    #[test]
    fn size_parse() {
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("10M"), Some(10 << 20));
        assert_eq!(parse_size("2g"), Some(2 * (1 << 30)));
        assert_eq!(parse_size("1t"), Some(1u64 << 40));
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn percentages() {
        assert_eq!(pct(50, 100), 50.0);
        assert_eq!(pct(1, 0), 0.0);
    }

    #[test]
    fn size_format_systems() {
        let iec = SizeFormat::default();
        assert_eq!(iec.format(1024), "1.0 KiB");
        let si = SizeFormat {
            system: UnitSystem::Si,
            display: UnitDisplay::Auto,
        };
        assert_eq!(si.format(1000), "1.0 KB");
        assert_eq!(si.format(1500), "1.5 KB");
        assert_eq!(si.format(999), "999 B");
    }

    #[test]
    fn size_format_fixed() {
        let mut f = SizeFormat {
            system: UnitSystem::Iec,
            display: UnitDisplay::Fixed(0),
        };
        assert_eq!(f.format(1234), "1,234 B");
        f.display = UnitDisplay::Fixed(2);
        assert_eq!(f.format(1_048_576), "1.0 MiB");
        let si = SizeFormat {
            system: UnitSystem::Si,
            display: UnitDisplay::Fixed(2),
        };
        assert_eq!(si.format(1_000_000), "1.0 MB");
        assert_eq!(si.display_label(), "MB");
        assert_eq!(si.system_label(), "SI");
        assert_eq!(si.system_description(), "1000基準");
        assert_eq!(SizeFormat::default().display_label(), "自動");
        assert_eq!(SizeFormat::default().system_label(), "IEC");
    }

    #[test]
    fn size_format_cycles() {
        let mut f = SizeFormat::default();
        f.toggle_system();
        assert_eq!(f.system, UnitSystem::Si);
        f.cycle_fixed();
        assert_eq!(f.display, UnitDisplay::Fixed(0));
        for _ in 0..7 {
            f.cycle_fixed();
        }
        assert_eq!(f.display, UnitDisplay::Auto);
    }

    #[test]
    fn sanitize_blocks_terminal_escapes() {
        // ESC, BEL, 改行, 復帰を除去/置換する
        let s = sanitize_display("a\u{1b}]52;c;ZXZpbA==\u{7}\nb\rc");
        assert!(!s.contains('\u{1b}'));
        assert!(!s.contains('\u{7}'));
        assert!(!s.contains('\n'));
        assert!(!s.contains('\r'));
        assert!(s.starts_with("a?]52;c;ZXZpbA==?"));
        // 双方向制御文字は除去
        assert_eq!(sanitize_display("ab\u{202E}cd\u{2066}e"), "abcde");
        // タブは空白
        assert_eq!(sanitize_display("a\tb"), "a b");
        // 通常の Unicode は保持
        assert_eq!(
            sanitize_display("日本語/ファイル.txt"),
            "日本語/ファイル.txt"
        );
    }
}

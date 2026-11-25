use std::ffi::{OsStr, OsString};
use std::path::Path;

/// 走査対象ルートの種別。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    Local,
    /// \\server\share 形式 (Windows)
    Unc,
    /// Z:\ 形式のネットワークドライブ (Windows)
    MappedNetwork,
}

impl RootKind {
    pub fn label(self) -> &'static str {
        match self {
            RootKind::Local => "local",
            RootKind::Unc => "unc",
            RootKind::MappedNetwork => "mapped_network",
        }
    }

    pub fn is_network(self) -> bool {
        matches!(self, RootKind::Unc | RootKind::MappedNetwork)
    }

    /// 画面表示用のやさしいラベル。
    pub fn display_label(self) -> &'static str {
        match self {
            RootKind::Local => "ローカル",
            RootKind::Unc => "ネットワーク (UNC)",
            RootKind::MappedNetwork => "ネットワークドライブ",
        }
    }
}

/// ルートパスを分類する。Windows 以外で UNC 風の入力は明示的に拒否する。
pub fn classify_root(p: &Path) -> Result<RootKind, String> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut comps = p.components();
        if let Some(c) = comps.next() {
            if let Component::Prefix(pc) = c {
                let pre = pc.kind();
                return match pre {
                    Prefix::UNC(_, share) | Prefix::VerbatimUNC(_, share) => {
                        if share.is_empty() {
                            Err("UNC パスには共有名が必要です (例: \\\\server\\share)".into())
                        } else {
                            Ok(RootKind::Unc)
                        }
                    }
                    Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                        Ok(if crate::scan::platform::drive_is_remote(d) {
                            RootKind::MappedNetwork
                        } else {
                            RootKind::Local
                        })
                    }
                    _ => Err("パスの形式を解釈できません".into()),
                };
            }
        }
        Ok(RootKind::Local)
    }
    #[cfg(not(windows))]
    {
        let bytes = p.as_os_str().as_encoded_bytes();
        if bytes.starts_with(b"\\\\") {
            return Err("UNC パスは Windows でのみサポートされています".into());
        }
        if bytes.starts_with(b"//") && bytes.contains(&b'\\') {
            return Err("UNC パスは Windows でのみサポートされています".into());
        }
        Ok(RootKind::Local)
    }
}

/// 長いパス (>260 文字) を Windows の拡張長表記へ変換する。長くない場合はそのまま。
#[cfg(windows)]
pub fn to_extended(p: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let bytes = p.as_os_str().as_encoded_bytes();
    let too_long = bytes.len() >= 240;
    // `..` や `.` を含むと拡張長表記は正規化されないため、そのまま使う
    let has_dot = p
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !too_long || has_dot || bytes.starts_with(b"\\\\?\\") {
        return p.to_path_buf();
    }
    let mut out = std::ffi::OsString::new();
    if bytes.starts_with(b"\\\\") {
        out.push("\\\\?\\UNC\\");
        // 先頭 2 バイト (ASCII の '\') を除いた残りは WTF-8 として有効
        let rest = unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(&bytes[2..]) };
        out.push(rest);
    } else if p.is_absolute() {
        out.push("\\\\?\\");
        out.push(p.as_os_str());
    } else {
        return p.to_path_buf();
    }
    std::path::PathBuf::from(out)
}

/// パス表示を指定幅に収める (末尾を優先し、先頭を省略する)。
pub fn short_path_display(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if max == 0 {
        return String::new();
    }
    if s.width() <= max {
        return s.to_string();
    }
    let mut cut = s;
    while cut.width() > max.saturating_sub(1) {
        let mut chars = cut.chars();
        chars.next();
        cut = chars.as_str();
    }
    format!("…{cut}")
}

/// 簡易 glob (`*` と `?` のみ)。セパレータも `*` に含まれる点に注意。
pub fn glob_match(pattern: &str, name: &str) -> bool {
    #[cfg(windows)]
    let (pattern, name) = (pattern.to_lowercase(), name.to_lowercase());
    let pat = pattern.as_bytes();
    let nam = name.as_bytes();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_match = 0usize;
    while ni < nam.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == nam[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star = Some(pi);
            star_match = ni;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_match += 1;
            ni = star_match;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Linux の疑似ファイルシステム。ルート "/" を走査するときのみ除外する。
pub fn is_pseudo_path(p: &Path) -> bool {
    const PSEUDO: [&str; 4] = ["/proc", "/sys", "/dev", "/run"];
    PSEUDO.iter().any(|x| p.as_os_str() == OsStr::new(x))
}

/// 表示用にファイル名を文字列化する (非UTF-8は代替文字で表示)。
pub fn display_name(name: &OsStr) -> String {
    name.to_string_lossy().into_owned()
}

/// パスから分離要素名を取り出すヘルパ (競合回避のための薄いラッパ)。
pub fn file_name_of(p: &Path) -> OsString {
    p.file_name().unwrap_or(p.as_os_str()).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("*.txt", "a.txt"));
        assert!(glob_match("*.txt", "a.b.txt"));
        assert!(!glob_match("*.txt", "a.tx"));
        assert!(glob_match("a?.log", "ab.log"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("node_modules", "node_modules"));
        assert!(!glob_match("node_modules", "node_modules2"));
        assert!(!glob_match("*.log", "log"));
    }

    #[test]
    fn short_display() {
        assert_eq!(short_path_display("/a/b", 10), "/a/b");
        assert_eq!(
            short_path_display("/very/long/path/segments", 10),
            "…/segments"
        );
        assert_eq!(short_path_display("x", 0), "");
    }

    #[test]
    fn classify_unix_rejects_unc() {
        let r = classify_root(Path::new("\\\\server\\share"));
        assert!(r.is_err());
        assert_eq!(classify_root(Path::new("/home")).unwrap(), RootKind::Local);
        assert_eq!(classify_root(Path::new(".")).unwrap(), RootKind::Local);
    }

    #[test]
    fn pseudo() {
        assert!(is_pseudo_path(Path::new("/proc")));
        assert!(!is_pseudo_path(Path::new("/home/user/project")));
    }
}

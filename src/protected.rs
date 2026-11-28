//! 削除を拒否する保護パスの判定。
//!
//! `Exact` は指定したパス自身だけを、`Subtree` はその配下すべてを保護する。
//! シンボリックリンク / ジャンクションの削除では、リンク先ではなく
//! リンク自身の位置を判定する。リンク先を消さないため、リンク自身の削除は
//! リンク先が保護対象でも許可する。

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtectMode {
    Exact,
    Subtree,
}

#[derive(Clone, Debug)]
pub(crate) struct ProtectedPath {
    pub path: PathBuf,
    pub mode: ProtectMode,
}

/// 削除対象が保護パスに含まれるか判定する。
///
/// `link_like` が true のときは末端を解決せず「親 + 名前」で判定する。
/// ホーム直下の保護は、ファイル / ディレクトリ / リンクを問わず適用する。
pub(crate) fn check_protected_path(path: &Path, link_like: bool) -> Result<(), String> {
    check_protected_with(path, link_like, default_rules(), home_roots())
}

fn check_protected_with(
    path: &Path,
    link_like: bool,
    rules: &[ProtectedPath],
    home_roots: &[PathBuf],
) -> Result<(), String> {
    let target = normalize_target(path, link_like)?;
    for rule in rules {
        if rule_matches(&target, rule) {
            let label = match rule.mode {
                ProtectMode::Exact => "Exact",
                ProtectMode::Subtree => "Subtree",
            };
            return Err(format!(
                "保護対象のパスです ({label}): {}",
                target.display()
            ));
        }
    }
    // ホームルート直下は、ユーザーのホーム本体やプロファイル用のエントリ。
    // リンクやジャンクションでも、そのエントリ自体の削除は拒否する。
    if let Some(parent) = target.parent() {
        if home_roots.iter().any(|root| paths_equal(parent, root)) {
            return Err(format!(
                "ホームディレクトリ本体は削除できません: {}",
                target.display()
            ));
        }
    }
    Ok(())
}

fn normalize_target(path: &Path, link_like: bool) -> Result<PathBuf, String> {
    let abs = absolute_lexical(path)?;
    if link_like {
        let parent = abs
            .parent()
            .ok_or_else(|| "リンクの親ディレクトリを判定できません".to_string())?;
        let name = abs
            .file_name()
            .ok_or_else(|| "リンク名を判定できません".to_string())?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|e| format!("リンクの親ディレクトリを確認できません: {e}"))?;
        Ok(strip_verbatim(&parent).join(name))
    } else {
        let canonical =
            std::fs::canonicalize(&abs).map_err(|e| format!("保護対象かを確認できません: {e}"))?;
        Ok(strip_verbatim(&canonical))
    }
}

/// カレントディレクトリを基準に絶対パス化し、`.` と `..` を字句的に畳む。
fn absolute_lexical(path: &Path) -> Result<PathBuf, String> {
    let abs = std::path::absolute(path).map_err(|e| format!("パスを絶対パスにできません: {e}"))?;
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        return Err("パスを正規化できません".into());
    }
    Ok(out)
}

#[cfg(windows)]
fn strip_verbatim(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

#[cfg(not(windows))]
fn strip_verbatim(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn rule_matches(target: &Path, rule: &ProtectedPath) -> bool {
    match rule.mode {
        ProtectMode::Exact => paths_equal(target, &rule.path),
        ProtectMode::Subtree => path_starts_with(target, &rule.path),
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let a: Vec<_> = a.components().map(component_key).collect();
    let b: Vec<_> = b.components().map(component_key).collect();
    a == b
}

fn path_starts_with(target: &Path, base: &Path) -> bool {
    let target: Vec<_> = target.components().map(component_key).collect();
    let base: Vec<_> = base.components().map(component_key).collect();
    !base.is_empty() && target.len() >= base.len() && target[..base.len()] == base[..]
}

fn component_key(c: Component<'_>) -> std::ffi::OsString {
    let s = c.as_os_str();
    #[cfg(windows)]
    {
        std::ffi::OsString::from(s.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        s.to_os_string()
    }
}

fn default_rules() -> &'static [ProtectedPath] {
    static RULES: OnceLock<Vec<ProtectedPath>> = OnceLock::new();
    RULES.get_or_init(build_rules)
}

fn build_rules() -> Vec<ProtectedPath> {
    let mut rules = Vec::new();

    #[cfg(unix)]
    {
        for p in [
            "/", "/home", "/Users", "/root", "/mnt", "/media", "/Volumes",
        ] {
            add_rule(&mut rules, Path::new(p), ProtectMode::Exact);
        }
        for p in [
            "/usr",
            "/etc",
            "/bin",
            "/sbin",
            "/lib",
            "/lib32",
            "/lib64",
            "/libx32",
            "/libexec",
            "/boot",
            "/dev",
            "/proc",
            "/sys",
            "/System",
            "/Library",
            "/Applications",
        ] {
            add_rule(&mut rules, Path::new(p), ProtectMode::Subtree);
        }
        for p in [
            "/var", "/tmp", "/var/tmp", "/opt", "/srv", "/run", "/snap", "/nix",
            // macOS の /private は /tmp や /var の実体を包含する。Subtree に
            // すると /private/tmp や /private/var/folders まで拒否される。
            "/private",
        ] {
            add_rule(&mut rules, Path::new(p), ProtectMode::Exact);
        }
        if let Some(home) = std::env::var_os("HOME") {
            add_rule(&mut rules, Path::new(&home), ProtectMode::Exact);
        }
    }

    #[cfg(windows)]
    {
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            add_rule(&mut rules, Path::new(&root), ProtectMode::Exact);
        }
        for var in [
            "SystemRoot",
            "windir",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramData",
        ] {
            if let Some(value) = std::env::var_os(var) {
                add_rule(&mut rules, Path::new(&value), ProtectMode::Subtree);
            }
        }
        for (p, mode) in [
            (r"C:\Users", ProtectMode::Exact),
            (r"C:\Boot", ProtectMode::Subtree),
            (r"C:\EFI", ProtectMode::Subtree),
            (r"C:\Config.Msi", ProtectMode::Subtree),
            (r"C:\Recovery", ProtectMode::Subtree),
            (r"C:\$Recycle.Bin", ProtectMode::Subtree),
            (r"C:\System Volume Information", ProtectMode::Subtree),
            (r"C:\Windows.old", ProtectMode::Subtree),
        ] {
            add_rule(&mut rules, Path::new(p), mode);
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            add_rule(&mut rules, Path::new(&home), ProtectMode::Exact);
        }
    }

    rules
}

fn add_rule(rules: &mut Vec<ProtectedPath>, path: &Path, mode: ProtectMode) {
    let Ok(abs) = absolute_lexical(path) else {
        return;
    };
    // シンボリックリンクの保護パスは、リンク自身の位置 (字句パス) と
    // リンク先 (canonical) の両方を保護する。片方だけだと、リンク自身の
    // 削除やリンク先の削除を取りこぼす。
    let lexical = strip_verbatim(&abs);
    let canonical = std::fs::canonicalize(&abs)
        .map(|p| strip_verbatim(&p))
        .unwrap_or_else(|_| lexical.clone());
    for normalized in [lexical, canonical] {
        if rules
            .iter()
            .any(|r| r.mode == mode && paths_equal(&r.path, &normalized))
        {
            continue;
        }
        rules.push(ProtectedPath {
            path: normalized,
            mode,
        });
    }
}

fn home_roots() -> &'static [PathBuf] {
    static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut roots = Vec::new();
        #[cfg(unix)]
        for p in ["/home", "/Users", "/var/home"] {
            push_existing_canonical(&mut roots, Path::new(p));
        }
        #[cfg(windows)]
        push_existing_canonical(&mut roots, Path::new(r"C:\Users"));
        roots
    })
}

fn push_existing_canonical(out: &mut Vec<PathBuf>, path: &Path) {
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return;
    };
    let canonical = strip_verbatim(&canonical);
    if !out.iter().any(|existing| paths_equal(existing, &canonical)) {
        out.push(canonical);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rules_for(path: &Path, mode: ProtectMode) -> Vec<ProtectedPath> {
        vec![ProtectedPath {
            path: path.to_path_buf(),
            mode,
        }]
    }

    #[test]
    fn exact_protects_only_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("protected");
        fs::create_dir_all(root.join("child")).unwrap();
        let rules = rules_for(&root, ProtectMode::Exact);

        let err = check_protected_with(&root, false, &rules, &[]).unwrap_err();
        assert!(err.contains("保護対象"), "{err}");
        assert!(check_protected_with(&root.join("child"), false, &rules, &[]).is_ok());
    }

    #[test]
    fn subtree_protects_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("system");
        fs::create_dir_all(root.join("child")).unwrap();
        let file = root.join("child").join("a.bin");
        fs::write(&file, b"x").unwrap();
        let rules = rules_for(&root, ProtectMode::Subtree);

        assert!(check_protected_with(&root, false, &rules, &[]).is_err());
        assert!(check_protected_with(&file, false, &rules, &[]).is_err());
    }

    #[test]
    fn home_itself_protected_but_child_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join("project")).unwrap();
        let rules = rules_for(&home, ProtectMode::Exact);

        assert!(check_protected_with(&home, false, &rules, &[]).is_err());
        assert!(check_protected_with(&home.join("project"), false, &rules, &[]).is_ok());
    }

    #[test]
    fn home_direct_child_protected() {
        let dir = tempfile::tempdir().unwrap();
        let homes = dir.path().join("homes");
        let user = homes.join("alice");
        fs::create_dir_all(user.join("project")).unwrap();
        let rules = Vec::new();

        let err =
            check_protected_with(&user, false, &rules, std::slice::from_ref(&homes)).unwrap_err();
        assert!(err.contains("ホーム"), "{err}");
        assert!(check_protected_with(
            &user.join("project"),
            false,
            &rules,
            std::slice::from_ref(&homes)
        )
        .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn home_direct_child_symlink_is_protected() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let homes = dir.path().join("homes");
        fs::create_dir_all(&homes).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let link = homes.join("bob");
        symlink(&outside, &link).unwrap();
        let rules = Vec::new();

        let err =
            check_protected_with(&link, true, &rules, std::slice::from_ref(&homes)).unwrap_err();
        assert!(err.contains("ホーム"), "{err}");
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn link_leaf_checks_its_own_location_not_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("protected");
        fs::create_dir_all(&protected).unwrap();
        let link = dir.path().join("link");
        symlink(&protected, &link).unwrap();
        let rules = rules_for(&protected, ProtectMode::Subtree);

        // リンク先は保護対象だが、リンク自身の削除は許可する。
        assert!(check_protected_with(&link, true, &rules, &[]).is_ok());
        // リンク先そのものは保護する。
        assert!(check_protected_with(&protected, false, &rules, &[]).is_err());
    }

    #[test]
    fn missing_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let rules = Vec::new();

        assert!(check_protected_with(&missing, false, &rules, &[]).is_err());
    }

    #[test]
    fn private_exact_allows_tmp_child() {
        // macOS の /private は /tmp の親だが、Exact なら /private/tmp 配下を
        // 巻き込まない。Subtree にすると temp ディレクトリまで拒否される。
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        let tmp = private.join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("keep.txt");
        fs::write(&file, b"x").unwrap();
        let rules = vec![
            ProtectedPath {
                path: private.clone(),
                mode: ProtectMode::Exact,
            },
            ProtectedPath {
                path: tmp.clone(),
                mode: ProtectMode::Exact,
            },
        ];

        assert!(check_protected_with(&private, false, &rules, &[]).is_err());
        assert!(check_protected_with(&tmp, false, &rules, &[]).is_err());
        assert!(check_protected_with(&file, false, &rules, &[]).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn root_directory_is_protected() {
        let rules = rules_for(Path::new("/"), ProtectMode::Exact);
        assert!(check_protected_with(Path::new("/"), false, &rules, &[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_rule_protects_both_link_and_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir_all(real.join("child")).unwrap();
        let link = dir.path().join("link");
        symlink(&real, &link).unwrap();

        let mut rules = Vec::new();
        add_rule(&mut rules, &link, ProtectMode::Exact);

        assert!(check_protected_with(&link, true, &rules, &[]).is_err());
        assert!(check_protected_with(&real, false, &rules, &[]).is_err());
        assert!(check_protected_with(&real.join("child"), false, &rules, &[]).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn drive_root_is_protected() {
        let rules = rules_for(Path::new(r"C:\"), ProtectMode::Exact);
        assert!(check_protected_with(Path::new(r"C:\"), false, &rules, &[]).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_temp_directory_is_not_protected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            check_protected_path(dir.path(), false).is_ok(),
            "{}",
            dir.path().display()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_tmp_directory_is_not_protected() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        assert!(
            check_protected_path(dir.path(), false).is_ok(),
            "{}",
            dir.path().display()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_private_root_is_protected() {
        assert!(check_protected_path(Path::new("/private"), false).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_etc_is_protected() {
        assert!(check_protected_path(Path::new("/etc/passwd"), false).is_err());
    }
}

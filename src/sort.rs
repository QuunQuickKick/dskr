use std::cmp::Ordering;
use std::str::FromStr;

use crate::scan::node::Tree;

/// 並び替えのキー。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortField {
    Size,
    Name,
    Count,
    Mtime,
}

impl SortField {
    pub fn next(self) -> Self {
        match self {
            SortField::Size => SortField::Name,
            SortField::Name => SortField::Count,
            SortField::Count => SortField::Mtime,
            SortField::Mtime => SortField::Size,
        }
    }

    /// TUI 用の短いラベル。
    pub fn label(self) -> &'static str {
        match self {
            SortField::Size => "サイズ",
            SortField::Name => "名前",
            SortField::Count => "ファイル数",
            SortField::Mtime => "更新日時",
        }
    }

    /// CLI/JSON 用の識別子。
    pub fn as_str(self) -> &'static str {
        match self {
            SortField::Size => "size",
            SortField::Name => "name",
            SortField::Count => "count",
            SortField::Mtime => "mtime",
        }
    }
}

impl FromStr for SortField {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "size" => Ok(SortField::Size),
            "name" => Ok(SortField::Name),
            "count" | "files" => Ok(SortField::Count),
            "mtime" | "time" => Ok(SortField::Mtime),
            _ => Err(format!("未知のソートキーです: {s} (size|name|count|mtime)")),
        }
    }
}

/// 並び替えの方向。Desc = 大きい順/新しい順/Z→A。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn toggled(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            SortDir::Asc => "↑",
            SortDir::Desc => "↓",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SortDir::Asc => "asc",
            SortDir::Desc => "desc",
        }
    }
}

impl FromStr for SortDir {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "asc" | "ascending" => Ok(SortDir::Asc),
            "desc" | "descending" => Ok(SortDir::Desc),
            _ => Err(format!("未知のソート方向です: {s} (asc|desc)")),
        }
    }
}

/// `size:desc` / `name:asc` / `size` のような指定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortSpec {
    pub field: SortField,
    pub dir: SortDir,
}

impl FromStr for SortSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.splitn(2, ':');
        let field = parts.next().unwrap_or("size").parse::<SortField>()?;
        let dir = match parts.next() {
            Some(d) => d.parse::<SortDir>()?,
            None => SortDir::Desc,
        };
        Ok(SortSpec { field, dir })
    }
}

fn name_lower(tree: &Tree, i: u32) -> String {
    tree.node(i).name.to_string_lossy().to_lowercase()
}

fn name_raw(tree: &Tree, i: u32) -> String {
    tree.node(i).name.to_string_lossy().into_owned()
}

/// ノード a と b を指定条件で比較する。
/// サイズ・件数・更新日時が同値の場合は名前昇順で安定させる
/// (方向を反転してもタイブレークは常に昇順)。
pub fn compare_nodes(
    tree: &Tree,
    a: u32,
    b: u32,
    field: SortField,
    dir: SortDir,
    show_allocated: bool,
) -> Ordering {
    let primary = match field {
        SortField::Size => {
            let na = tree.node(a);
            let nb = tree.node(b);
            if show_allocated {
                na.total_alloc.cmp(&nb.total_alloc)
            } else {
                na.total_logical.cmp(&nb.total_logical)
            }
        }
        SortField::Name => name_lower(tree, a)
            .cmp(&name_lower(tree, b))
            .then_with(|| name_raw(tree, a).cmp(&name_raw(tree, b))),
        SortField::Count => tree.node(a).file_count.cmp(&tree.node(b).file_count),
        SortField::Mtime => tree.node(a).mtime.cmp(&tree.node(b).mtime),
    };

    let primary = if dir == SortDir::Desc {
        primary.reverse()
    } else {
        primary
    };

    if field == SortField::Name {
        primary
    } else {
        primary.then_with(|| name_raw(tree, a).cmp(&name_raw(tree, b)))
    }
}

/// ディレクトリ直下の子を並び替えて返す。
pub fn sorted_children(
    tree: &Tree,
    parent: u32,
    field: SortField,
    dir: SortDir,
    show_allocated: bool,
) -> Vec<u32> {
    let mut kids = tree.node(parent).children.clone();
    kids.sort_by(|&a, &b| compare_nodes(tree, a, b, field, dir, show_allocated));
    kids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::node::{Node, Tree};

    fn fixture() -> (Tree, u32, u32, u32, u32) {
        let mut t = Tree::with_root("root".into(), 0);
        let a = t
            .push_child_capped(0, Node::file("b.txt".into(), 0, 1, 100, 100, 100), 100)
            .unwrap();
        let b = t
            .push_child_capped(0, Node::file("a.txt".into(), 0, 1, 300, 300, 300), 100)
            .unwrap();
        let c = t
            .push_child_capped(0, Node::dir("c".into(), 0, 1, 200), 100)
            .unwrap();
        t.push_child_capped(c, Node::file("z".into(), c, 2, 50, 50, 50), 100)
            .unwrap();
        t.aggregate();
        (t, a, b, c, 0)
    }

    fn names(t: &Tree, ids: &[u32]) -> Vec<String> {
        ids.iter()
            .map(|&i| t.node(i).name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn size_desc_is_largest_first() {
        let (t, _, _, _, root) = fixture();
        let ids = sorted_children(&t, root, SortField::Size, SortDir::Desc, true);
        assert_eq!(names(&t, &ids), vec!["a.txt", "b.txt", "c"]);
        assert!(t.node(ids[0]).total_alloc >= t.node(ids[1]).total_alloc);
    }

    #[test]
    fn size_asc_is_smallest_first() {
        let (t, _, _, _, root) = fixture();
        let ids = sorted_children(&t, root, SortField::Size, SortDir::Asc, true);
        assert_eq!(names(&t, &ids), vec!["c", "b.txt", "a.txt"]);
    }

    #[test]
    fn name_direction_applies() {
        let (t, _, _, _, root) = fixture();
        let asc = sorted_children(&t, root, SortField::Name, SortDir::Asc, true);
        assert_eq!(names(&t, &asc), vec!["a.txt", "b.txt", "c"]);
        let desc = sorted_children(&t, root, SortField::Name, SortDir::Desc, true);
        assert_eq!(names(&t, &desc), vec!["c", "b.txt", "a.txt"]);
    }

    #[test]
    fn count_and_mtime_directions() {
        let (t, _, _, _, root) = fixture();
        let count_desc = sorted_children(&t, root, SortField::Count, SortDir::Desc, true);
        // a.txt/b.txt=1, c=1 (own file), tie -> name asc
        assert_eq!(names(&t, &count_desc), vec!["a.txt", "b.txt", "c"]);

        let mtime_desc = sorted_children(&t, root, SortField::Mtime, SortDir::Desc, true);
        assert_eq!(names(&t, &mtime_desc), vec!["a.txt", "c", "b.txt"]);
    }

    #[test]
    fn sort_spec_parse() {
        let s: SortSpec = "size:desc".parse().unwrap();
        assert_eq!(s.field, SortField::Size);
        assert_eq!(s.dir, SortDir::Desc);

        let s: SortSpec = "name".parse().unwrap();
        assert_eq!(s.field, SortField::Name);
        assert_eq!(s.dir, SortDir::Desc);

        assert!("unknown".parse::<SortSpec>().is_err());
        assert!("size:sideways".parse::<SortSpec>().is_err());
    }

    #[test]
    fn sort_cycle_matches_column_order() {
        // 表示列 (サイズ → 名前 → ファイル数 → 更新日時) と同じ順で切り替わる。
        let order = [
            SortField::Size,
            SortField::Name,
            SortField::Count,
            SortField::Mtime,
        ];
        let mut field = SortField::Size;
        for expected in order {
            assert_eq!(field, expected);
            field = field.next();
        }
        assert_eq!(field, SortField::Size);
    }
}

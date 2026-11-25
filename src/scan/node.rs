use std::ffi::{OsStr, OsString};

/// ノード種別。Other はシンボリックリンク・ジャンクション・特殊ファイルなど。
/// Blocked は -x / --local-only などにより走査しないと決めた実ディレクトリ
/// (中身は集計されない)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    Other,
    Blocked,
}

/// アリーナ方式のツリーノード。インデックス (u32) で参照する。
pub struct Node {
    pub name: Box<OsStr>,
    pub parent: u32,
    pub children: Vec<u32>,
    pub kind: NodeKind,
    /// ファイル/その他のノード自体のサイズ。ディレクトリは 0。
    pub own_alloc: u64,
    pub own_logical: u64,
    /// 集計後のサブツリー合計。
    pub total_alloc: u64,
    pub total_logical: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub err_count: u32,
    pub mtime: i64,
    pub depth: u16,
    /// max_depth / max_nodes により子が省略されたことを示す。
    pub truncated: bool,
}

impl Node {
    pub fn dir(name: OsString, parent: u32, depth: u16, mtime: i64) -> Node {
        Node {
            name: name.into_boxed_os_str(),
            parent,
            children: Vec::new(),
            kind: NodeKind::Dir,
            own_alloc: 0,
            own_logical: 0,
            total_alloc: 0,
            total_logical: 0,
            file_count: 0,
            dir_count: 0,
            err_count: 0,
            mtime,
            depth,
            truncated: false,
        }
    }

    pub fn file(
        name: OsString,
        parent: u32,
        depth: u16,
        alloc: u64,
        logical: u64,
        mtime: i64,
    ) -> Node {
        Node {
            name: name.into_boxed_os_str(),
            parent,
            children: Vec::new(),
            kind: NodeKind::File,
            own_alloc: alloc,
            own_logical: logical,
            total_alloc: alloc,
            total_logical: logical,
            file_count: 1,
            dir_count: 0,
            err_count: 0,
            mtime,
            depth,
            truncated: false,
        }
    }

    pub fn other(
        name: OsString,
        parent: u32,
        depth: u16,
        alloc: u64,
        logical: u64,
        mtime: i64,
    ) -> Node {
        Node {
            name: name.into_boxed_os_str(),
            parent,
            children: Vec::new(),
            kind: NodeKind::Other,
            own_alloc: alloc,
            own_logical: logical,
            total_alloc: alloc,
            total_logical: logical,
            file_count: 0,
            dir_count: 0,
            err_count: 0,
            mtime,
            depth,
            truncated: false,
        }
    }

    /// 走査しない実ディレクトリ (-x 境界・ネットワークFSなど)。
    /// 中身を調べていないためサイズは 0。
    pub fn blocked(name: OsString, parent: u32, depth: u16, mtime: i64) -> Node {
        Node {
            name: name.into_boxed_os_str(),
            parent,
            children: Vec::new(),
            kind: NodeKind::Blocked,
            own_alloc: 0,
            own_logical: 0,
            total_alloc: 0,
            total_logical: 0,
            file_count: 0,
            dir_count: 0,
            err_count: 0,
            mtime,
            depth,
            truncated: false,
        }
    }
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: u32,
}

impl Tree {
    pub fn with_root(name: OsString, mtime: i64) -> Tree {
        let root_node = Node::dir(name, u32::MAX, 0, mtime);
        Tree {
            nodes: vec![root_node],
            root: 0,
        }
    }

    pub fn node(&self, i: u32) -> &Node {
        &self.nodes[i as usize]
    }

    /// 子ノードを追加してそのインデックスを返す。容量上限超過時は親に
    /// truncated を立てて None を返す。
    /// ノード番号は u32 のため、u32::MAX を超える件数は保持できない。
    pub fn push_child_capped(&mut self, parent: u32, node: Node, max_nodes: u64) -> Option<u32> {
        if self.nodes.len() as u64 >= max_nodes || self.nodes.len() >= u32::MAX as usize {
            self.nodes[parent as usize].truncated = true;
            return None;
        }
        let idx = self.nodes.len() as u32;
        self.nodes.push(node);
        self.nodes[parent as usize].children.push(idx);
        Some(idx)
    }

    /// ノードを親から外し、祖先の集計値から差し引く。
    ///
    /// アリーナ内のノード自体は残るが、親から参照されなくなる。
    /// ルートや範囲外のインデックスは変更せず false を返す。
    pub fn detach(&mut self, idx: u32) -> bool {
        if idx == self.root || idx as usize >= self.nodes.len() {
            return false;
        }
        let (parent, kind, ta, tl, fc, dc) = {
            let n = &self.nodes[idx as usize];
            if n.parent == u32::MAX {
                return false;
            }
            (
                n.parent,
                n.kind,
                n.total_alloc,
                n.total_logical,
                n.file_count,
                n.dir_count,
            )
        };
        if !self.nodes[parent as usize].children.contains(&idx) {
            return false;
        }
        self.nodes[parent as usize].children.retain(|&c| c != idx);
        let dir_delta = dc + u64::from(kind == NodeKind::Dir);
        let mut cur = parent;
        loop {
            let n = &mut self.nodes[cur as usize];
            n.total_alloc = n.total_alloc.saturating_sub(ta);
            n.total_logical = n.total_logical.saturating_sub(tl);
            n.file_count = n.file_count.saturating_sub(fc);
            n.dir_count = n.dir_count.saturating_sub(dir_delta);
            if cur == self.root {
                break;
            }
            if n.parent == u32::MAX {
                break;
            }
            cur = n.parent;
        }
        true
    }

    /// 逆順に走査して子の合計を親へ加算する。
    /// 子は必ず親より後のインデックスで作られるため、後ろから 1 回で足りる。
    pub fn aggregate(&mut self) {
        for i in (0..self.nodes.len()).rev() {
            let children = std::mem::take(&mut self.nodes[i].children);
            let (mut ta, mut tl, mut fc, mut dc) = (0u64, 0u64, 0u64, 0u64);
            for &c in &children {
                let ch = &self.nodes[c as usize];
                ta = ta.saturating_add(ch.total_alloc);
                tl = tl.saturating_add(ch.total_logical);
                fc = fc.saturating_add(ch.file_count);
                dc = dc.saturating_add(ch.dir_count);
                if ch.kind == NodeKind::Dir {
                    dc = dc.saturating_add(1);
                }
            }
            let n = &mut self.nodes[i];
            n.total_alloc = n.total_alloc.saturating_add(ta);
            n.total_logical = n.total_logical.saturating_add(tl);
            n.file_count = n.file_count.saturating_add(fc);
            n.dir_count = n.dir_count.saturating_add(dc);
            n.children = children;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_sums_subtree() {
        let mut t = Tree::with_root("root".into(), 0);
        let d = t
            .push_child_capped(0, Node::dir("sub".into(), 0, 1, 0), 100)
            .unwrap();
        t.push_child_capped(d, Node::file("a".into(), d, 2, 100, 100, 0), 100)
            .unwrap();
        t.push_child_capped(d, Node::file("b".into(), d, 2, 200, 200, 0), 100)
            .unwrap();
        t.push_child_capped(0, Node::file("c".into(), 0, 1, 50, 50, 0), 100)
            .unwrap();
        t.aggregate();
        let root = t.node(0);
        assert_eq!(root.total_alloc, 350);
        assert_eq!(root.total_logical, 350);
        assert_eq!(root.file_count, 3);
        assert_eq!(root.dir_count, 1);
        let sub = t.node(d);
        assert_eq!(sub.total_alloc, 300);
        assert_eq!(sub.file_count, 2);
    }

    #[test]
    fn detach_updates_ancestor_totals_and_children() {
        let mut t = Tree::with_root("root".into(), 0);
        let d = t
            .push_child_capped(0, Node::dir("sub".into(), 0, 1, 0), 100)
            .unwrap();
        let a = t
            .push_child_capped(d, Node::file("a".into(), d, 2, 100, 120, 0), 100)
            .unwrap();
        t.push_child_capped(d, Node::file("b".into(), d, 2, 200, 220, 0), 100)
            .unwrap();
        t.push_child_capped(0, Node::file("c".into(), 0, 1, 50, 50, 0), 100)
            .unwrap();
        t.aggregate();

        assert!(t.detach(a));
        assert!(!t.node(d).children.contains(&a));
        assert_eq!(t.node(d).total_alloc, 200);
        assert_eq!(t.node(d).total_logical, 220);
        assert_eq!(t.node(d).file_count, 1);
        assert_eq!(t.node(0).total_alloc, 250);
        assert_eq!(t.node(0).total_logical, 270);
        assert_eq!(t.node(0).file_count, 2);
        assert_eq!(t.node(0).dir_count, 1);

        // ディレクトリを外すと、自身の分も祖先から減る。
        assert!(t.detach(d));
        assert_eq!(t.node(0).total_alloc, 50);
        assert_eq!(t.node(0).total_logical, 50);
        assert_eq!(t.node(0).file_count, 1);
        assert_eq!(t.node(0).dir_count, 0);
        assert!(!t.node(0).children.contains(&d));
    }

    #[test]
    fn detach_refuses_root_and_out_of_range() {
        let mut t = Tree::with_root("root".into(), 0);
        assert!(!t.detach(0));
        assert!(!t.detach(99));
        assert_eq!(t.node(0).children.len(), 0);
    }

    #[test]
    fn cap_marks_truncated() {
        let mut t = Tree::with_root("root".into(), 0);
        let n = t.push_child_capped(0, Node::file("a".into(), 0, 1, 1, 1, 0), 2);
        assert!(n.is_some());
        let n2 = t.push_child_capped(0, Node::file("b".into(), 0, 1, 1, 1, 0), 2);
        assert!(n2.is_none());
        assert!(t.node(0).truncated);
    }

    #[test]
    fn blocked_is_not_counted_as_dir() {
        // 走査していないディレクトリなので、集計にはディレクトリ数・サイズと
        // して現れない。
        let mut t = Tree::with_root("root".into(), 0);
        let b = t
            .push_child_capped(0, Node::blocked("other-fs".into(), 0, 1, 0), 100)
            .unwrap();
        t.push_child_capped(0, Node::file("a".into(), 0, 1, 10, 10, 0), 100)
            .unwrap();
        t.aggregate();
        assert_eq!(t.node(b).kind, NodeKind::Blocked);
        assert_eq!(t.node(b).total_alloc, 0);
        assert_eq!(t.node(b).file_count, 0);
        assert_eq!(t.node(0).dir_count, 0);
        assert_eq!(t.node(0).file_count, 1);
        // detach しても祖先の集計は変化しない (中身を持たないため)。
        assert!(t.detach(b));
        assert_eq!(t.node(0).total_alloc, 10);
    }
}

use std::collections::HashMap;

/// A node in the project folder tree.
#[derive(Debug)]
pub struct FolderNode {
    /// Display name (last path component).
    pub name: String,
    /// Full absolute path for this node.
    pub full_path: String,
    /// Rolled-up count: self + all descendants.
    pub total_count: usize,
    /// Whether this node is expanded in the UI.
    pub expanded: bool,
    /// Whether this node has children.
    pub has_children: bool,
    /// Child nodes, sorted by total_count descending.
    pub children: Vec<FolderNode>,
}

/// A visible row when the tree is flattened for rendering.
#[derive(Debug)]
pub struct FlatRow {
    /// Indentation depth (0 = top-level).
    pub depth: usize,
    /// Index path through the tree to reach this node (e.g. [0, 2] = root.children[0].children[2]).
    pub tree_path: Vec<usize>,
    /// Display name.
    pub name: String,
    /// Full path for filtering.
    pub full_path: String,
    /// Rolled-up count.
    pub total_count: usize,
    /// Whether this node has children (show ▸/▾).
    pub has_children: bool,
    /// Whether this node is expanded.
    pub expanded: bool,
}

/// The folder tree built from search result project paths.
#[derive(Debug)]
pub struct FolderTree {
    /// Top-level roots (usually one, but could be multiple unrelated paths).
    pub roots: Vec<FolderNode>,
    /// Total number of results across all folders.
    pub total_count: usize,
}

impl FolderTree {
    /// Build a tree from a list of project paths.
    /// Each path string can appear multiple times (once per search result).
    pub fn build(project_paths: &[Option<String>]) -> Self {
        // Count occurrences of each path.
        let mut counts: HashMap<String, usize> = HashMap::new();
        for path in project_paths.iter().flatten() {
            *counts.entry(path.clone()).or_default() += 1;
        }

        let total_count: usize = counts.values().sum();

        if counts.is_empty() {
            return FolderTree {
                roots: Vec::new(),
                total_count,
            };
        }

        // Find the longest common prefix to collapse the root.
        let paths: Vec<&str> = counts.keys().map(|s| s.as_str()).collect();
        let common_prefix = longest_common_path_prefix(&paths);

        // Build a trie from the path segments below the common prefix.
        let mut trie = TrieNode::default();
        for (path, &count) in &counts {
            let suffix = if common_prefix.is_empty() {
                path.as_str()
            } else if path.len() > common_prefix.len() {
                &path[common_prefix.len() + 1..] // skip the '/'
            } else {
                "" // path IS the common prefix
            };

            let segments: Vec<&str> = if suffix.is_empty() {
                Vec::new()
            } else {
                suffix.split('/').collect()
            };

            trie.insert(&segments, path, count);
        }

        // Convert trie to FolderNode tree.
        let roots = if trie.children.is_empty() {
            // All sessions are in the same single directory.
            let name = common_prefix
                .rsplit('/')
                .next()
                .unwrap_or(&common_prefix)
                .to_string();
            vec![FolderNode {
                name,
                full_path: common_prefix,
                total_count: trie.direct_count,
                expanded: false,
                has_children: false,
                children: Vec::new(),
            }]
        } else if common_prefix.is_empty() {
            // No common prefix — multiple unrelated root paths.
            trie_to_folder_nodes(&trie, "")
        } else {
            // Common prefix with sub-paths. Wrap in a root node.
            let children = trie_to_folder_nodes(&trie, &common_prefix);
            let child_total: usize = children.iter().map(|c| c.total_count).sum();
            let name = common_prefix
                .rsplit('/')
                .next()
                .unwrap_or(&common_prefix)
                .to_string();
            vec![FolderNode {
                name,
                full_path: common_prefix,
                total_count: trie.direct_count + child_total,
                expanded: false,
                has_children: !children.is_empty(),
                children,
            }]
        };

        let mut tree = FolderTree { roots, total_count };
        tree.auto_expand();
        tree
    }

    /// Auto-expand single-child chains so the user doesn't have to click
    /// through a linear path to reach the first branching point.
    fn auto_expand(&mut self) {
        let single_root = self.roots.len() == 1;
        for root in &mut self.roots {
            if single_root && root.has_children {
                root.expanded = true;
            }
            auto_expand_node(root);
        }
    }

    /// Flatten the visible portion of the tree into rows for rendering.
    pub fn visible_rows(&self) -> Vec<FlatRow> {
        let mut rows = Vec::new();
        for (i, root) in self.roots.iter().enumerate() {
            flatten_node(root, 0, &mut vec![i], &mut rows);
        }
        rows
    }

    /// Expand the node at the given tree_path.
    pub fn expand(&mut self, tree_path: &[usize]) {
        if let Some(node) = self.node_at_mut(tree_path) {
            if node.has_children {
                node.expanded = true;
            }
        }
    }

    /// Collapse the node at the given tree_path.
    pub fn collapse(&mut self, tree_path: &[usize]) {
        if let Some(node) = self.node_at_mut(tree_path) {
            node.expanded = false;
        }
    }

    fn node_at_mut(&mut self, tree_path: &[usize]) -> Option<&mut FolderNode> {
        if tree_path.is_empty() {
            return None;
        }
        let mut current = self.roots.get_mut(tree_path[0])?;
        for &idx in &tree_path[1..] {
            current = current.children.get_mut(idx)?;
        }
        Some(current)
    }
}

// -- Trie for intermediate construction --

#[derive(Default)]
struct TrieNode {
    children: HashMap<String, TrieNode>,
    direct_count: usize,
    full_path: Option<String>,
}

impl TrieNode {
    fn insert(&mut self, segments: &[&str], full_path: &str, count: usize) {
        if segments.is_empty() {
            self.direct_count += count;
            self.full_path = Some(full_path.to_string());
            return;
        }
        let child = self
            .children
            .entry(segments[0].to_string())
            .or_default();
        child.insert(&segments[1..], full_path, count);
    }

}

fn trie_to_folder_nodes(trie: &TrieNode, base_path: &str) -> Vec<FolderNode> {
    let mut nodes: Vec<FolderNode> = trie
        .children
        .iter()
        .map(|(name, child)| {
            let full_path = if base_path.is_empty() {
                name.clone()
            } else {
                format!("{base_path}/{name}")
            };

            // Collapse single-child chains: if this node has no direct sessions
            // and exactly one child, merge them into "name/child_name".
            let (display_name, effective_child, effective_path) =
                collapse_single_child(name, child, &full_path);

            let children = trie_to_folder_nodes(effective_child, &effective_path);
            let child_total: usize = children.iter().map(|c| c.total_count).sum();

            FolderNode {
                name: display_name,
                full_path: effective_path,
                total_count: effective_child.direct_count + child_total,
                expanded: false,
                has_children: !children.is_empty(),
                children,
            }
        })
        .collect();

    // Sort by total_count descending, then by name.
    nodes.sort_by(|a, b| b.total_count.cmp(&a.total_count).then(a.name.cmp(&b.name)));
    nodes
}

/// Collapse chains like A -> B -> C (where A and B have no direct sessions and one child)
/// into a single node displayed as "A/B/C".
fn collapse_single_child<'a>(
    name: &str,
    node: &'a TrieNode,
    full_path: &str,
) -> (String, &'a TrieNode, String) {
    let mut display_name = name.to_string();
    let mut current = node;
    let mut current_path = full_path.to_string();

    while current.direct_count == 0 && current.children.len() == 1 {
        let (child_name, child_node) = current.children.iter().next().unwrap();
        display_name = format!("{display_name}/{child_name}");
        current_path = format!("{current_path}/{child_name}");
        current = child_node;
    }

    (display_name, current, current_path)
}

fn flatten_node(
    node: &FolderNode,
    depth: usize,
    path: &mut Vec<usize>,
    rows: &mut Vec<FlatRow>,
) {
    rows.push(FlatRow {
        depth,
        tree_path: path.clone(),
        name: node.name.clone(),
        full_path: node.full_path.clone(),
        total_count: node.total_count,
        has_children: node.has_children,
        expanded: node.expanded,
    });

    if node.expanded {
        for (i, child) in node.children.iter().enumerate() {
            path.push(i);
            flatten_node(child, depth + 1, path, rows);
            path.pop();
        }
    }
}

/// Recursively expand nodes that have exactly one child — these form a
/// linear chain where there's nothing to choose, so show them pre-expanded.
fn auto_expand_node(node: &mut FolderNode) {
    if node.children.len() == 1 {
        node.children[0].expanded = node.children[0].has_children;
        auto_expand_node(&mut node.children[0]);
    }
}

/// Find the longest common directory prefix among a set of paths.
fn longest_common_path_prefix(paths: &[&str]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    if paths.len() == 1 {
        return paths[0].to_string();
    }

    // Use the shortest path as the reference to avoid out-of-bounds issues.
    let shortest = paths.iter().min_by_key(|p| p.len()).unwrap();

    let mut prefix_len = 0;
    for (i, ch) in shortest.char_indices() {
        if paths.iter().any(|p| p.as_bytes().get(i).copied() != Some(ch as u8)) {
            break;
        }
        prefix_len = i + ch.len_utf8();
    }

    let prefix = &shortest[..prefix_len];

    // If the entire shortest path is a common prefix, check that it's a valid
    // path boundary (all longer paths have '/' right after it).
    if prefix_len == shortest.len() {
        let valid = paths.iter().all(|p| {
            p.len() == prefix_len || p.as_bytes().get(prefix_len) == Some(&b'/')
        });
        if valid {
            return prefix.to_string();
        }
    }

    // Otherwise cut at the last '/' boundary.
    match prefix.rfind('/') {
        Some(pos) if pos > 0 => prefix[..pos].to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_folder() {
        let paths = vec![
            Some("/home/user/project".to_string()),
            Some("/home/user/project".to_string()),
            Some("/home/user/project".to_string()),
        ];
        let tree = FolderTree::build(&paths);
        assert_eq!(tree.total_count, 3);
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].total_count, 3);
        assert!(!tree.roots[0].has_children);
    }

    #[test]
    fn test_nested_folders() {
        let paths = vec![
            Some("/X".to_string()),
            Some("/X".to_string()),
            Some("/X".to_string()),
            Some("/X/Y".to_string()),
            Some("/X/Y".to_string()),
            Some("/X/Y".to_string()),
            Some("/X/Y".to_string()),
            Some("/X/Z".to_string()),
            Some("/X/Z".to_string()),
            Some("/X/Z/A".to_string()),
            Some("/X/Z/A".to_string()),
            Some("/X/Z/A".to_string()),
            Some("/X/Z/A".to_string()),
            Some("/X/Z/A".to_string()),
        ];
        let tree = FolderTree::build(&paths);
        assert_eq!(tree.total_count, 14);
        // Root should be X with total 14
        assert_eq!(tree.roots.len(), 1);
        let x = &tree.roots[0];
        assert_eq!(x.name, "X");
        assert_eq!(x.total_count, 14);
        assert!(x.has_children);
        // Children: Z (7) and Y (4), sorted by count desc
        assert_eq!(x.children.len(), 2);
        assert_eq!(x.children[0].name, "Z");
        assert_eq!(x.children[0].total_count, 7);
        assert!(x.children[0].has_children); // has child A
        assert_eq!(x.children[1].name, "Y");
        assert_eq!(x.children[1].total_count, 4);
        assert!(!x.children[1].has_children);
    }

    #[test]
    fn test_expand_collapse() {
        let paths = vec![
            Some("/X".to_string()),
            Some("/X/Y".to_string()),
            Some("/X/Z".to_string()),
        ];
        let mut tree = FolderTree::build(&paths);

        // Single root X with 2 children: auto-expanded since it's the only root.
        let rows = tree.visible_rows();
        assert_eq!(rows.len(), 3); // X, Z, Y (sorted by count)
        assert_eq!(rows[0].name, "X");

        // Collapse root.
        tree.collapse(&[0]);
        let rows = tree.visible_rows();
        assert_eq!(rows.len(), 1);

        // Re-expand root.
        tree.expand(&[0]);
        let rows = tree.visible_rows();
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn test_none_paths_ignored() {
        let paths = vec![
            None,
            Some("/X".to_string()),
            None,
            Some("/X/Y".to_string()),
        ];
        let tree = FolderTree::build(&paths);
        assert_eq!(tree.total_count, 2);
    }

    #[test]
    fn test_auto_expand_single_child_chain() {
        // /A has one child /A/B which has one child /A/B/C which has 2 children.
        // The chain A -> B -> C should all be auto-expanded.
        let paths = vec![
            Some("/A/B/C/D".to_string()),
            Some("/A/B/C/D".to_string()),
            Some("/A/B/C/E".to_string()),
        ];
        let tree = FolderTree::build(&paths);
        // Common prefix is /A/B/C, so root is "C" with children D and E.
        // Single root → auto-expanded. 2 children → recursion stops.
        let rows = tree.visible_rows();
        // Should see: C, D, E (all visible without manual expanding)
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "C");
    }

    #[test]
    fn test_auto_expand_deep_single_chain() {
        // All results in one folder: /X/Y/Z
        // Plus some in /X/Y/Z/A — single root, single child chain.
        let paths = vec![
            Some("/X/Y/Z".to_string()),
            Some("/X/Y/Z/A".to_string()),
        ];
        let tree = FolderTree::build(&paths);
        // Common prefix = /X/Y/Z. Root = "Z" with one child "A".
        // Single root → expanded. Single child → A also expanded (but A is a leaf).
        let rows = tree.visible_rows();
        assert_eq!(rows.len(), 2); // Z and A both visible
    }

    #[test]
    fn test_single_child_collapse() {
        // /A/B/C/D and /A/B/C/E should collapse /A/B/C into one display node
        let paths = vec![
            Some("/A/B/C/D".to_string()),
            Some("/A/B/C/E".to_string()),
        ];
        let tree = FolderTree::build(&paths);
        // Root should be collapsed to show the common ancestor
        assert_eq!(tree.roots.len(), 1);
        // The root represents /A/B/C and should have children D and E
        let root = &tree.roots[0];
        assert!(root.has_children);
        assert_eq!(root.children.len(), 2);
    }
}

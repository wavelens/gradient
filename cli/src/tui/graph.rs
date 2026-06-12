/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::tui::View;
use connector::builds::BuildGraph;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::collections::HashMap;

pub type NodeId = String;

struct Node {
    label: String,
    children: Vec<NodeId>,
    expanded: bool,
}

pub struct GraphTree {
    roots: Vec<NodeId>,
    nodes: HashMap<NodeId, Node>,
    flat: Vec<(NodeId, usize)>,
    selected: usize,
}

impl GraphTree {
    #[cfg(test)]
    pub fn from_edges(nodes: &[(&str, &str)], edges: &[(&str, &str)], roots: Vec<NodeId>) -> Self {
        let mut map: HashMap<NodeId, Node> = HashMap::new();
        for (id, label) in nodes {
            map.insert(
                (*id).to_string(),
                Node { label: (*label).to_string(), children: Vec::new(), expanded: false },
            );
        }
        for (parent, child) in edges {
            if let Some(n) = map.get_mut(*parent) {
                n.children.push((*child).to_string());
            }
        }
        let mut t = Self { roots, nodes: map, flat: Vec::new(), selected: 0 };
        t.rebuild_flat();
        t
    }

    pub fn from_build_graph(g: &BuildGraph) -> Self {
        let nodes: Vec<(String, String)> = g
            .nodes
            .iter()
            .filter_map(|n| {
                let id = n.get("id")?.as_str()?.to_string();
                let label = n
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| n.get("path").and_then(|v| v.as_str()))
                    .unwrap_or("?")
                    .to_string();
                Some((id, label))
            })
            .collect();
        let edges: Vec<(String, String)> = g
            .edges
            .iter()
            .filter_map(|e| {
                Some((e.get("source")?.as_str()?.to_string(), e.get("target")?.as_str()?.to_string()))
            })
            .collect();

        let mut map: HashMap<NodeId, Node> = HashMap::new();
        for (id, label) in &nodes {
            map.insert(id.clone(), Node { label: label.clone(), children: Vec::new(), expanded: false });
        }
        for (source, target) in &edges {
            if let Some(n) = map.get_mut(source) {
                n.children.push(target.clone());
            }
        }
        let roots = vec![g.root.clone()];
        let mut t = Self { roots, nodes: map, flat: Vec::new(), selected: 0 };
        t.rebuild_flat();
        t
    }

    fn rebuild_flat(&mut self) {
        let mut flat = Vec::new();
        let roots = self.roots.clone();
        let mut on_path: Vec<NodeId> = Vec::new();
        for r in &roots {
            self.dfs(r, 0, &mut flat, &mut on_path);
        }
        self.flat = flat;
        if self.selected >= self.flat.len() {
            self.selected = self.flat.len().saturating_sub(1);
        }
    }

    fn dfs(&self, id: &str, depth: usize, flat: &mut Vec<(NodeId, usize)>, on_path: &mut Vec<NodeId>) {
        if on_path.iter().any(|p| p == id) {
            return;
        }
        let Some(node) = self.nodes.get(id) else { return };
        flat.push((id.to_string(), depth));
        if node.expanded {
            on_path.push(id.to_string());
            let children = node.children.clone();
            for c in &children {
                self.dfs(c, depth + 1, flat, on_path);
            }
            on_path.pop();
        }
    }

    #[cfg(test)]
    pub fn flat_len(&self) -> usize { self.flat.len() }
    pub fn move_down(&mut self) { if self.selected + 1 < self.flat.len() { self.selected += 1; } }
    pub fn move_up(&mut self) { self.selected = self.selected.saturating_sub(1); }
    #[cfg(test)]
    pub fn contains(&self, id: &str) -> bool { self.flat.iter().any(|(i, _)| i == id) }

    pub fn toggle_expand(&mut self) {
        if let Some((id, _)) = self.flat.get(self.selected).cloned() {
            if let Some(n) = self.nodes.get_mut(&id) && !n.children.is_empty() {
                n.expanded = !n.expanded;
            }
            self.rebuild_flat();
        }
    }
}

impl View for GraphTree {
    fn render(&mut self, frame: &mut ratatui::Frame) {
        use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
        let rows: Vec<ListItem> = self
            .flat
            .iter()
            .map(|(id, depth)| {
                let n = &self.nodes[id];
                let marker = if n.children.is_empty() { "  " } else if n.expanded { "v " } else { "> " };
                ListItem::new(format!("{}{}{}", "  ".repeat(*depth), marker, n.label))
            })
            .collect();
        let mut state = ListState::default();
        if !self.flat.is_empty() {
            state.select(Some(self.selected));
        }
        let list = List::new(rows)
            .block(Block::default().borders(Borders::ALL).title("Dependency graph (Enter/Space expand, Esc quit)"))
            .highlight_symbol("* ");
        frame.render_stateful_widget(list, frame.area(), &mut state);
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return true,
            KeyCode::Down => self.move_down(),
            KeyCode::Up => self.move_up(),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_expand(),
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> GraphTree {
        GraphTree::from_edges(
            &[("root", "root-pkg"), ("a", "a-pkg"), ("b", "b-pkg"), ("c", "c-pkg")],
            &[("root", "a"), ("root", "b"), ("a", "c")],
            vec!["root".into()],
        )
    }

    #[test]
    fn collapsed_root_shows_only_root() {
        assert_eq!(tree().flat_len(), 1);
    }

    #[test]
    fn expand_reveals_children() {
        let mut t = tree();
        t.toggle_expand();
        assert_eq!(t.flat_len(), 3);
    }

    #[test]
    fn expand_nested() {
        let mut t = tree();
        t.toggle_expand();
        t.move_down();
        t.toggle_expand();
        assert!(t.contains("c"));
    }
}

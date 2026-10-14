//! Unknown content follows a rename (spec D-4). [`Preserved`] is keyed by object name, so a
//! renamed Table, Query or Column would otherwise lose the unknown items read with it.
//!
//! **Copied, not moved.** The app keeps [`Preserved`] beside the Document, outside undo: after
//! an undone rename the old name must still find its items. Items left under a name no object
//! has are never written (as after a delete).

use super::{
    Key, Preserved, expression_header, parse_relationship, relationship_header, same_header,
};
use crate::syntax::{Item, Node, quote_name};

impl Preserved {
    /// Table and Query `old` is now `new`: copies their unknown items, and those of the
    /// Relationships on it, to the new name.
    pub fn rename_table(&mut self, old: &str, new: &str) {
        for (key, kind) in [
            (Key::Table as fn(String) -> Key, "table"),
            (Key::Query, "query"),
        ] {
            if let Some(items) = self.trees.get(&key(old.into())) {
                let mut items = items.clone();
                for item in &mut items {
                    if let Item::Node(n) = item
                        && n.kind == kind
                        && same_header(&n.header, &quote_name(old))
                    {
                        n.header = quote_name(new);
                    }
                }
                self.trees.insert(key(new.into()), items);
            }
        }
        self.rename_in_relationships(|t, c| (t == old).then(|| (new.to_owned(), c.to_owned())));
    }

    /// Column `old` of Table `table` is now `new`: copies its unknown items, and those of the
    /// Relationships on it, to the new name.
    pub fn rename_column(&mut self, table: &str, old: &str, new: &str) {
        self.rename_in_table(table, "column", old, new);
        self.rename_in_relationships(|t, c| {
            (t == table && c == old).then(|| (t.to_owned(), new.to_owned()))
        });
    }

    /// Measure `old` of Table `table` is now `new`: copies its unknown items to the new name.
    pub fn rename_measure(&mut self, table: &str, old: &str, new: &str) {
        self.rename_in_table(table, "measure", old, new);
    }

    /// Copies the `kind` object `old` of Table `table` (a Column or Measure; plain or an
    /// expression node) to `new`.
    fn rename_in_table(&mut self, table: &str, kind: &str, old: &str, new: &str) {
        let tree = self.trees.get_mut(&Key::Table(table.into()));
        let node = tree.and_then(|items| {
            items.iter_mut().find_map(|i| match i {
                Item::Node(n)
                    if n.kind == "table" && same_header(&n.header, &quote_name(table)) =>
                {
                    Some(n)
                }
                _ => None,
            })
        });
        if let Some(node) = node {
            let copy = node.items.iter().find_map(|i| match i {
                Item::Node(c) if c.kind == kind => {
                    let (name, expression) = expression_header(&c.header)?;
                    (name == old).then(|| Node {
                        header: match expression {
                            Some(_) => format!("{} =", quote_name(new)),
                            None => quote_name(new),
                        },
                        ..c.clone()
                    })
                }
                _ => None,
            });
            if let Some(copy) = copy {
                upsert(&mut node.items, copy);
            }
        }
    }

    /// Copies each Relationship node with an end `to` renames ((table, column) → new names).
    fn rename_in_relationships(&mut self, to: impl Fn(&str, &str) -> Option<(String, String)>) {
        let Some(items) = self.trees.get_mut(&Key::Relationships) else {
            return;
        };
        let copies: Vec<Node> = items
            .iter()
            .filter_map(|i| match i {
                Item::Node(n) if n.kind == "relationship" => {
                    let (ft, fc, tt, tc) = parse_relationship(&n.header)?;
                    let from = to(&ft, &fc);
                    let target = to(&tt, &tc);
                    if from.is_none() && target.is_none() {
                        return None;
                    }
                    let (ft, fc) = from.unwrap_or((ft, fc));
                    let (tt, tc) = target.unwrap_or((tt, tc));
                    Some(Node {
                        header: relationship_header(&ft, &fc, &tt, &tc),
                        ..n.clone()
                    })
                }
                _ => None,
            })
            .collect();
        for copy in copies {
            upsert(items, copy);
        }
    }
}

/// Puts `node` in `items`, replacing the node of the same kind and header if there is one (so
/// a later save merges its items once).
fn upsert(items: &mut Vec<Item>, node: Node) {
    let same = items.iter_mut().find_map(|i| match i {
        Item::Node(n) if n.kind == node.kind && same_header(&n.header, &node.header) => Some(n),
        _ => None,
    });
    match same {
        Some(n) => *n = node,
        None => items.push(Item::Node(node)),
    }
}

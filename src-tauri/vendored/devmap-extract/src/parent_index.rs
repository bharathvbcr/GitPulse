//! A bounded, extraction-scoped index of syntax-tree parents.
//!
//! tree-sitter reconstructs `Node::parent()` from the root. The extractor asks
//! for the same ancestors from many independent language rules, making this a
//! large cost even on ordinary source. A cursor supplies the same links in one
//! walk. Misses retain tree-sitter's answer and the existing deadline checks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;
use tree_sitter::{ffi, Node};

const MAX_LINKS: usize = 131_072;
const MAX_ACTIVE_TREES: usize = 8;

struct Frame {
    owner: Rc<()>,
    tree: *const ffi::TSTree,
    parents: HashMap<usize, Option<ffi::TSNode>>,
}

thread_local! {
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// The borrow pins the tree until its links are removed; Rc also prevents
/// moving the guard to another thread and retiring the wrong thread's cache.
struct ParentIndex<'tree> {
    _root: Node<'tree>,
    owner: Rc<()>,
}

impl<'tree> ParentIndex<'tree> {
    fn new(root: Node<'tree>, deadline: Instant) -> Self {
        let guard = Self {
            _root: root,
            owner: Rc::new(()),
        };
        if FRAMES.with(|frames| frames.borrow().len() >= MAX_ACTIVE_TREES) {
            return guard;
        }
        let mut parents = HashMap::with_capacity(root.descendant_count().min(MAX_LINKS));
        let mut ancestors = Vec::new();
        let mut cursor = root.walk();
        'walk: loop {
            if parents.len() >= MAX_LINKS
                || (parents.len() % 256 == 0 && Instant::now() >= deadline)
            {
                break;
            }
            let node = cursor.node();
            parents.insert(node.id(), ancestors.last().copied());
            if cursor.goto_first_child() {
                ancestors.push(node.into_raw());
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break 'walk;
                }
                ancestors.pop();
            }
        }
        FRAMES.with(|frames| {
            frames.borrow_mut().push(Frame {
                owner: guard.owner.clone(),
                tree: root.into_raw().tree,
                parents,
            })
        });
        guard
    }
}

impl Drop for ParentIndex<'_> {
    fn drop(&mut self) {
        // Remove by ownership, not stack position. Nested extraction, unwinding,
        // and even out-of-order guard drops cannot resurrect a retired tree.
        FRAMES.with(|frames| {
            frames
                .borrow_mut()
                .retain(|frame| !Rc::ptr_eq(&frame.owner, &self.owner))
        });
    }
}

/// The guard cannot escape this function (including through `mem::forget`).
/// This scoped API is what makes the borrowed raw-node cache safe to expose.
pub(crate) fn with_index<R>(
    tree: &tree_sitter::Tree,
    deadline: Instant,
    work: impl FnOnce() -> R,
) -> R {
    let _guard = ParentIndex::new(tree.root_node(), deadline);
    work()
}

/// Outer None means uncached; inner None is the cached root's real parent.
pub(crate) fn parent(node: Node<'_>) -> Option<Option<Node<'_>>> {
    let tree = node.into_raw().tree;
    FRAMES.with(|frames| {
        frames
            .borrow()
            .iter()
            .rev()
            .find(|frame| frame.tree == tree)
            .and_then(|frame| frame.parents.get(&node.id()))
            .map(|parent| {
                parent.map(|raw| {
                    // SAFETY: entries come only from live cursor Nodes. A guard
                    // borrows their immutable Tree and removes them before that
                    // borrow ends. The checked tree identity matches the input
                    // Node, so the returned Node may use the input's lifetime.
                    // No pointer cast, transmute, or fabricated TSNode is used.
                    debug_assert_eq!(raw.tree, tree);
                    debug_assert!(!raw.id.is_null());
                    unsafe { Node::from_raw(raw) }
                })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tree(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    fn every_cached_parent_matches_the_native_tree_including_errors() {
        for source in [
            "fn f(a:i32) { let b = a; g(b); }",
            "fn ((((",
            "",
            "fn f() { ((())) }",
            "// comment\nstruct S { x: usize }\n",
        ] {
            let tree = tree(source);
            let root = tree.root_node();
            let guard = ParentIndex::new(root, Instant::now() + Duration::from_secs(5));
            let mut nodes = vec![root];
            while let Some(node) = nodes.pop() {
                let cached = parent(node).expect("small tree is fully cached");
                assert_eq!(cached, node.parent());
                assert_eq!(
                    cached.map(|p| p.byte_range()),
                    node.parent().map(|p| p.byte_range())
                );
                nodes.extend(node.children(&mut node.walk()));
            }
            drop(guard);
            assert!(parent(root).is_none());
        }
    }

    #[test]
    fn tree_identity_nesting_unwind_and_out_of_order_retirement_are_isolated() {
        let first = tree("fn a() {}");
        let second = tree("fn b() { c(); }");
        let first_guard =
            ParentIndex::new(first.root_node(), Instant::now() + Duration::from_secs(5));
        assert!(parent(second.root_node()).is_none());
        let second_guard =
            ParentIndex::new(second.root_node(), Instant::now() + Duration::from_secs(5));
        drop(first_guard);
        assert!(parent(first.root_node()).is_none());
        assert_eq!(parent(second.root_node()), Some(None));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _nested =
                ParentIndex::new(first.root_node(), Instant::now() + Duration::from_secs(5));
            panic!("retire on unwind");
        }));
        assert!(result.is_err());
        assert!(parent(first.root_node()).is_none());
        assert_eq!(parent(second.root_node()), Some(None));
        drop(second_guard);
        assert!(parent(second.root_node()).is_none());
    }

    #[test]
    fn expired_index_build_is_a_cache_miss_and_never_an_invented_root() {
        let tree = tree("fn a() { b(); }");
        let _guard = ParentIndex::new(tree.root_node(), Instant::now());
        assert!(parent(tree.root_node().child(0).unwrap()).is_none());
    }

    #[test]
    fn capacity_and_nested_depth_are_bounded_with_native_fallback() {
        let tree = tree(&";".repeat(MAX_LINKS + 128));
        let root = tree.root_node();
        let guard = ParentIndex::new(root, Instant::now() + Duration::from_secs(5));
        FRAMES.with(|frames| assert_eq!(frames.borrow().last().unwrap().parents.len(), MAX_LINKS));
        let last = root.child(root.child_count() - 1).unwrap();
        assert!(
            parent(last).is_none(),
            "the capped tail is an explicit cache miss"
        );
        assert_eq!(
            super::super::treesitter::bounded_parent(last),
            last.parent()
        );
        let mut guards = vec![guard];
        for _ in 0..MAX_ACTIVE_TREES + 4 {
            guards.push(ParentIndex::new(root, Instant::now()));
        }
        FRAMES.with(|frames| assert_eq!(frames.borrow().len(), MAX_ACTIVE_TREES));
        drop(guards);
        assert!(parent(root).is_none());
    }

    #[test]
    fn concurrent_scopes_and_tree_allocator_reuse_cannot_share_parents() {
        std::thread::scope(|threads| {
            for lane in 0..8 {
                threads.spawn(move || {
                    for round in 0..64 {
                        let source = format!("fn f{lane}_{round}() {{ other(); }}");
                        let tree = tree(&source);
                        let root = tree.root_node();
                        with_index(&tree, Instant::now() + Duration::from_secs(5), || {
                            let node = root.child(0).unwrap();
                            assert_eq!(parent(node), Some(node.parent()));
                        });
                        assert!(parent(root).is_none());
                    }
                });
            }
        });
    }
}

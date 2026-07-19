//! Unit tests that build classic data structures using `Sdarc` / `WeakSdarc`
//! and verify that every allocation is properly freed after dropping.
//!
//! Each test creates its own `Sdarc<AtomicI64>` counter.  Every `DropCounted`
//! holds a reference to it, incrementing on creation / clone and decrementing
//! on drop.  After the structure is dropped and the collector has finished,
//! the counter must be back at zero.

use crate::atomic_sdarc::AtomicNullableSdarc;
use crate::collector::collector_update_now_and_wait;
use crate::sdarc::Sdarc;
use crate::weak_sdarc::WeakSdarc;
use std::sync::atomic::{AtomicI64, Ordering};
// ---------------------------------------------------------------------------
// DropCounted — per-test leak detector via a shared `Sdarc<AtomicI64>`
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct DropCounted {
    #[allow(dead_code)]
    id: u64,
    counter: Sdarc<AtomicI64>,
}

impl DropCounted {
    fn new(id: u64, counter: &Sdarc<AtomicI64>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            counter: counter.clone(),
        }
    }
}

impl Clone for DropCounted {
    fn clone(&self) -> Self {
        self.counter.fetch_add(1, Ordering::Relaxed);
        Self {
            id: self.id,
            counter: self.counter.clone(),
        }
    }
}

impl Drop for DropCounted {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

fn make_counter() -> Sdarc<AtomicI64> {
    Sdarc::new(AtomicI64::new(0))
}

fn counter_value(c: &Sdarc<AtomicI64>) -> i64 {
    c.load(Ordering::Relaxed)
}

// ===========================================================================
// 1. Singly linked list
// ===========================================================================

struct SllNode {
    #[allow(dead_code)]
    value: DropCounted,
    next: Option<Sdarc<SllNode>>,
}

impl SllNode {
    fn new(id: u64, next: Option<Sdarc<SllNode>>, counter: &Sdarc<AtomicI64>) -> Self {
        Self {
            value: DropCounted::new(id, counter),
            next,
        }
    }
}

/// Build a singly linked list of length `n` (ids 1..n). Returns the head.
/// Built tail-to-head so no post-construction mutation is needed.
fn sll_build(n: usize, counter: &Sdarc<AtomicI64>) -> Sdarc<SllNode> {
    let mut head: Option<Sdarc<SllNode>> = None;
    for i in (1..=n).rev() {
        head = Some(Sdarc::new(SllNode::new(i as u64, head, counter)));
    }
    head.unwrap()
}

#[test]
fn singly_linked_list_drops_correctly() {
    let counter = make_counter();
    {
        let list = sll_build(20, &counter);

        // Traverse and verify
        let mut count = 0;
        let mut curr = Some(list.clone());
        while let Some(node) = curr {
            count += 1;
            curr = node.next.clone();
        }
        assert_eq!(count, 20);
    }
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "singly linked list leak: {} still allocated",
        counter_value(&counter)
    );
}

// ===========================================================================
// 2. Doubly linked list — forward link via Sdarc, backward via WeakSdarc
// ===========================================================================

struct DllNode {
    #[allow(dead_code)]
    value: DropCounted,
    prev: Option<WeakSdarc<DllNode>>,
    /// Use `AtomicNullableSdarc` so we can store the forward link after
    /// construction (Sdarc doesn't expose `&mut`).
    next: AtomicNullableSdarc<DllNode>,
}

impl DllNode {
    fn new(id: u64, prev: Option<WeakSdarc<DllNode>>, counter: &Sdarc<AtomicI64>) -> Self {
        Self {
            value: DropCounted::new(id, counter),
            prev,
            next: AtomicNullableSdarc::new(),
        }
    }
}

/// Build a doubly linked list of length `n`. Returns (head, tail).
fn dll_build(n: usize, counter: &Sdarc<AtomicI64>) -> (Sdarc<DllNode>, Sdarc<DllNode>) {
    assert!(n >= 1);

    let mut nodes: Vec<Sdarc<DllNode>> = Vec::with_capacity(n);
    for i in 0..n {
        let prev_weak = if i > 0 {
            Some(nodes[i - 1].downgrade())
        } else {
            None
        };
        nodes.push(Sdarc::new(DllNode::new(i as u64, prev_weak, counter)));
    }

    // Link forward: node[i].next = node[i+1]
    for i in 0..n - 1 {
        nodes[i].next.store(Some(nodes[i + 1].clone()));
    }

    let head = nodes[0].clone();
    let tail = nodes[n - 1].clone();
    (head, tail)
}

#[test]
fn doubly_linked_list_drops_correctly() {
    let counter = make_counter();
    let n = 20;
    {
        let (head, tail) = dll_build(n, &counter);

        // Traverse forward
        let mut count = 0;
        let mut curr = Some(head.clone());
        while let Some(node) = curr {
            count += 1;
            curr = node.next.load();
        }
        assert_eq!(count, n, "forward traversal wrong length");

        // Traverse backward from tail via weak-upgrade, verifying each
        // child's `prev` upgrade succeeds and points to the correct node.
        let mut backward: Vec<u64> = Vec::new();
        let mut maybe = Some(tail);
        while let Some(node) = maybe {
            backward.push(node.value.id);
            maybe = match &node.prev {
                Some(w) => {
                    let upgraded = w.upgrade();
                    assert!(
                        upgraded.is_some(),
                        "weak upgrade failed at id={}",
                        node.value.id
                    );
                    upgraded
                }
                None => None,
            };
        }
        assert_eq!(backward.len(), n, "backward traversal wrong length");
        // Verify ids are consecutive in reverse: 19, 18, ..., 0
        for (i, &id) in backward.iter().enumerate() {
            assert_eq!(id, (n - 1 - i) as u64);
        }
    }
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "doubly linked list leak: {} still allocated",
        counter_value(&counter)
    );
}

// ===========================================================================
// 3. Binary tree without parent reference
// ===========================================================================

struct BtNode {
    #[allow(dead_code)]
    value: DropCounted,
    left: Option<Sdarc<BtNode>>,
    right: Option<Sdarc<BtNode>>,
}

impl BtNode {
    fn leaf(id: u64, counter: &Sdarc<AtomicI64>) -> Self {
        Self {
            value: DropCounted::new(id, counter),
            left: None,
            right: None,
        }
    }

    fn branch(
        id: u64,
        left: Sdarc<BtNode>,
        right: Sdarc<BtNode>,
        counter: &Sdarc<AtomicI64>,
    ) -> Self {
        Self {
            value: DropCounted::new(id, counter),
            left: Some(left),
            right: Some(right),
        }
    }
}

/// Build a perfect binary tree of depth `d` (root at depth 0, 2^d - 1 nodes).
fn bt_build_perfect(depth: usize, counter: &Sdarc<AtomicI64>) -> Option<Sdarc<BtNode>> {
    fn build(id: &mut u64, depth: usize, counter: &Sdarc<AtomicI64>) -> Option<Sdarc<BtNode>> {
        if depth == 0 {
            return None;
        }
        let my_id = *id;
        *id += 1;
        let left = build(id, depth - 1, counter);
        let right = build(id, depth - 1, counter);
        let node = match (left, right) {
            (Some(l), Some(r)) => BtNode::branch(my_id, l, r, counter),
            (None, None) => BtNode::leaf(my_id, counter),
            _ => unreachable!(),
        };
        Some(Sdarc::new(node))
    }

    let mut id = 0;
    build(&mut id, depth, counter)
}

#[test]
fn binary_tree_no_parent_drops_correctly() {
    let counter = make_counter();
    let depth = 6; // 2^6 - 1 = 63 nodes
    {
        let root = bt_build_perfect(depth, &counter).expect("root should exist");

        fn count(node: &BtNode) -> usize {
            1 + node.left.as_ref().map(|n| count(n)).unwrap_or(0)
                + node.right.as_ref().map(|n| count(n)).unwrap_or(0)
        }
        assert_eq!(count(&root), (1 << depth) - 1);
    }
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "binary tree leak: {} still allocated",
        counter_value(&counter)
    );
}

// ===========================================================================
// 4. Binary tree with parent WeakSdarc
// ===========================================================================

struct BtpNode {
    #[allow(dead_code)]
    value: DropCounted,
    parent: Option<WeakSdarc<BtpNode>>,
    /// Use `AtomicNullableSdarc` so the parent can set child links after
    /// constructing the child (which needs the parent's Sdarc for downgrade).
    left: AtomicNullableSdarc<BtpNode>,
    right: AtomicNullableSdarc<BtpNode>,
}

impl BtpNode {
    fn new(id: u64, parent: Option<&Sdarc<BtpNode>>, counter: &Sdarc<AtomicI64>) -> Self {
        Self {
            value: DropCounted::new(id, counter),
            parent: parent.map(|p| p.downgrade()),
            left: AtomicNullableSdarc::new(),
            right: AtomicNullableSdarc::new(),
        }
    }
}

/// Build a perfect binary tree of depth `d` with parent weak refs.
fn btp_build_perfect(depth: usize, counter: &Sdarc<AtomicI64>) -> Option<Sdarc<BtpNode>> {
    fn build(
        id: &mut u64,
        depth: usize,
        parent: Option<&Sdarc<BtpNode>>,
        counter: &Sdarc<AtomicI64>,
    ) -> Option<Sdarc<BtpNode>> {
        if depth == 0 {
            return None;
        }
        let my_id = *id;
        *id += 1;
        let node = Sdarc::new(BtpNode::new(my_id, parent, counter));

        let left = build(id, depth - 1, Some(&node), counter);
        let right = build(id, depth - 1, Some(&node), counter);

        if let Some(l) = left {
            node.left.store(Some(l));
        }
        if let Some(r) = right {
            node.right.store(Some(r));
        }

        Some(node)
    }

    let mut id = 0;
    build(&mut id, depth, None, counter)
}

#[test]
fn binary_tree_with_parent_weak_drops_correctly() {
    let counter = make_counter();
    let depth = 6;
    {
        let root = btp_build_perfect(depth, &counter).expect("root should exist");

        fn count(node: &BtpNode) -> usize {
            1 + node.left.load().as_ref().map(|n| count(n)).unwrap_or(0)
                + node.right.load().as_ref().map(|n| count(n)).unwrap_or(0)
        }
        assert_eq!(count(&root), (1 << depth) - 1);

        // Verify parent weak refs work while tree is alive.
        // root's immediate children:
        if let Some(left) = root.left.load() {
            let parent_upgrade = left.parent.as_ref().and_then(|w| w.upgrade());
            assert!(parent_upgrade.is_some());
            let a = &parent_upgrade.unwrap();
            assert!(Sdarc::<BtpNode>::ptr_eq(a, &root));
        }
        if let Some(right) = root.right.load() {
            let parent_upgrade = right.parent.as_ref().and_then(|w| w.upgrade());
            assert!(parent_upgrade.is_some());
            let a = &parent_upgrade.unwrap();
            assert!(Sdarc::<BtpNode>::ptr_eq(a, &root));
        }

        // Walk from a deep leaf back up to root via parent weak upgrades.
        // In a perfect tree, going left repeatedly reaches the leftmost leaf.
        let mut curr = root.clone();
        let mut depth_walked = 0;
        loop {
            let left = curr.left.load();
            if left.is_none() {
                break;
            }
            curr = left.unwrap();
            depth_walked += 1;
        }
        // `curr` is now the leftmost leaf.  Walk back up via parent.
        let mut up_count = 0;
        loop {
            let parent_upgrade = curr.parent.as_ref().and_then(|w| w.upgrade());
            match parent_upgrade {
                Some(p) => {
                    up_count += 1;
                    curr = p;
                }
                None => break,
            }
        }
        assert_eq!(up_count, depth_walked);
        // We should be back at the root
        assert!(Sdarc::<BtpNode>::ptr_eq(&curr, &root));
    }
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "binary tree with parent weak leak: {} still allocated",
        counter_value(&counter)
    );
}

// ===========================================================================
// 5. Custom type whose `Drop` allocates `Sdarc` then drops it
// ===========================================================================

/// This type allocates a fresh `Sdarc<DropCounted>` in its destructor and
/// immediately drops it — exercising the code path where an Sdarc is created
/// and destroyed entirely inside another Sdarc's `drop`, which may run in the
/// collector thread (and therefore be added to `COLLECTOR_THREAD_LOCAL`).
#[derive(Debug)]
struct DropAllocator {
    #[allow(dead_code)]
    value: DropCounted,
    /// A separate counter used by the temporary allocations inside `drop()`.
    drop_counter: Sdarc<AtomicI64>,
}

impl DropAllocator {
    fn new(id: u64, counter: &Sdarc<AtomicI64>, drop_counter: &Sdarc<AtomicI64>) -> Self {
        Self {
            value: DropCounted::new(id, counter),
            drop_counter: drop_counter.clone(),
        }
    }
}

impl Drop for DropAllocator {
    fn drop(&mut self) {
        // Allocate fresh Sdarcs using the drop_counter, clone, then drop all
        // within this destructor.  If this runs in the collector thread the
        // inner counters are queued via COLLECTOR_THREAD_LOCAL.
        let a = Sdarc::new(DropCounted::new(999, &self.drop_counter));
        let b = a.clone();
        let c = b.clone();
        drop(a);
        drop(b);
        drop(c);
    }
}

#[test]
fn drop_that_allocates_sdarc_cleans_up() {
    let counter = make_counter();
    let drop_counter = make_counter();

    // Each DropAllocator's drop() creates+destroys 3 DropCounted referencing
    // `drop_counter`.  The DropAllocator's own DropCounted references `counter`.
    {
        let container = Sdarc::new(DropAllocator::new(1, &counter, &drop_counter));
        let clones: Vec<_> = (0..5).map(|_| container.clone()).collect();
        drop(container);
        drop(clones);
    }

    // Two collections are needed: the first frees the DropAllocator (which
    // creates+destroys inner Sdarcs in its destructor); the inner Sdarcs are
    // buffered into pending_to_track and won't be tracked until the *next*
    // outer iteration — so we wait for two full cycles.
    collector_update_now_and_wait();
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "drop-allocator outer leak: {} still allocated",
        counter_value(&counter)
    );
    assert_eq!(
        counter_value(&drop_counter),
        0,
        "drop-allocator inner leak: {} still allocated",
        counter_value(&drop_counter)
    );
}

// ===========================================================================
// 6. Deep left-skewed tree — exercises collector's inner iteration loop
// ===========================================================================

/// A degenerate (left-skewed) tree of depth ~100. When the root is dropped,
/// its left child's ref count goes to 0, which drops it, cascading down.
/// The collector's inner-iteration / `COLLECTOR_THREAD_LOCAL` path handles
/// this chain without needing O(depth) outer collection cycles.
#[test]
fn deep_left_skewed_tree_drops_correctly() {
    let counter = make_counter();
    let depth = 100;
    {
        let mut curr: Option<Sdarc<BtNode>> = None;
        for i in (0..depth).rev() {
            let node = if let Some(child) = curr.take() {
                BtNode::branch(
                    i as u64,
                    child,
                    Sdarc::new(BtNode::leaf((i * 1000) as u64, &counter)),
                    &counter,
                )
            } else {
                BtNode::leaf(i as u64, &counter)
            };
            curr = Some(Sdarc::new(node));
        }
        let _root = curr.unwrap();
    }
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "deep tree leak: {} still allocated",
        counter_value(&counter)
    );
}

// ===========================================================================
// 7. Vec<Sdarc<T>> — common usage pattern
// ===========================================================================

#[test]
fn vec_of_sdarc_drops_correctly() {
    let counter = make_counter();
    {
        let mut v: Vec<Sdarc<DropCounted>> = Vec::new();
        for i in 0..50 {
            v.push(Sdarc::new(DropCounted::new(i, &counter)));
        }
        assert_eq!(v.len(), 50);
    }
    collector_update_now_and_wait();
    assert_eq!(
        counter_value(&counter),
        0,
        "vec leak: {} still allocated",
        counter_value(&counter)
    );
}

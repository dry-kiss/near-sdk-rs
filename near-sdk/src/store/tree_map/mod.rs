mod entry;
mod impls;
mod iter;

pub use entry::Entry;
pub use iter::{Iter, IterMut, Values, ValuesMut};
// pub use iter::{Iter, IterMut, Keys, Values, ValuesMut};

use borsh::{BorshDeserialize, BorshSerialize};
use std::borrow::Borrow;

use super::lookup_map as lm;
use crate::crypto_hash::{CryptoHasher, Sha256};
use crate::store::free_list::{FreeList, FreeListIndex};
use crate::store::LookupMap;
use crate::{env, IntoStorageKey};

fn expect<T>(val: Option<T>) -> T {
    val.unwrap_or_else(|| env::abort())
}

/// TreeMap based on AVL-tree
///
/// Runtime complexity (worst case):
/// - `get`/`contains_key`:     O(1) - UnorderedMap lookup
/// - `insert`/`remove`:        O(log(N))
/// - `min`/`max`:              O(log(N))
/// - `above`/`below`:          O(log(N))
/// - `range` of K elements:    O(Klog(N))
///
pub struct TreeMap<K, V, H = Sha256>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    values: LookupMap<K, V, H>,
    tree: Tree<K>,
}

//? Manual implementations needed only because borsh derive is leaking field types
// https://github.com/near/borsh-rs/issues/41
impl<K, V, H> BorshSerialize for TreeMap<K, V, H>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    fn serialize<W: borsh::maybestd::io::Write>(
        &self,
        writer: &mut W,
    ) -> Result<(), borsh::maybestd::io::Error> {
        BorshSerialize::serialize(&self.values, writer)?;
        BorshSerialize::serialize(&self.tree, writer)?;
        Ok(())
    }
}

impl<K, V, H> BorshDeserialize for TreeMap<K, V, H>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    fn deserialize(buf: &mut &[u8]) -> Result<Self, borsh::maybestd::io::Error> {
        Ok(Self {
            values: BorshDeserialize::deserialize(buf)?,
            tree: BorshDeserialize::deserialize(buf)?,
        })
    }
}

#[derive(BorshDeserialize, BorshSerialize)]
struct Tree<K>
where
    K: BorshSerialize + Ord,
{
    root: Option<FreeListIndex>,
    nodes: FreeList<Node<K>>,
}

impl<K> Tree<K>
where
    K: BorshSerialize + Ord,
{
    fn new<S>(prefix: S) -> Self
    where
        S: IntoStorageKey,
    {
        Tree { root: None, nodes: FreeList::new(prefix) }
    }
}

#[derive(Clone, BorshSerialize, BorshDeserialize)]
struct Node<K> {
    // id: FreeListIndex,
    key: K,                     // key stored in a node
    lft: Option<FreeListIndex>, // left link of a node
    rgt: Option<FreeListIndex>, // right link of a node
    ht: u32,                    // height of a subtree at a node
}

impl<K> Node<K>
where
    K: Ord + Clone + BorshSerialize + BorshDeserialize,
{
    fn of(key: K) -> Self {
        Self { key, lft: None, rgt: None, ht: 0 }
    }

    fn left<'a>(&self, list: &'a FreeList<Node<K>>) -> Option<(FreeListIndex, &'a Node<K>)> {
        self.lft.and_then(|id| list.get(id).map(|node| (id, node)))
    }

    fn right<'a>(&self, list: &'a FreeList<Node<K>>) -> Option<(FreeListIndex, &'a Node<K>)> {
        self.rgt.and_then(|id| list.get(id).map(|node| (id, node)))
    }
}

impl<K, V> TreeMap<K, V, Sha256>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
{
    pub fn new<S>(prefix: S) -> Self
    where
        S: IntoStorageKey,
    {
        Self::with_hasher(prefix)
    }
}

impl<K, V, H> TreeMap<K, V, H>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    pub fn with_hasher<S>(prefix: S) -> Self
    where
        S: IntoStorageKey,
    {
        let prefix = prefix.into_storage_key();
        let mut vec_key = prefix.into_storage_key();
        let map_key = [vec_key.as_slice(), b"v"].concat();
        vec_key.push(b'n');
        Self { values: LookupMap::with_hasher(map_key), tree: Tree::new(vec_key) }
    }

    /// Return the amount of elements inside of the map.
    pub fn len(&self) -> u32 {
        self.tree.nodes.len()
    }

    /// Returns true if there are no elements inside of the map.
    pub fn is_empty(&self) -> bool {
        self.tree.nodes.is_empty()
    }
}

impl<K, V, H> TreeMap<K, V, H>
where
    K: Ord + Clone + BorshSerialize + BorshDeserialize,
    V: BorshSerialize + BorshDeserialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    /// Clears the map, removing all key-value pairs. Keeps the allocated memory
    /// for reuse.
    pub fn clear(&mut self) {
        self.tree.root = None;
        for k in self.tree.nodes.drain() {
            // Set instead of remove to avoid loading the value from storage.
            self.values.set(k.key, None);
        }
    }

    // fn save(&mut self, node: Node<K>) {
    // todo!()
    // if node.id < self.len() {
    //     self.tree.replace(node.id, node);
    // } else {
    //     self.tree.push(node);
    // }
    // }

    /// Returns `true` if the map contains a value for the specified key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`BorshSerialize`] and [`ToOwned<Owned = K>`](ToOwned) on the borrowed form *must* match
    /// those for the key type.
    pub fn contains_key<Q: ?Sized>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: BorshSerialize + ToOwned<Owned = K> + Ord,
    {
        self.values.contains_key(k)
    }

    /// Returns a reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`BorshSerialize`] and [`ToOwned<Owned = K>`](ToOwned) on the borrowed form *must* match
    /// those for the key type.
    pub fn get<Q: ?Sized>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: BorshSerialize + ToOwned<Owned = K>,
    {
        self.values.get(k)
    }

    /// Returns a mutable reference to the value corresponding to the key.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`BorshSerialize`] and [`ToOwned<Owned = K>`](ToOwned) on the borrowed form *must* match
    /// those for the key type.
    pub fn get_mut<Q: ?Sized>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: BorshSerialize + ToOwned<Owned = K>,
    {
        self.values.get_mut(k)
    }

    /// Inserts a key-value pair into the map.
    ///
    /// If the map did not have this key present, [`None`] is returned.
    ///
    /// If the map did have this key present, the value is updated, and the old
    /// value is returned. The key is not updated, though; this matters for
    /// types that can be `==` without being identical.
    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Clone + BorshDeserialize,
    {
        // fix pattern when refactor
        match self.values.entry(key.clone()) {
            lm::Entry::Occupied(mut v) => Some(core::mem::replace(v.get_mut(), value)),
            lm::Entry::Vacant(v) => {
                self.tree.internal_insert(key);
                v.insert(value);
                None
            }
        }
    }

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`BorshSerialize`] and [`ToOwned<Owned = K>`](ToOwned) on the borrowed form *must* match
    /// those for the key type.
    pub fn remove<Q: ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q> + BorshDeserialize,
        Q: BorshSerialize + ToOwned<Owned = K> + Ord,
    {
        let existing = self.values.remove(key);
        if existing.is_some() {
            self.tree.root = self.tree.do_remove(key);
        }
        existing
    }

    /// Returns the smallest key that is greater or equal to key given as the parameter
    fn ceil_key<'a: 'b, 'b: 'a>(&'a self, key: &'b K) -> Option<&K> {
        if self.contains_key(key) {
            Some(key)
        } else {
            self.tree.higher(key)
        }
    }

    /// Returns the largest key that is less or equal to key given as the parameter
    fn floor_key<'a: 'b, 'b: 'a>(&'a self, key: &'b K) -> Option<&K> {
        if self.contains_key(key) {
            Some(key)
        } else {
            self.tree.lower(key)
        }
    }
}

impl<K> Tree<K>
where
    K: Ord + Clone + BorshSerialize + BorshDeserialize,
{
    fn node(&self, id: FreeListIndex) -> Option<&Node<K>> {
        self.nodes.get(id)
    }

    /// Returns the smallest stored key from the tree
    fn min(&self) -> Option<&K> {
        let root = self.root?;
        self.min_at(root, root).map(|(n, _)| &n.key)
    }

    /// Returns the largest stored key from the tree
    fn max(&self) -> Option<&K> {
        let root = self.root?;
        self.max_at(root, root).map(|(n, _)| &n.key)
    }

    /// Returns the smallest key that is strictly greater than key given as the parameter
    fn higher(&self, key: &K) -> Option<&K> {
        let root = self.root?;
        self.above_at(root, key)
    }

    /// Returns the largest key that is strictly less than key given as the parameter
    fn lower(&self, key: &K) -> Option<&K> {
        let root = self.root?;
        self.below_at(root, key)
    }

    // /// Iterate all entries in ascending order: min to max, both inclusive
    // pub fn iter(&self) -> impl Iterator<Item = (K, V)> + '_ {
    //     Cursor::asc(self)
    // }

    // /// Iterate entries in ascending order: given key (exclusive) to max (inclusive)
    // pub fn iter_from(&self, key: K) -> impl Iterator<Item = (K, V)> + '_ {
    //     Cursor::asc_from(self, key)
    // }

    // /// Iterate all entries in descending order: max to min, both inclusive
    // pub fn iter_rev(&self) -> impl Iterator<Item = (K, V)> + '_ {
    //     Cursor::desc(self)
    // }

    // /// Iterate entries in descending order: given key (exclusive) to min (inclusive)
    // pub fn iter_rev_from(&self, key: K) -> impl Iterator<Item = (K, V)> + '_ {
    //     Cursor::desc_from(self, key)
    // }

    // /// Iterate entries in ascending order according to specified bounds.
    // ///
    // /// # Panics
    // ///
    // /// Panics if range start > end.
    // /// Panics if range start == end and both bounds are Excluded.
    // pub fn range(&self, r: (Bound<K>, Bound<K>)) -> impl Iterator<Item = (K, V)> + '_ {
    //     let (lo, hi) = match r {
    //         (Bound::Included(a), Bound::Included(b)) if a > b => env::panic_str("Invalid range."),
    //         (Bound::Excluded(a), Bound::Included(b)) if a > b => env::panic_str("Invalid range."),
    //         (Bound::Included(a), Bound::Excluded(b)) if a > b => env::panic_str("Invalid range."),
    //         (Bound::Excluded(a), Bound::Excluded(b)) if a >= b => env::panic_str("Invalid range."),
    //         (lo, hi) => (lo, hi),
    //     };

    //     Cursor::range(self, lo, hi)
    // }

    //
    // Internal utilities
    //

    /// Returns (node, parent node) of left-most lower (min) node starting from given node `at`.
    /// As min_at only traverses the tree down, if a node `at` is the minimum node in a subtree,
    /// its parent must be explicitly provided in advance.
    // TODO check if ok to pass reference to root instead of looking up index
    fn min_at(&self, mut at: FreeListIndex, p: FreeListIndex) -> Option<(&Node<K>, &Node<K>)> {
        let mut parent: Option<&Node<K>> = self.node(p);
        loop {
            let node = self.node(at);
            match node.as_ref().and_then(|n| n.lft) {
                Some(lft) => {
                    at = lft;
                    parent = node;
                }
                None => {
                    return node.and_then(|n| parent.map(|p| (n, p)));
                }
            }
        }
    }

    /// Returns (node, parent node) of right-most lower (max) node starting from given node `at`.
    /// As min_at only traverses the tree down, if a node `at` is the minimum node in a subtree,
    /// its parent must be explicitly provided in advance.
    fn max_at(&self, mut at: FreeListIndex, p: FreeListIndex) -> Option<(&Node<K>, &Node<K>)> {
        let mut parent: Option<&Node<K>> = self.node(p);
        loop {
            let node = self.node(at);
            match node.as_ref().and_then(|n| n.rgt) {
                Some(rgt) => {
                    parent = node;
                    at = rgt;
                }
                None => {
                    return node.and_then(|n| parent.map(|p| (n, p)));
                }
            }
        }
    }

    fn above_at(&self, mut at: FreeListIndex, key: &K) -> Option<&K> {
        let mut seen: Option<&K> = None;
        loop {
            let node = self.node(at);
            match node.as_ref().map(|n| &n.key) {
                Some(k) => {
                    if k.le(key) {
                        match node.and_then(|n| n.rgt) {
                            Some(rgt) => at = rgt,
                            None => break,
                        }
                    } else {
                        seen = Some(k);
                        match node.and_then(|n| n.lft) {
                            Some(lft) => at = lft,
                            None => break,
                        }
                    }
                }
                None => break,
            }
        }
        seen
    }

    fn below_at(&self, mut at: FreeListIndex, key: &K) -> Option<&K> {
        let mut seen: Option<&K> = None;
        loop {
            let node = self.node(at);
            match node.map(|n| &n.key) {
                Some(k) => {
                    if k.lt(key) {
                        seen = Some(k);
                        match node.and_then(|n| n.rgt) {
                            Some(rgt) => at = rgt,
                            None => break,
                        }
                    } else {
                        match node.and_then(|n| n.lft) {
                            Some(lft) => at = lft,
                            None => break,
                        }
                    }
                }
                None => break,
            }
        }
        seen
    }

    fn internal_insert(&mut self, key: K) {
        if let Some(root) = self.root {
            let node = expect(self.node(root)).clone();
            self.insert_at(node, root, key);
        } else {
            self.root = Some(self.nodes.insert(Node::of(key)));
        }
    }

    fn insert_at(&mut self, mut node: Node<K>, id: FreeListIndex, key: K) -> FreeListIndex {
        if key.eq(&node.key) {
            // This branch should not be hit, because we check for existence in insert.
            id
        } else {
            if key.lt(&node.key) {
                let idx = match node.lft {
                    Some(lft) => self.insert_at(expect(self.node(lft)).clone(), id, key),
                    None => self.nodes.insert(Node::of(key)),
                };
                node.lft = Some(idx);
            } else {
                let idx = match node.rgt {
                    Some(rgt) => self.insert_at(expect(self.node(rgt)).clone(), id, key),
                    None => self.nodes.insert(Node::of(key)),
                };
                node.rgt = Some(idx);
            };

            self.update_height(&mut node, id);
            self.enforce_balance(&mut node, id)
        }
    }

    // Calculate and save the height of a subtree at node `at`:
    // height[at] = 1 + max(height[at.L], height[at.R])
    fn update_height(&mut self, node: &mut Node<K>, id: FreeListIndex) {
        let lft = node.lft.and_then(|id| self.node(id).map(|n| n.ht)).unwrap_or_default();
        let rgt = node.rgt.and_then(|id| self.node(id).map(|n| n.ht)).unwrap_or_default();

        node.ht = 1 + std::cmp::max(lft, rgt);
        // Cloning and saving before enforcing balance seems weird, but I don't know a way
        // around without using unsafe, yet.
        *expect(self.nodes.get_mut(id)) = node.clone();
    }

    // Balance = difference in heights between left and right subtrees at given node.
    fn get_balance(&self, node: &Node<K>) -> i64 {
        let lht = node.lft.and_then(|id| self.node(id).map(|n| n.ht)).unwrap_or_default();
        let rht = node.rgt.and_then(|id| self.node(id).map(|n| n.ht)).unwrap_or_default();

        lht as i64 - rht as i64
    }

    // Left rotation of an AVL subtree with at node `at`.
    // New root of subtree is returned, caller is responsible for updating proper link from parent.
    fn rotate_left(&mut self, node: &mut Node<K>, id: FreeListIndex) -> FreeListIndex {
        // TODO clone shouldn't be required
        let (left_id, mut left) = expect(node.right(&self.nodes).map(|(id, n)| (id, n.clone())));
        let lft_rgt = left.rgt;

        // at.L = at.L.R
        node.lft = lft_rgt;

        // at.L.R = at
        left.rgt = Some(id);

        // at = at.L
        self.update_height(node, id);
        self.update_height(&mut left, id);

        left_id
    }

    // Right rotation of an AVL subtree at node in `at`.
    // New root of subtree is returned, caller is responsible for updating proper link from parent.
    fn rotate_right(&mut self, node: &mut Node<K>, id: FreeListIndex) -> FreeListIndex {
        let (rgt_id, mut rgt) = expect(node.right(&self.nodes).map(|(id, r)| (id, r.clone())));
        let rgt_lft = rgt.lft;

        // at.R = at.R.L
        node.rgt = rgt_lft;

        // at.R.L = at
        rgt.lft = Some(id);

        // at = at.R
        self.update_height(node, id);
        self.update_height(&mut rgt, id);

        rgt_id
    }

    // Check balance at a given node and enforce it if necessary with respective rotations.
    fn enforce_balance(&mut self, node: &mut Node<K>, id: FreeListIndex) -> FreeListIndex {
        let balance = self.get_balance(node);
        if balance > 1 {
            let (left_id, mut left) = expect(node.left(&self.nodes).map(|(id, n)| (id, n.clone())));
            if self.get_balance(&left) < 0 {
                let rotated = self.rotate_right(&mut left, left_id);
                node.lft = Some(rotated);
            }
            self.rotate_left(node, id)
        } else if balance < -1 {
            let (right_id, mut right) =
                expect(node.right(&self.nodes).map(|(id, r)| (id, r.clone())));
            if self.get_balance(&right) > 0 {
                let rotated = self.rotate_left(&mut right, right_id);
                node.rgt = Some(rotated);
            }
            self.rotate_right(node, id)
        } else {
            id
        }
    }

    // Returns (node, parent node) for a node that holds the `key`.
    // For root node, same node is returned for node and parent node.
    fn lookup_at<Q: ?Sized>(&self, mut at: FreeListIndex, key: &Q) -> Option<(&Node<K>, &Node<K>)>
    where
        K: Borrow<Q>,
        Q: BorshSerialize + Eq + PartialOrd,
    {
        let mut p: &Node<K> = self.node(at).unwrap();
        while let Some(node) = self.node(at) {
            let node_key: &Q = node.key.borrow();
            if node_key.eq(key) {
                return Some((node, p));
            } else if node_key.lt(key) {
                match node.rgt {
                    Some(rgt) => {
                        p = node;
                        at = rgt;
                    }
                    None => break,
                }
            } else {
                match node.lft {
                    Some(lft) => {
                        p = node;
                        at = lft;
                    }
                    None => break,
                }
            }
        }
        None
    }

    // Navigate from root to node holding `key` and backtrace back to the root
    // enforcing balance (if necessary) along the way.
    fn check_balance(&mut self, at: FreeListIndex, key: &K) -> FreeListIndex {
        match self.node(at).cloned() {
            Some(mut node) => {
                if !node.key.eq(key) {
                    if node.key.gt(key) {
                        if let Some(l) = node.lft {
                            let id = self.check_balance(l, key);
                            node.lft = Some(id);
                        }
                    } else if let Some(r) = node.rgt {
                        let id = self.check_balance(r, key);
                        node.rgt = Some(id);
                    }
                }
                self.update_height(&mut node, at);
                self.enforce_balance(&mut node, at)
            }
            None => at,
        }
    }

    // Node holding the key is not removed from the tree - instead the substitute node is found,
    // the key is copied to 'removed' node from substitute node, and then substitute node gets
    // removed from the tree.
    //
    // The substitute node is either:
    // - right-most (max) node of the left subtree (containing smaller keys) of node holding `key`
    // - or left-most (min) node of the right subtree (containing larger keys) of node holding `key`
    //
    fn do_remove<Q: ?Sized>(&mut self, key: &Q) -> Option<FreeListIndex>
    where
        K: Borrow<Q>,
        Q: BorshSerialize + Eq + PartialOrd,
    {
        // r_node - node containing key of interest
        // p_node - immediate parent node of r_node
        let (mut r_node, mut p_node) = match self.root.and_then(|root| self.lookup_at(root, key)) {
            Some((l, r)) => (l.clone(), r.clone()),
            None => return self.root, // cannot remove a missing key, no changes to the tree needed
        };

        let lft_opt = r_node.lft;
        let rgt_opt = r_node.rgt;

        if lft_opt.is_none() && rgt_opt.is_none() {
            todo!()
            // let p_node_key: &Q = p_node.key.borrow();
            // // remove leaf
            // if p_node_key.lt(key) {
            //     p_node.rgt = None;
            // } else {
            //     p_node.lft = None;
            // }
            // self.update_height(&mut p_node);

            // self.swap_with_last(r_node.id);

            // // removing node might have caused a imbalance - balance the tree up to the root,
            // // starting from lowest affected key - the parent of a leaf node in this case
            // self.check_balance(self.root, &p_node.key)
        } else {
            // non-leaf node, select subtree to proceed with
            let b = self.get_balance(&r_node);
            if b >= 0 {
                todo!()
                // // proceed with left subtree
                // let lft = lft_opt.unwrap();

                // // k - max key from left subtree
                // // n - node that holds key k, p - immediate parent of n
                // let (n, p) = self.max_at(lft, r_node.id).unwrap();
                // let (n, mut p) = (n.clone(), p.clone());
                // let k = n.key.clone();

                // if p.rgt.as_ref().map(|&id| id == n.id).unwrap_or_default() {
                //     // n is on right link of p
                //     p.rgt = n.lft;
                // } else {
                //     // n is on left link of p
                //     p.lft = n.lft;
                // }

                // self.update_height(&mut p);

                // if r_node.id == p.id {
                //     // r_node.id and p.id can overlap on small trees (2 levels, 2-3 nodes)
                //     // that leads to nasty lost update of the key, refresh below fixes that
                //     r_node = self.node(r_node.id).unwrap().clone();
                // }
                // r_node.key = k;
                // self.save(r_node.to_owned());

                // self.swap_with_last(n.id);

                // // removing node might have caused an imbalance - balance the tree up to the root,
                // // starting from the lowest affected key (max key from left subtree in this case)
                // self.check_balance(self.root, &p.key)
            } else {
                todo!()
                // // proceed with right subtree
                // let rgt = rgt_opt.unwrap();

                // // k - min key from right subtree
                // // n - node that holds key k, p - immediate parent of n
                // let (n, p) = self.min_at(rgt, r_node.id).unwrap();
                // let (n, mut p) = (n.clone(), p.clone());
                // let k = n.key.clone();

                // if p.lft.map(|id| id == n.id).unwrap_or_default() {
                //     // n is on left link of p
                //     p.lft = n.rgt;
                // } else {
                //     // n is on right link of p
                //     p.rgt = n.rgt;
                // }

                // self.update_height(&mut p);

                // if r_node.id == p.id {
                //     // r_node.id and p.id can overlap on small trees (2 levels, 2-3 nodes)
                //     // that leads to nasty lost update of the key, refresh below fixes that
                //     r_node = self.node(r_node.id).unwrap().clone();
                // }
                // r_node.key = k;
                // self.save(r_node.to_owned());

                // self.swap_with_last(n.id);

                // // removing node might have caused a imbalance - balance the tree up to the root,
                // // starting from the lowest affected key (min key from right subtree in this case)
                // self.check_balance(self.root, &p.key)
            }
        };
        todo!()
    }

    // // Move content of node with id = `len - 1` (parent left or right link, left, right, key, height)
    // // to node with given `id`, and remove node `len - 1` (pop the vector of nodes).
    // // This ensures that among `n` nodes in the tree, max `id` is `n-1`, so when new node is inserted,
    // // it gets an `id` as its position in the vector.
    // fn swap_with_last(&mut self, id: u32) {
    //     if id == self.len() - 1 {
    //         // noop: id is already last element in the vector
    //         self.tree.pop();
    //         return;
    //     }

    //     let (mut n, mut p) = {
    //         let key = self.node(self.len() - 1).map(|n| &n.key).unwrap();
    //         // TODO clone
    //         self.lookup_at(self.root, &key).map(|(n, p)| (n.clone(), p.clone())).unwrap()
    //     };

    //     if n.id != p.id {
    //         if p.lft.map(|id| id == n.id).unwrap_or_default() {
    //             p.lft = Some(id);
    //         } else {
    //             p.rgt = Some(id);
    //         }
    //         self.save(p);
    //     }

    //     if self.root == n.id {
    //         self.root = id;
    //     }

    //     n.id = id;
    //     self.save(n);
    //     self.tree.pop();
    // }
}

// impl<K, V, H> Iterator for Cursor<'_, K, V, H>
// where
//     K: Ord + Clone + BorshSerialize + BorshDeserialize,
//     V: BorshSerialize + BorshDeserialize,
//     H: CryptoHasher<Digest = [u8; 32]>,
// {
//     type Item = (K, V);

//     fn next(&mut self) -> Option<Self::Item> {
//         <Self as Iterator>::nth(self, 0)
//     }

//     fn size_hint(&self) -> (usize, Option<usize>) {
//         // Constrains max count. Not worth it to cause storage reads to make this more accurate.
//         (0, Some(self.map.len() as usize))
//     }

//     fn count(mut self) -> usize {
//         // Because this Cursor allows for bounded/starting from a key, there is no way of knowing
//         // how many elements are left to iterate without loading keys in order. This could be
//         // optimized in the case of a standard iterator by having a separate type, but this would
//         // be a breaking change, so there will be slightly more reads than necessary in this case.
//         let mut count = 0;
//         while self.key.is_some() {
//             count += 1;
//             self.progress_key();
//         }
//         count
//     }

//     fn nth(&mut self, n: usize) -> Option<Self::Item> {
//         for _ in 0..n {
//             // Skip over elements not iterated over to get to `nth`. This avoids loading values
//             // from storage.
//             self.progress_key();
//         }

//         let key = self.progress_key()?;
//         let value = self.map.get(&key)?;

//         Some((key, value))
//     }

//     fn last(mut self) -> Option<Self::Item> {
//         if self.asc && matches!(self.hi, Bound::Unbounded) {
//             self.map.max().and_then(|k| self.map.get(&k).map(|v| (k, v)))
//         } else if !self.asc && matches!(self.lo, Bound::Unbounded) {
//             self.map.min().and_then(|k| self.map.get(&k).map(|v| (k, v)))
//         } else {
//             // Cannot guarantee what the last is within the range, must load keys until last.
//             let key = core::iter::from_fn(|| self.progress_key()).last();
//             key.and_then(|k| self.map.get(&k).map(|v| (k, v)))
//         }
//     }
// }

// impl<K, V, H> std::iter::FusedIterator for Cursor<'_, K, V, H>
// where
//     K: Ord + Clone + BorshSerialize + BorshDeserialize,
//     V: BorshSerialize + BorshDeserialize,
//     H: CryptoHasher<Digest = [u8; 32]>,
// {
// }

// fn fits<K: Ord>(key: &K, lo: &Bound<K>, hi: &Bound<K>) -> bool {
//     (match lo {
//         Bound::Included(ref x) => key >= x,
//         Bound::Excluded(ref x) => key > x,
//         Bound::Unbounded => true,
//     }) && (match hi {
//         Bound::Included(ref x) => key <= x,
//         Bound::Excluded(ref x) => key < x,
//         Bound::Unbounded => true,
//     })
// }

// pub struct Cursor<'a, K, V, H>
// where
//     K: BorshSerialize + Ord,
//     V: BorshSerialize,
//     H: CryptoHasher<Digest = [u8; 32]>,
// {
//     asc: bool,
//     lo: Bound<K>,
//     hi: Bound<K>,
//     key: Option<K>,
//     map: &'a TreeMap<K, V, H>,
// }

// impl<'a, K, V, H> Cursor<'a, K, V, H>
// where
//     K: Ord + Clone + BorshSerialize + BorshDeserialize,
//     V: BorshSerialize + BorshDeserialize,
//     H: CryptoHasher<Digest = [u8; 32]>,
// {
//     fn asc(map: &'a TreeMap<K, V, H>) -> Self {
//         let key: Option<K> = map.min();
//         Self { asc: true, key, lo: Bound::Unbounded, hi: Bound::Unbounded, map }
//     }

//     fn asc_from(map: &'a TreeMap<K, V, H>, key: K) -> Self {
//         let key = map.higher(&key);
//         Self { asc: true, key, lo: Bound::Unbounded, hi: Bound::Unbounded, map }
//     }

//     fn desc(map: &'a TreeMap<K, V, H>) -> Self {
//         let key: Option<K> = map.max();
//         Self { asc: false, key, lo: Bound::Unbounded, hi: Bound::Unbounded, map }
//     }

//     fn desc_from(map: &'a TreeMap<K, V, H>, key: K) -> Self {
//         let key = map.lower(&key);
//         Self { asc: false, key, lo: Bound::Unbounded, hi: Bound::Unbounded, map }
//     }

//     fn range(map: &'a TreeMap<K, V, H>, lo: Bound<K>, hi: Bound<K>) -> Self {
//         let key = match &lo {
//             Bound::Included(k) if map.contains_key(k) => Some(k.clone()),
//             Bound::Included(k) | Bound::Excluded(k) => map.higher(k),
//             _ => None,
//         };
//         let key = key.filter(|k| fits(k, &lo, &hi));

//         Self { asc: true, key, lo, hi, map }
//     }

//     /// Progresses the key one index, will return the previous key
//     fn progress_key(&mut self) -> Option<K> {
//         let new_key = self
//             .key
//             .as_ref()
//             .and_then(|k| if self.asc { self.map.higher(k) } else { self.map.lower(k) })
//             .filter(|k| fits(k, &self.lo, &self.hi));
//         core::mem::replace(&mut self.key, new_key)
//     }
// }

impl<K, V, H> TreeMap<K, V, H>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    /// An iterator visiting all key-value pairs in arbitrary order.
    /// The iterator element type is `(&'a K, &'a V)`.
    pub fn iter(&self) -> Iter<K, V, H>
    where
        K: BorshDeserialize,
    {
        Iter::new(self)
    }

    /// An iterator visiting all key-value pairs in arbitrary order,
    /// with exclusive references to the values.
    /// The iterator element type is `(&'a K, &'a mut V)`.
    pub fn iter_mut(&mut self) -> IterMut<K, V, H>
    where
        K: BorshDeserialize,
    {
        IterMut::new(self)
    }

    // /// An iterator visiting all keys in arbitrary order.
    // /// The iterator element type is `&'a K`.
    // pub fn keys(&self) -> Keys<K>
    // where
    //     K: BorshDeserialize,
    // {
    //     Keys::new(self)
    // }

    /// An iterator visiting all values in arbitrary order.
    /// The iterator element type is `&'a V`.
    pub fn values(&self) -> Values<K, V, H>
    where
        K: BorshDeserialize,
    {
        Values::new(self)
    }

    /// A mutable iterator visiting all values in arbitrary order.
    /// The iterator element type is `&'a mut V`.
    pub fn values_mut(&mut self) -> ValuesMut<K, V, H>
    where
        K: BorshDeserialize,
    {
        ValuesMut::new(self)
    }
}

impl<K, V, H> TreeMap<K, V, H>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize + BorshDeserialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    /// Removes a key from the map, returning the stored key and value if the
    /// key was previously in the map.
    ///
    /// The key may be any borrowed form of the map's key type, but
    /// [`BorshSerialize`] and [`ToOwned<Owned = K>`](ToOwned) on the borrowed form *must* match
    /// those for the key type.
    ///
    /// # Examples
    ///
    /// ```
    /// use near_sdk::store::TreeMap;
    ///
    /// let mut map = TreeMap::new(b"m");
    /// map.insert(1, "a".to_string());
    /// assert_eq!(map.remove(&1), Some("a".to_string()));
    /// assert_eq!(map.remove(&1), None);
    /// ```
    pub fn remove_entry<Q: ?Sized>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q> + BorshDeserialize,
        Q: BorshSerialize + ToOwned<Owned = K>,
    {
        todo!()
        // // Remove value
        // let old_value = self.values.remove(k)?;

        // // Remove key with index if value exists
        // let key = self
        //     .keys
        //     .remove(old_value.key_index)
        //     .unwrap_or_else(|| env::panic_str(ERR_INCONSISTENT_STATE));

        // // Return removed value
        // Some((key, old_value.value))
    }

    // /// Gets the given key's corresponding entry in the map for in-place manipulation.
    // /// ```
    // /// use near_sdk::store::TreeMap;
    // ///
    // /// let mut count = TreeMap::new(b"m");
    // ///
    // /// for ch in [7, 2, 4, 7, 4, 1, 7] {
    // ///     let counter = count.entry(ch).or_insert(0);
    // ///     *counter += 1;
    // /// }
    // ///
    // /// assert_eq!(count[&4], 2);
    // /// assert_eq!(count[&7], 3);
    // /// assert_eq!(count[&1], 1);
    // /// assert_eq!(count.get(&8), None);
    // /// ```
    // pub fn entry(&mut self, key: K) -> Entry<K, V>
    // where
    //     K: Clone,
    // {
    //     Entry::new(self.values.entry(key), &mut self.keys)
    // }
}

impl<K, V, H> TreeMap<K, V, H>
where
    K: BorshSerialize + Ord,
    V: BorshSerialize,
    H: CryptoHasher<Digest = [u8; 32]>,
{
    /// Flushes the intermediate values of the map before this is called when the structure is
    /// [`Drop`]ed. This will write all modified values to storage but keep all cached values
    /// in memory.
    pub fn flush(&mut self) {
        self.values.flush();
        self.tree.nodes.flush();
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{next_trie_id, test_env};

    extern crate rand;
    use self::rand::RngCore;
    use quickcheck::QuickCheck;
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    use std::fmt::Formatter;
    use std::fmt::{Debug, Result};

    /// Return height of the tree - number of nodes on the longest path starting from the root node.
    fn height<K, V, H>(tree: &TreeMap<K, V, H>) -> u32
    where
        K: Ord + Clone + BorshSerialize + BorshDeserialize,
        V: BorshSerialize + BorshDeserialize,
        H: CryptoHasher<Digest = [u8; 32]>,
    {
        tree.tree.root.and_then(|root| tree.tree.node(root)).map(|n| n.ht).unwrap_or_default()
    }

    fn random(n: u32) -> Vec<u32> {
        let mut rng = rand::thread_rng();
        let mut vec = Vec::with_capacity(n as usize);
        (0..n).for_each(|_| {
            vec.push(rng.next_u32() % 1000);
        });
        vec
    }

    fn log2(x: f64) -> f64 {
        std::primitive::f64::log(x, 2.0f64)
    }

    fn max_tree_height(n: u32) -> u32 {
        // h <= C * log2(n + D) + B
        // where:
        // C =~ 1.440, D =~ 1.065, B =~ 0.328
        // (source: https://en.wikipedia.org/wiki/AVL_tree)
        const B: f64 = -0.328;
        const C: f64 = 1.440;
        const D: f64 = 1.065;

        let h = C * log2(n as f64 + D) + B;
        h.ceil() as u32
    }

    impl<K> Debug for Node<K>
    where
        K: Ord + Clone + Debug + BorshSerialize + BorshDeserialize,
    {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result {
            f.debug_struct("Node")
                // .field("id", &self.id)
                .field("key", &self.key)
                .field("lft", &self.lft)
                .field("rgt", &self.rgt)
                .field("ht", &self.ht)
                .finish()
        }
    }

    impl<K, V, H> Debug for TreeMap<K, V, H>
    where
        K: Ord + Clone + Debug + BorshSerialize + BorshDeserialize,
        V: Debug + BorshSerialize + BorshDeserialize,
        H: CryptoHasher<Digest = [u8; 32]>,
    {
        fn fmt(&self, f: &mut Formatter<'_>) -> Result {
            f.debug_struct("TreeMap")
                .field("root", &self.tree.root)
                .field("tree", &self.tree.nodes.iter().collect::<Vec<&Node<K>>>())
                .finish()
        }
    }

    #[test]
    fn test_empty() {
        let map: TreeMap<u8, u8> = TreeMap::new(b't');
        assert_eq!(map.len(), 0);
        assert_eq!(height(&map), 0);
        assert_eq!(map.get(&42), None);
        assert!(!map.contains_key(&42));
        assert_eq!(map.tree.min(), None);
        assert_eq!(map.tree.max(), None);
        assert_eq!(map.tree.lower(&42), None);
        assert_eq!(map.tree.higher(&42), None);
    }

    #[test]
    fn test_insert_3_rotate_l_l() {
        let mut map: TreeMap<i32, i32> = TreeMap::new(next_trie_id());
        assert_eq!(height(&map), 0);

        map.insert(3, 3);
        assert_eq!(height(&map), 1);

        map.insert(2, 2);
        assert_eq!(height(&map), 2);

        map.insert(1, 1);
        assert_eq!(height(&map), 2);

        let root = map.tree.root.unwrap();
        assert_eq!(root, FreeListIndex(1));
        assert_eq!(map.tree.node(root).map(|n| n.key), Some(2));

        map.clear();
    }

    #[test]
    fn test_insert_3_rotate_r_r() {
        let mut map: TreeMap<i32, i32> = TreeMap::new(next_trie_id());
        assert_eq!(height(&map), 0);

        map.insert(1, 1);
        assert_eq!(height(&map), 1);

        map.insert(2, 2);
        assert_eq!(height(&map), 2);

        map.insert(3, 3);

        let root = map.tree.root.unwrap();
        assert_eq!(root, FreeListIndex(1));
        assert_eq!(map.tree.node(root).map(|n| n.key), Some(2));
        assert_eq!(height(&map), 2);

        map.clear();
    }

    #[test]
    fn test_insert_lookup_n_asc() {
        let mut map: TreeMap<i32, i32> = TreeMap::new(next_trie_id());

        let n: u32 = 30;
        let cases = (0..2 * (n as i32)).collect::<Vec<i32>>();

        let mut counter = 0;
        for k in cases.iter().copied() {
            if k % 2 == 0 {
                counter += 1;
                map.insert(k, counter);
            }
        }

        counter = 0;
        for k in &cases {
            if *k % 2 == 0 {
                counter += 1;
                assert_eq!(map.get(k), Some(&counter));
            } else {
                assert_eq!(map.get(k), None);
            }
        }

        assert!(height(&map) <= max_tree_height(n));
        map.clear();
    }

    #[test]
    pub fn test_insert_one() {
        let mut map = TreeMap::new(b"m");
        assert_eq!(None, map.insert(1, 2));
        assert_eq!(2, map.insert(1, 3).unwrap());
    }

    #[test]
    fn test_insert_lookup_n_desc() {
        let mut map: TreeMap<i32, i32> = TreeMap::new(next_trie_id());

        let n: u32 = 30;
        let cases = (0..2 * (n as i32)).rev().collect::<Vec<i32>>();

        let mut counter = 0;
        for k in cases.iter().copied() {
            if k % 2 == 0 {
                counter += 1;
                map.insert(k, counter);
            }
        }

        counter = 0;
        for k in &cases {
            if *k % 2 == 0 {
                counter += 1;
                assert_eq!(map.get(k), Some(&counter));
            } else {
                assert_eq!(map.get(k), None);
            }
        }

        assert!(height(&map) <= max_tree_height(n));
        map.clear();
    }

    #[test]
    fn insert_n_random() {
        test_env::setup_free();

        for k in 1..10 {
            // tree size is 2^k
            let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

            let n = 1 << k;
            let input: Vec<u32> = random(n);

            for x in input.iter().copied() {
                map.insert(x, 42);
            }

            for x in &input {
                assert_eq!(map.get(x), Some(&42));
            }

            assert!(height(&map) <= max_tree_height(n));
            map.clear();
        }
    }

    // #[test]
    // fn test_min() {
    //     let n: u32 = 30;
    //     let vec = random(n);

    //     let mut map: TreeMap<u32, u32> = TreeMap::new(b't');
    //     for x in vec.iter().rev().copied() {
    //         map.insert(x, 1);
    //     }

    //     assert_eq!(map.min().unwrap(), *vec.iter().min().unwrap());
    //     map.clear();
    // }

    #[test]
    fn test_max() {
        let n: u32 = 30;
        let vec = random(n);

        let mut map: TreeMap<u32, u32> = TreeMap::new(b't');
        for x in vec.iter().rev().copied() {
            map.insert(x, 1);
        }

        assert_eq!(map.tree.max().unwrap(), vec.iter().max().unwrap());
        map.clear();
    }

    #[test]
    fn test_lower() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        let vec: Vec<u32> = vec![10, 20, 30, 40, 50];

        for x in vec.into_iter() {
            map.insert(x, 1);
        }

        assert_eq!(map.tree.lower(&5), None);
        assert_eq!(map.tree.lower(&10), None);
        assert_eq!(map.tree.lower(&11), Some(&10));
        assert_eq!(map.tree.lower(&20), Some(&10));
        assert_eq!(map.tree.lower(&49), Some(&40));
        assert_eq!(map.tree.lower(&50), Some(&40));
        assert_eq!(map.tree.lower(&51), Some(&50));

        map.clear();
    }

    #[test]
    fn test_higher() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        let vec: Vec<u32> = vec![10, 20, 30, 40, 50];

        for x in vec.into_iter() {
            map.insert(x, 1);
        }

        assert_eq!(map.tree.higher(&5), Some(&10));
        assert_eq!(map.tree.higher(&10), Some(&20));
        assert_eq!(map.tree.higher(&11), Some(&20));
        assert_eq!(map.tree.higher(&20), Some(&30));
        assert_eq!(map.tree.higher(&49), Some(&50));
        assert_eq!(map.tree.higher(&50), None);
        assert_eq!(map.tree.higher(&51), None);

        map.clear();
    }

    #[test]
    fn test_floor_key() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        let vec: Vec<u32> = vec![10, 20, 30, 40, 50];

        for x in vec.into_iter() {
            map.insert(x, 1);
        }

        assert_eq!(map.floor_key(&5), None);
        assert_eq!(map.floor_key(&10), Some(&10));
        assert_eq!(map.floor_key(&11), Some(&10));
        assert_eq!(map.floor_key(&20), Some(&20));
        assert_eq!(map.floor_key(&49), Some(&40));
        assert_eq!(map.floor_key(&50), Some(&50));
        assert_eq!(map.floor_key(&51), Some(&50));

        map.clear();
    }

    #[test]
    fn test_ceil_key() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        let vec: Vec<u32> = vec![10, 20, 30, 40, 50];

        for x in vec.into_iter() {
            map.insert(x, 1);
        }

        assert_eq!(map.ceil_key(&5), Some(&10));
        assert_eq!(map.ceil_key(&10), Some(&10));
        assert_eq!(map.ceil_key(&11), Some(&20));
        assert_eq!(map.ceil_key(&20), Some(&20));
        assert_eq!(map.ceil_key(&49), Some(&50));
        assert_eq!(map.ceil_key(&50), Some(&50));
        assert_eq!(map.ceil_key(&51), None);

        map.clear();
    }

    #[test]
    fn test_remove_1() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        map.insert(1, 1);
        assert_eq!(map.get(&1), Some(&1));
        map.remove(&1);
        assert_eq!(map.get(&1), None);
        assert_eq!(map.tree.nodes.len(), 0);
        map.clear();
    }

    #[test]
    fn test_remove_3() {
        let map: TreeMap<u32, u32> = avl(&[(0, 0)], &[0, 0, 1]);

        assert_eq!(map.iter().collect::<Vec<(&u32, &u32)>>(), vec![]);
    }

    #[test]
    fn test_remove_3_desc() {
        let vec: Vec<u32> = vec![3, 2, 1];
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }
        map.clear();
    }

    #[test]
    fn test_remove_3_asc() {
        let vec: Vec<u32> = vec![1, 2, 3];
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }
        map.clear();
    }

    #[test]
    fn test_remove_7_regression_1() {
        let vec: Vec<u32> =
            vec![2104297040, 552624607, 4269683389, 3382615941, 155419892, 4102023417, 1795725075];
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }
        map.clear();
    }

    #[test]
    fn test_remove_7_regression_2() {
        let vec: Vec<u32> =
            vec![700623085, 87488544, 1500140781, 1111706290, 3187278102, 4042663151, 3731533080];
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }
        map.clear();
    }

    #[test]
    fn test_remove_9_regression() {
        let vec: Vec<u32> = vec![
            1186903464, 506371929, 1738679820, 1883936615, 1815331350, 1512669683, 3581743264,
            1396738166, 1902061760,
        ];
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }
        map.clear();
    }

    #[test]
    fn test_remove_20_regression_1() {
        let vec: Vec<u32> = vec![
            552517392, 3638992158, 1015727752, 2500937532, 638716734, 586360620, 2476692174,
            1425948996, 3608478547, 757735878, 2709959928, 2092169539, 3620770200, 783020918,
            1986928932, 200210441, 1972255302, 533239929, 497054557, 2137924638,
        ];
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }
        map.clear();
    }

    #[test]
    fn test_remove_7_regression() {
        let vec: Vec<u32> = vec![280, 606, 163, 857, 436, 508, 44, 801];

        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in &vec {
            assert_eq!(map.get(x), None);
            map.insert(*x, 1);
            assert_eq!(map.get(x), Some(&1));
        }

        for x in &vec {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }

        assert_eq!(map.len(), 0, "map.len() > 0");
        assert_eq!(map.tree.nodes.len(), 0, "map.tree is not empty");
        map.clear();
    }

    #[test]
    fn test_insert_8_remove_4_regression() {
        let insert = vec![882, 398, 161, 76];
        let remove = vec![242, 687, 860, 811];

        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for (i, (k1, k2)) in insert.iter().zip(remove.iter()).enumerate() {
            let v = i as u32;
            map.insert(*k1, v);
            map.insert(*k2, v);
        }

        for k in remove.iter() {
            map.remove(k);
        }

        assert_eq!(map.len(), insert.len() as u32);

        for (i, k) in (0..).zip(insert.iter()) {
            assert_eq!(map.get(k), Some(&i));
        }
    }

    #[test]
    fn test_remove_n() {
        let n: u32 = 20;
        let vec = random(n);

        let mut set: HashSet<u32> = HashSet::new();
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        for x in &vec {
            map.insert(*x, 1);
            set.insert(*x);
        }

        assert_eq!(map.len(), set.len() as u32);

        for x in &set {
            assert_eq!(map.get(x), Some(&1));
            map.remove(x);
            assert_eq!(map.get(x), None);
        }

        assert_eq!(map.len(), 0, "map.len() > 0");
        assert_eq!(map.tree.nodes.len(), 0, "map.tree is not empty");
        map.clear();
    }

    #[test]
    fn test_remove_root_3() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        map.insert(2, 1);
        map.insert(3, 1);
        map.insert(1, 1);
        map.insert(4, 1);

        map.remove(&2);

        assert_eq!(map.get(&1), Some(&1));
        assert_eq!(map.get(&2), None);
        assert_eq!(map.get(&3), Some(&1));
        assert_eq!(map.get(&4), Some(&1));
        map.clear();
    }

    #[test]
    fn test_insert_2_remove_2_regression() {
        let ins: Vec<u32> = vec![11760225, 611327897];
        let rem: Vec<u32> = vec![2982517385, 1833990072];

        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        map.insert(ins[0], 1);
        map.insert(ins[1], 1);

        map.remove(&rem[0]);
        map.remove(&rem[1]);

        let h = height(&map);
        let h_max = max_tree_height(map.len());
        assert!(h <= h_max, "h={} h_max={}", h, h_max);
        map.clear();
    }

    #[test]
    fn test_insert_n_duplicates() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

        for x in 0..30 {
            map.insert(x, x);
            map.insert(42, x);
        }

        assert_eq!(map.get(&42), Some(&29));
        assert_eq!(map.len(), 31);
        assert_eq!(map.tree.nodes.len(), 31);

        map.clear();
    }

    #[test]
    fn test_insert_2n_remove_n_random() {
        for k in 1..4 {
            let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
            let mut set: HashSet<u32> = HashSet::new();

            let n = 1 << k;
            let ins: Vec<u32> = random(n);
            let rem: Vec<u32> = random(n);

            for x in &ins {
                set.insert(*x);
                map.insert(*x, 42);
            }

            for x in &rem {
                set.insert(*x);
                map.insert(*x, 42);
            }

            for x in &rem {
                set.remove(x);
                map.remove(x);
            }

            assert_eq!(map.len(), set.len() as u32);

            let h = height(&map);
            let h_max = max_tree_height(n);
            assert!(h <= h_max, "[n={}] tree is too high: {} (max is {}).", n, h, h_max);

            map.clear();
        }
    }

    #[test]
    fn test_remove_empty() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        assert_eq!(map.remove(&1), None);
    }

    #[test]
    fn test_iter() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        map.insert(1, 41);
        map.insert(2, 42);
        map.insert(3, 43);

        assert_eq!(map.iter().collect::<Vec<_>>(), vec![(&1, &41), (&2, &42), (&3, &43)]);

        // Test custom iterator impls
        assert_eq!(map.iter().nth(1), Some((&2, &42)));
        assert_eq!(map.iter().count(), 3);
        assert_eq!(map.iter().last(), Some((&3, &43)));
        map.clear();
    }

    #[test]
    fn test_iter_empty() {
        let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        assert_eq!(map.iter().count(), 0);
    }

    #[test]
    fn test_iter_rev() {
        let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        map.insert(1, 41);
        map.insert(2, 42);
        map.insert(3, 43);

        assert_eq!(
            map.iter().rev().collect::<Vec<(&u32, &u32)>>(),
            vec![(&3, &43), (&2, &42), (&1, &41)]
        );

        // Test custom iterator impls
        assert_eq!(map.iter().rev().nth(1), Some((&2, &42)));
        assert_eq!(map.iter().rev().count(), 3);
        assert_eq!(map.iter().rev().last(), Some((&1, &41)));
        map.clear();
    }

    #[test]
    fn test_iter_rev_empty() {
        let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
        assert_eq!(map.iter().rev().count(), 0);
    }

    // #[test]
    // fn test_iter_from() {
    //     let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

    //     let one: Vec<u32> = vec![10, 20, 30, 40, 50];
    //     let two: Vec<u32> = vec![45, 35, 25, 15, 5];

    //     for x in &one {
    //         map.insert(*x, 42);
    //     }

    //     for x in &two {
    //         map.insert(*x, 42);
    //     }

    //     assert_eq!(
    //         map.iter_from(29).collect::<Vec<(u32, u32)>>(),
    //         vec![(30, 42), (35, 42), (40, 42), (45, 42), (50, 42)]
    //     );

    //     assert_eq!(
    //         map.iter_from(30).collect::<Vec<(u32, u32)>>(),
    //         vec![(35, 42), (40, 42), (45, 42), (50, 42)]
    //     );

    //     assert_eq!(
    //         map.iter_from(31).collect::<Vec<(u32, u32)>>(),
    //         vec![(35, 42), (40, 42), (45, 42), (50, 42)]
    //     );

    //     // Test custom iterator impls
    //     assert_eq!(map.iter_from(31).nth(2), Some((45, 42)));
    //     assert_eq!(map.iter_from(31).count(), 4);
    //     assert_eq!(map.iter_from(31).last(), Some((50, 42)));

    //     map.clear();
    // }

    // #[test]
    // fn test_iter_from_empty() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     assert_eq!(map.iter_from(42).count(), 0);
    // }

    // #[test]
    // fn test_iter_rev_from() {
    //     let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

    //     let one: Vec<u32> = vec![10, 20, 30, 40, 50];
    //     let two: Vec<u32> = vec![45, 35, 25, 15, 5];

    //     for x in &one {
    //         map.insert(x, &42);
    //     }

    //     for x in &two {
    //         map.insert(x, &42);
    //     }

    //     assert_eq!(
    //         map.iter_rev_from(29).collect::<Vec<(u32, u32)>>(),
    //         vec![(25, 42), (20, 42), (15, 42), (10, 42), (5, 42)]
    //     );

    //     assert_eq!(
    //         map.iter_rev_from(30).collect::<Vec<(u32, u32)>>(),
    //         vec![(25, 42), (20, 42), (15, 42), (10, 42), (5, 42)]
    //     );

    //     assert_eq!(
    //         map.iter_rev_from(31).collect::<Vec<(u32, u32)>>(),
    //         vec![(30, 42), (25, 42), (20, 42), (15, 42), (10, 42), (5, 42)]
    //     );

    //     // Test custom iterator impls
    //     assert_eq!(map.iter_rev_from(31).nth(2), Some((20, 42)));
    //     assert_eq!(map.iter_rev_from(31).count(), 6);
    //     assert_eq!(map.iter_rev_from(31).last(), Some((5, 42)));

    //     map.clear();
    // }

    // #[test]
    // fn test_range() {
    //     let mut map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());

    //     let one: Vec<u32> = vec![10, 20, 30, 40, 50];
    //     let two: Vec<u32> = vec![45, 35, 25, 15, 5];

    //     for x in &one {
    //         map.insert(x, &42);
    //     }

    //     for x in &two {
    //         map.insert(x, &42);
    //     }

    //     assert_eq!(
    //         map.range((Bound::Included(20), Bound::Excluded(30))).collect::<Vec<(u32, u32)>>(),
    //         vec![(20, 42), (25, 42)]
    //     );

    //     assert_eq!(
    //         map.range((Bound::Excluded(10), Bound::Included(40))).collect::<Vec<(u32, u32)>>(),
    //         vec![(15, 42), (20, 42), (25, 42), (30, 42), (35, 42), (40, 42)]
    //     );

    //     assert_eq!(
    //         map.range((Bound::Included(20), Bound::Included(40))).collect::<Vec<(u32, u32)>>(),
    //         vec![(20, 42), (25, 42), (30, 42), (35, 42), (40, 42)]
    //     );

    //     assert_eq!(
    //         map.range((Bound::Excluded(20), Bound::Excluded(45))).collect::<Vec<(u32, u32)>>(),
    //         vec![(25, 42), (30, 42), (35, 42), (40, 42)]
    //     );

    //     assert_eq!(
    //         map.range((Bound::Excluded(25), Bound::Excluded(30))).collect::<Vec<(u32, u32)>>(),
    //         vec![]
    //     );

    //     assert_eq!(
    //         map.range((Bound::Included(25), Bound::Included(25))).collect::<Vec<(u32, u32)>>(),
    //         vec![(25, 42)]
    //     );

    //     assert_eq!(
    //         map.range((Bound::Excluded(25), Bound::Included(25))).collect::<Vec<(u32, u32)>>(),
    //         vec![]
    //     ); // the range makes no sense, but `BTreeMap` does not panic in this case

    //     // Test custom iterator impls
    //     assert_eq!(map.range((Bound::Excluded(20), Bound::Excluded(45))).nth(2), Some((35, 42)));
    //     assert_eq!(map.range((Bound::Excluded(20), Bound::Excluded(45))).count(), 4);
    //     assert_eq!(map.range((Bound::Excluded(20), Bound::Excluded(45))).last(), Some((40, 42)));

    //     map.clear();
    // }

    // #[test]
    // #[should_panic(expected = "Invalid range.")]
    // fn test_range_panics_same_excluded() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     let _ = map.range((Bound::Excluded(1), Bound::Excluded(1)));
    // }

    // #[test]
    // #[should_panic(expected = "Invalid range.")]
    // fn test_range_panics_non_overlap_incl_exlc() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     let _ = map.range((Bound::Included(2), Bound::Excluded(1)));
    // }

    // #[test]
    // #[should_panic(expected = "Invalid range.")]
    // fn test_range_panics_non_overlap_excl_incl() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     let _ = map.range((Bound::Excluded(2), Bound::Included(1)));
    // }

    // #[test]
    // #[should_panic(expected = "Invalid range.")]
    // fn test_range_panics_non_overlap_incl_incl() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     let _ = map.range((Bound::Included(2), Bound::Included(1)));
    // }

    // #[test]
    // #[should_panic(expected = "Invalid range.")]
    // fn test_range_panics_non_overlap_excl_excl() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     let _ = map.range((Bound::Excluded(2), Bound::Excluded(1)));
    // }

    // #[test]
    // fn test_iter_rev_from_empty() {
    //     let map: TreeMap<u32, u32> = TreeMap::new(next_trie_id());
    //     assert_eq!(map.iter_rev_from(42).count(), 0);
    // }

    #[test]
    fn test_balance_regression_1() {
        let insert = vec![(2, 0), (3, 0), (4, 0)];
        let remove = vec![0, 0, 0, 1];

        let map = avl(&insert, &remove);
        assert!(is_balanced(&map, map.tree.root.unwrap()));
    }

    #[test]
    fn test_balance_regression_2() {
        let insert = vec![(1, 0), (2, 0), (0, 0), (3, 0), (5, 0), (6, 0)];
        let remove = vec![0, 0, 0, 3, 5, 6, 7, 4];

        let map = avl(&insert, &remove);
        assert!(is_balanced(&map, map.tree.root.unwrap()));
    }

    //
    // Property-based tests of AVL-based TreeMap against std::collections::BTreeMap
    //

    fn avl<K, V>(insert: &[(K, V)], remove: &[K]) -> TreeMap<K, V, Sha256>
    where
        K: Ord + Clone + BorshSerialize + BorshDeserialize,
        V: Default + BorshSerialize + BorshDeserialize + Clone,
    {
        test_env::setup_free();
        let mut map: TreeMap<K, V, _> = TreeMap::new(next_trie_id());
        for k in remove {
            map.insert(k.clone(), Default::default());
        }
        let n = insert.len().max(remove.len());
        for i in 0..n {
            if i < remove.len() {
                map.remove(&remove[i]);
            }
            if i < insert.len() {
                let (k, v) = &insert[i];
                map.insert(k.clone(), v.clone());
            }
        }
        map
    }

    fn rb<K, V>(insert: &[(K, V)], remove: &[K]) -> BTreeMap<K, V>
    where
        K: Ord + Clone + BorshSerialize + BorshDeserialize,
        V: Clone + Default + BorshSerialize + BorshDeserialize,
    {
        let mut map: BTreeMap<K, V> = BTreeMap::default();
        for k in remove {
            map.insert(k.clone(), Default::default());
        }
        let n = insert.len().max(remove.len());
        for i in 0..n {
            if i < remove.len() {
                map.remove(&remove[i]);
            }
            if i < insert.len() {
                let (k, v) = &insert[i];
                map.insert(k.clone(), v.clone());
            }
        }
        map
    }

    #[test]
    fn prop_avl_vs_rb() {
        fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>) -> bool {
            let a = avl(&insert, &remove);
            let b = rb(&insert, &remove);
            let v1: Vec<(&u32, &u32)> = a.iter().collect();
            let v2: Vec<(&u32, &u32)> = b.iter().collect();
            v1 == v2
        }

        QuickCheck::new()
            .tests(300)
            .quickcheck(prop as fn(std::vec::Vec<(u32, u32)>, std::vec::Vec<u32>) -> bool);
    }

    fn is_balanced<K, V, H>(map: &TreeMap<K, V, H>, root: FreeListIndex) -> bool
    where
        K: Debug + Ord + Clone + BorshSerialize + BorshDeserialize,
        V: Debug + BorshSerialize + BorshDeserialize,
        H: CryptoHasher<Digest = [u8; 32]>,
    {
        let node = map.tree.node(root).unwrap();
        let balance = map.tree.get_balance(&node);

        (balance >= -1 && balance <= 1)
            && node.lft.map(|id| is_balanced(map, id)).unwrap_or(true)
            && node.rgt.map(|id| is_balanced(map, id)).unwrap_or(true)
    }

    #[test]
    fn prop_avl_balance() {
        test_env::setup_free();

        fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>) -> bool {
            let map = avl(&insert, &remove);
            map.is_empty() || is_balanced(&map, map.tree.root.unwrap())
        }

        QuickCheck::new()
            .tests(300)
            .quickcheck(prop as fn(std::vec::Vec<(u32, u32)>, std::vec::Vec<u32>) -> bool);
    }

    #[test]
    fn prop_avl_height() {
        test_env::setup_free();

        fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>) -> bool {
            let map = avl(&insert, &remove);
            height(&map) <= max_tree_height(map.len())
        }

        QuickCheck::new()
            .tests(300)
            .quickcheck(prop as fn(std::vec::Vec<(u32, u32)>, std::vec::Vec<u32>) -> bool);
    }

    // fn range_prop(
    //     insert: Vec<(u32, u32)>,
    //     remove: Vec<u32>,
    //     range: (Bound<u32>, Bound<u32>),
    // ) -> bool {
    //     let a = avl(&insert, &remove);
    //     let b = rb(&insert, &remove);
    //     let v1: Vec<(u32, u32)> = a.range(range).collect();
    //     let v2: Vec<(u32, u32)> = b.range(range).map(|(k, v)| (*k, *v)).collect();
    //     v1 == v2
    // }

    // type Prop = fn(std::vec::Vec<(u32, u32)>, std::vec::Vec<u32>, u32, u32) -> bool;

    // #[test]
    // fn prop_avl_vs_rb_range_incl_incl() {
    //     fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>, r1: u32, r2: u32) -> bool {
    //         let range = (Bound::Included(r1.min(r2)), Bound::Included(r1.max(r2)));
    //         range_prop(insert, remove, range)
    //     }

    //     QuickCheck::new().tests(300).quickcheck(prop as Prop);
    // }

    // #[test]
    // fn prop_avl_vs_rb_range_incl_excl() {
    //     fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>, r1: u32, r2: u32) -> bool {
    //         let range = (Bound::Included(r1.min(r2)), Bound::Excluded(r1.max(r2)));
    //         range_prop(insert, remove, range)
    //     }

    //     QuickCheck::new().tests(300).quickcheck(prop as Prop);
    // }

    // #[test]
    // fn prop_avl_vs_rb_range_excl_incl() {
    //     fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>, r1: u32, r2: u32) -> bool {
    //         let range = (Bound::Excluded(r1.min(r2)), Bound::Included(r1.max(r2)));
    //         range_prop(insert, remove, range)
    //     }

    //     QuickCheck::new().tests(300).quickcheck(prop as Prop);
    // }

    // #[test]
    // fn prop_avl_vs_rb_range_excl_excl() {
    //     fn prop(insert: Vec<(u32, u32)>, remove: Vec<u32>, r1: u32, r2: u32) -> bool {
    //         // (Excluded(x), Excluded(x)) is invalid range, checking against it makes no sense
    //         r1 == r2 || {
    //             let range = (Bound::Excluded(r1.min(r2)), Bound::Excluded(r1.max(r2)));
    //             range_prop(insert, remove, range)
    //         }
    //     }

    //     QuickCheck::new().tests(300).quickcheck(prop as Prop);
    // }
}

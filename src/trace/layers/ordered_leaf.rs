//! Implementation using ordered keys and exponential search.

use crate::{
    algebra::{AddAssignByRef, AddByRef, HasZero, NegByRef},
    trace::{
        consolidation::consolidate_slice,
        layers::{advance, Builder, Cursor, MergeBuilder, Trie, TrieSlice, TupleBuilder},
    },
    NumEntries, SharedRef,
};
use deepsize::DeepSizeOf;
use std::{
    cmp::{min, Ordering},
    fmt::{Display, Formatter},
    ops::{Add, AddAssign, Neg},
};

/// A layer of unordered values.
#[derive(Debug, DeepSizeOf, Eq, PartialEq, Clone)]
pub struct OrderedLeaf<K, R> {
    /// Unordered values.
    pub vals: Vec<(K, R)>,
}

impl<K: Ord + Clone, R: Eq + HasZero + AddAssignByRef + Clone> Trie for OrderedLeaf<K, R> {
    type Item = (K, R);
    type Cursor = OrderedLeafCursor;
    type MergeBuilder = OrderedLeafBuilder<K, R>;
    type TupleBuilder = UnorderedLeafBuilder<K, R>;
    fn keys(&self) -> usize {
        self.vals.len()
    }
    fn tuples(&self) -> usize {
        <OrderedLeaf<K, R> as Trie>::keys(self)
    }
    fn cursor_from(&self, lower: usize, upper: usize) -> Self::Cursor {
        OrderedLeafCursor {
            bounds: (lower, upper),
            pos: lower,
        }
    }
}

impl<K, R> Display for OrderedLeaf<K, R>
where
    K: Ord + Clone + Display,
    R: Eq + HasZero + AddAssignByRef + Clone + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        TrieSlice(self, self.cursor()).fmt(f)
    }
}

impl<'a, K, R> Display for TrieSlice<'a, OrderedLeaf<K, R>>
where
    K: Ord + Clone + Display,
    R: Eq + HasZero + AddAssignByRef + Clone + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let TrieSlice(storage, cursor) = self;
        let mut cursor: OrderedLeafCursor = cursor.clone();

        while cursor.valid(storage) {
            let (key, val) = cursor.key(storage);
            writeln!(f, "{} -> {}", key, val)?;
            cursor.step(storage);
        }

        Ok(())
    }
}

// TODO: by-value merge
impl<K, R> Add<Self> for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: Eq + HasZero + AddAssignByRef + Clone,
{
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        if self.is_empty() {
            rhs
        } else if rhs.is_empty() {
            self
        } else {
            self.merge(&rhs)
        }
    }
}

impl<K, R> AddAssign<Self> for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: Eq + HasZero + AddAssignByRef + Clone,
{
    fn add_assign(&mut self, rhs: Self) {
        if !rhs.is_empty() {
            *self = self.merge(&rhs);
        }
    }
}

impl<K, R> AddAssignByRef for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: Eq + HasZero + AddAssignByRef + Clone,
{
    fn add_assign_by_ref(&mut self, other: &Self) {
        if !other.is_empty() {
            *self = self.merge(other);
        }
    }
}

impl<K, R> AddByRef for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: Eq + HasZero + AddAssignByRef + Clone,
{
    fn add_by_ref(&self, rhs: &Self) -> Self {
        self.merge(rhs)
    }
}

impl<K, R> NegByRef for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: NegByRef,
{
    fn neg_by_ref(&self) -> Self {
        Self {
            vals: self
                .vals
                .iter()
                .map(|(k, v)| (k.clone(), v.neg_by_ref()))
                .collect(),
        }
    }
}

impl<K, R> Neg for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: Neg<Output = R>,
{
    type Output = Self;

    fn neg(self) -> Self {
        Self {
            vals: self.vals.into_iter().map(|(k, v)| (k, v.neg())).collect(),
        }
    }
}

impl<K, R> NumEntries for OrderedLeaf<K, R>
where
    K: Ord + Clone,
    R: Eq + HasZero + AddAssignByRef + Clone,
{
    fn num_entries_shallow(&self) -> usize {
        self.keys()
    }

    fn num_entries_deep(&self) -> usize {
        self.keys()
    }

    const CONST_NUM_ENTRIES: Option<usize> = None;
}

impl<K, R> SharedRef for OrderedLeaf<K, R>
where
    K: Clone,
    R: Clone,
{
    type Target = Self;

    fn try_into_owned(self) -> Result<Self::Target, Self> {
        Ok(self)
    }
}

/// A builder for unordered values.
pub struct OrderedLeafBuilder<K, R> {
    /// Unordered values.
    pub vals: Vec<(K, R)>,
}

impl<K: Ord + Clone, R: Eq + HasZero + AddAssignByRef + Clone> Builder
    for OrderedLeafBuilder<K, R>
{
    type Trie = OrderedLeaf<K, R>;
    fn boundary(&mut self) -> usize {
        self.vals.len()
    }
    fn done(self) -> Self::Trie {
        OrderedLeaf { vals: self.vals }
    }
}

impl<K: Ord + Clone, R: Eq + HasZero + AddAssignByRef + Clone> MergeBuilder
    for OrderedLeafBuilder<K, R>
{
    fn with_capacity(other1: &Self::Trie, other2: &Self::Trie) -> Self {
        OrderedLeafBuilder {
            vals: Vec::with_capacity(
                <OrderedLeaf<K, R> as Trie>::keys(other1)
                    + <OrderedLeaf<K, R> as Trie>::keys(other2),
            ),
        }
    }
    fn with_key_capacity(cap: usize) -> Self {
        OrderedLeafBuilder {
            vals: Vec::with_capacity(cap),
        }
    }
    #[inline]
    fn copy_range(&mut self, other: &Self::Trie, lower: usize, upper: usize) {
        self.vals.extend_from_slice(&other.vals[lower..upper]);
    }
    fn push_merge(
        &mut self,
        other1: (&Self::Trie, <Self::Trie as Trie>::Cursor),
        other2: (&Self::Trie, <Self::Trie as Trie>::Cursor),
    ) -> usize {
        let (trie1, cursor1) = other1;
        let (trie2, cursor2) = other2;
        let mut lower1 = cursor1.bounds.0;
        let upper1 = cursor1.bounds.1;
        let mut lower2 = cursor2.bounds.0;
        let upper2 = cursor2.bounds.1;

        self.vals.reserve((upper1 - lower1) + (upper2 - lower2));

        // while both mergees are still active
        while lower1 < upper1 && lower2 < upper2 {
            match trie1.vals[lower1].0.cmp(&trie2.vals[lower2].0) {
                Ordering::Less => {
                    // determine how far we can advance lower1 until we reach/pass lower2
                    let step = 1 + advance(&trie1.vals[(1 + lower1)..upper1], |x| {
                        x.0 < trie2.vals[lower2].0
                    });
                    let step = min(step, 1000);
                    <OrderedLeafBuilder<K, R> as MergeBuilder>::copy_range(
                        self,
                        trie1,
                        lower1,
                        lower1 + step,
                    );
                    lower1 += step;
                }
                Ordering::Equal => {
                    let mut sum = trie1.vals[lower1].1.clone();
                    sum.add_assign_by_ref(&trie2.vals[lower2].1);
                    if !sum.is_zero() {
                        self.vals.push((trie1.vals[lower1].0.clone(), sum));
                    }

                    lower1 += 1;
                    lower2 += 1;
                }
                Ordering::Greater => {
                    // determine how far we can advance lower2 until we reach/pass lower1
                    let step = 1 + advance(&trie2.vals[(1 + lower2)..upper2], |x| {
                        x.0 < trie1.vals[lower1].0
                    });
                    let step = min(step, 1000);
                    <OrderedLeafBuilder<K, R> as MergeBuilder>::copy_range(
                        self,
                        trie2,
                        lower2,
                        lower2 + step,
                    );
                    lower2 += step;
                }
            }
        }

        if lower1 < upper1 {
            <OrderedLeafBuilder<K, R> as MergeBuilder>::copy_range(self, trie1, lower1, upper1);
        }
        if lower2 < upper2 {
            <OrderedLeafBuilder<K, R> as MergeBuilder>::copy_range(self, trie2, lower2, upper2);
        }

        self.vals.len()
    }
}

impl<K: Ord + Clone, R: Eq + HasZero + AddAssignByRef + Clone> TupleBuilder
    for OrderedLeafBuilder<K, R>
{
    type Item = (K, R);
    fn new() -> Self {
        OrderedLeafBuilder { vals: Vec::new() }
    }
    fn with_capacity(cap: usize) -> Self {
        OrderedLeafBuilder {
            vals: Vec::with_capacity(cap),
        }
    }
    #[inline]
    fn push_tuple(&mut self, tuple: (K, R)) {
        self.vals.push(tuple)
    }

    fn tuples(&self) -> usize {
        self.vals.len()
    }
}

#[derive(DeepSizeOf)]
pub struct UnorderedLeafBuilder<K, R> {
    pub vals: Vec<(K, R)>,
    boundary: usize,
}

impl<K: Ord + Clone, R: Eq + HasZero + AddAssignByRef + Clone> Builder
    for UnorderedLeafBuilder<K, R>
{
    type Trie = OrderedLeaf<K, R>;

    fn boundary(&mut self) -> usize {
        let consolidated_len = consolidate_slice(&mut self.vals[self.boundary..]);
        self.boundary += consolidated_len;
        self.vals.truncate(self.boundary);
        self.boundary
    }
    fn done(mut self) -> Self::Trie {
        self.boundary();
        OrderedLeaf { vals: self.vals }
    }
}

impl<K: Ord + Clone, R: Eq + HasZero + AddAssignByRef + Clone> TupleBuilder
    for UnorderedLeafBuilder<K, R>
{
    type Item = (K, R);
    fn new() -> Self {
        UnorderedLeafBuilder {
            vals: Vec::new(),
            boundary: 0,
        }
    }
    fn with_capacity(cap: usize) -> Self {
        UnorderedLeafBuilder {
            vals: Vec::with_capacity(cap),
            boundary: 0,
        }
    }
    #[inline]
    fn push_tuple(&mut self, tuple: (K, R)) {
        self.vals.push(tuple)
    }

    fn tuples(&self) -> usize {
        self.vals.len()
    }
}

/// A cursor for walking through an unordered sequence of values.
///
/// This cursor does not support `seek`, though I'm not certain how to expose
/// this.
#[derive(Clone, Debug)]
pub struct OrderedLeafCursor {
    pos: usize,
    bounds: (usize, usize),
}

impl OrderedLeafCursor {
    pub fn seek_key<K: Eq + Ord + Clone, R: Clone>(
        &mut self,
        storage: &OrderedLeaf<K, R>,
        key: &K,
    ) {
        self.pos += advance(&storage.vals[self.pos..self.bounds.1], |(k, _)| k.lt(key));
    }
}

impl<K: Eq + Ord + Clone, R: Clone> Cursor<OrderedLeaf<K, R>> for OrderedLeafCursor {
    type Key = (K, R);
    type ValueStorage = ();

    fn keys(&self) -> usize {
        self.bounds.1 - self.bounds.0
    }
    fn key<'a>(&self, storage: &'a OrderedLeaf<K, R>) -> &'a Self::Key {
        &storage.vals[self.pos]
    }
    fn values<'a>(&self, _storage: &'a OrderedLeaf<K, R>) -> (&'a (), ()) {
        (&(), ())
    }
    fn step(&mut self, storage: &OrderedLeaf<K, R>) {
        self.pos += 1;
        if !self.valid(storage) {
            self.pos = self.bounds.1;
        }
    }
    fn seek(&mut self, storage: &OrderedLeaf<K, R>, key: &Self::Key) {
        self.seek_key(storage, &key.0);
    }
    fn valid(&self, _storage: &OrderedLeaf<K, R>) -> bool {
        self.pos < self.bounds.1
    }
    fn rewind(&mut self, _storage: &OrderedLeaf<K, R>) {
        self.pos = self.bounds.0;
    }
    fn reposition(&mut self, _storage: &OrderedLeaf<K, R>, lower: usize, upper: usize) {
        self.pos = lower;
        self.bounds = (lower, upper);
    }
}

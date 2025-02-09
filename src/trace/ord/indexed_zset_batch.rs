use std::{
    cmp::max,
    convert::{TryFrom, TryInto},
    fmt::Debug,
    marker::PhantomData,
    ops::{Add, AddAssign, Neg},
    rc::Rc,
};

use timely::progress::Antichain;

use crate::{
    algebra::{AddAssignByRef, AddByRef, HasZero, MonoidValue, NegByRef},
    lattice::Lattice,
    trace::{
        layers::{
            ordered::{OrdOffset, OrderedBuilder, OrderedCursor, OrderedLayer},
            ordered_leaf::{OrderedLeaf, OrderedLeafBuilder},
            Builder as TrieBuilder, Cursor as TrieCursor, MergeBuilder, Trie, TupleBuilder,
        },
        ord::merge_batcher::MergeBatcher,
        Batch, BatchReader, Builder, Cursor, Merger,
    },
    NumEntries, SharedRef,
};

use deepsize::DeepSizeOf;

/// An immutable collection of update tuples.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrdIndexedZSet<K, V, R, O = usize>
where
    K: Ord,
    V: Ord,
    R: Clone,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    /// Where all the dataz is.
    pub layer: OrderedLayer<K, OrderedLeaf<V, R>, O>,
    pub lower: Antichain<()>,
    pub upper: Antichain<()>,
}

impl<K, V, R, O> HasZero for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn zero() -> Self {
        Self::empty(())
    }

    fn is_zero(&self) -> bool {
        self.is_empty()
    }
}

impl<K, V, R, O> SharedRef for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone,
    V: Ord + Clone,
    R: Clone,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Target = Self;

    fn try_into_owned(self) -> Result<Self::Target, Self> {
        Ok(self)
    }
}

impl<K, V, R, O> From<OrderedLayer<K, OrderedLeaf<V, R>, O>> for OrdIndexedZSet<K, V, R, O>
where
    K: Ord,
    V: Ord,
    R: Clone,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn from(layer: OrderedLayer<K, OrderedLeaf<V, R>, O>) -> Self {
        Self {
            layer,
            lower: Antichain::from_elem(()),
            upper: Antichain::new(),
        }
    }
}

impl<K, V, R, O> From<OrderedLayer<K, OrderedLeaf<V, R>, O>> for Rc<OrdIndexedZSet<K, V, R, O>>
where
    K: Ord,
    V: Ord,
    R: Clone,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn from(layer: OrderedLayer<K, OrderedLeaf<V, R>, O>) -> Self {
        Rc::new(From::from(layer))
    }
}

impl<K, V, R, O> TryFrom<Rc<OrdIndexedZSet<K, V, R, O>>> for OrdIndexedZSet<K, V, R, O>
where
    K: Ord,
    V: Ord,
    R: Clone,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Error = Rc<OrdIndexedZSet<K, V, R, O>>;

    fn try_from(batch: Rc<OrdIndexedZSet<K, V, R, O>>) -> Result<Self, Self::Error> {
        Rc::try_unwrap(batch)
    }
}

impl<K, V, R, O> DeepSizeOf for OrdIndexedZSet<K, V, R, O>
where
    K: DeepSizeOf + Ord,
    V: DeepSizeOf + Ord,
    R: DeepSizeOf + Clone,
    O: DeepSizeOf + OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn deep_size_of_children(&self, _context: &mut deepsize::Context) -> usize {
        self.layer.deep_size_of()
    }
}

impl<K, V, R, O> NumEntries for OrdIndexedZSet<K, V, R, O>
where
    K: Clone + Ord,
    V: Clone + Ord,
    R: Eq + HasZero + AddAssignByRef + Clone,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn num_entries_shallow(&self) -> usize {
        self.layer.num_entries_shallow()
    }

    fn num_entries_deep(&self) -> usize {
        self.layer.num_entries_deep()
    }

    const CONST_NUM_ENTRIES: Option<usize> =
        <OrderedLayer<K, OrderedLeaf<V, R>, O>>::CONST_NUM_ENTRIES;
}

impl<K, V, R, O> NegByRef for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone,
    V: Ord + Clone,
    R: MonoidValue + NegByRef,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn neg_by_ref(&self) -> Self {
        Self {
            layer: self.layer.neg_by_ref(),
            lower: self.lower.clone(),
            upper: self.upper.clone(),
        }
    }
}

impl<K, V, R, O> Neg for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone,
    V: Ord + Clone,
    R: MonoidValue + Neg<Output = R>,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Output = Self;

    fn neg(self) -> Self {
        Self {
            layer: self.layer.neg(),
            lower: self.lower,
            upper: self.upper,
        }
    }
}

// TODO: by-value merge
impl<K, V, R, O> Add<Self> for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        let lower = self.lower().meet(rhs.lower());
        let upper = self.upper().join(rhs.upper());

        Self {
            layer: self.layer.add(rhs.layer),
            lower,
            upper,
        }
    }
}

impl<K, V, R, O> AddAssign<Self> for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn add_assign(&mut self, rhs: Self) {
        self.lower = self.lower().meet(rhs.lower());
        self.upper = self.upper().join(rhs.upper());
        self.layer.add_assign(rhs.layer);
    }
}

impl<K, V, R, O> AddAssignByRef for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn add_assign_by_ref(&mut self, rhs: &Self) {
        self.layer.add_assign_by_ref(&rhs.layer);
        self.lower = self.lower().meet(rhs.lower());
        self.upper = self.upper().join(rhs.upper());
    }
}

impl<K, V, R, O> AddByRef for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn add_by_ref(&self, rhs: &Self) -> Self {
        Self {
            layer: self.layer.add_by_ref(&rhs.layer),
            lower: self.lower().meet(rhs.lower()),
            upper: self.upper().join(rhs.upper()),
        }
    }
}

impl<K, V, R, O> BatchReader for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Key = K;
    type Val = V;
    type Time = ();
    type R = R;
    type Cursor = OrdIndexedZSetCursor<K, V, R, O>;

    fn cursor(&self) -> Self::Cursor {
        OrdIndexedZSetCursor {
            cursor: self.layer.cursor(),
            _phantom: PhantomData,
        }
    }
    fn len(&self) -> usize {
        <OrderedLayer<K, OrderedLeaf<V, R>, O> as Trie>::tuples(&self.layer)
    }
    fn lower(&self) -> &Antichain<()> {
        &self.lower
    }
    fn upper(&self) -> &Antichain<()> {
        &self.upper
    }
}

impl<K, V, R, O> Batch for OrdIndexedZSet<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Batcher = MergeBatcher<K, V, (), R, Self>;
    type Builder = OrdIndexedZSetBuilder<K, V, R, O>;
    type Merger = OrdIndexedZSetMerger<K, V, R, O>;

    fn begin_merge(&self, other: &Self) -> Self::Merger {
        OrdIndexedZSetMerger::new(self, other)
    }

    fn recede_to(&mut self, _frontier: &()) {}
}

/// State for an in-progress merge.
pub struct OrdIndexedZSetMerger<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    // result that we are currently assembling.
    result: <OrderedLayer<K, OrderedLeaf<V, R>, O> as Trie>::MergeBuilder,
}

impl<K, V, R, O> Merger<K, V, (), R, OrdIndexedZSet<K, V, R, O>>
    for OrdIndexedZSetMerger<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn new(batch1: &OrdIndexedZSet<K, V, R, O>, batch2: &OrdIndexedZSet<K, V, R, O>) -> Self {
        OrdIndexedZSetMerger {
            result: <<OrderedLayer<K, OrderedLeaf<V, R>, O> as Trie>::MergeBuilder as MergeBuilder>::with_capacity(&batch1.layer, &batch2.layer),
        }
    }
    fn done(self) -> OrdIndexedZSet<K, V, R, O> {
        OrdIndexedZSet {
            layer: self.result.done(),
            lower: Antichain::from_elem(()),
            upper: Antichain::new(),
        }
    }
    fn work(
        &mut self,
        source1: &OrdIndexedZSet<K, V, R, O>,
        source2: &OrdIndexedZSet<K, V, R, O>,
        fuel: &mut isize,
    ) {
        *fuel -= self.result.push_merge(
            (&source1.layer, source1.layer.cursor()),
            (&source2.layer, source2.layer.cursor()),
        ) as isize;
        *fuel = max(*fuel, 1);
    }
}

/// A cursor for navigating a single layer.
#[derive(Debug)]
pub struct OrdIndexedZSetCursor<K, V, R, O>
where
    K: Ord + Clone,
    V: Ord + Clone,
    R: MonoidValue,
{
    cursor: OrderedCursor<OrderedLeaf<V, R>>,
    _phantom: PhantomData<(K, O)>,
}

impl<K, V, R, O> Cursor<K, V, (), R> for OrdIndexedZSetCursor<K, V, R, O>
where
    K: Ord + Clone,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    type Storage = OrdIndexedZSet<K, V, R, O>;

    fn key<'a>(&self, storage: &'a Self::Storage) -> &'a K {
        self.cursor.key(&storage.layer)
    }
    fn val<'a>(&self, storage: &'a Self::Storage) -> &'a V {
        &self.cursor.child.key(&storage.layer.vals).0
    }
    fn map_times<L: FnMut(&(), &R)>(&mut self, storage: &Self::Storage, mut logic: L) {
        if self.cursor.child.valid(&storage.layer.vals) {
            logic(&(), &self.cursor.child.key(&storage.layer.vals).1);
        }
    }
    fn weight(&mut self, storage: &Self::Storage) -> R {
        debug_assert!(self.cursor.child.valid(&storage.layer.vals));
        self.cursor.child.key(&storage.layer.vals).1.clone()
    }

    fn key_valid(&self, storage: &Self::Storage) -> bool {
        self.cursor.valid(&storage.layer)
    }
    fn val_valid(&self, storage: &Self::Storage) -> bool {
        self.cursor.child.valid(&storage.layer.vals)
    }
    fn step_key(&mut self, storage: &Self::Storage) {
        self.cursor.step(&storage.layer);
    }
    fn seek_key(&mut self, storage: &Self::Storage, key: &K) {
        self.cursor.seek(&storage.layer, key);
    }
    fn step_val(&mut self, storage: &Self::Storage) {
        self.cursor.child.step(&storage.layer.vals);
    }
    fn seek_val(&mut self, storage: &Self::Storage, val: &V) {
        self.cursor.child.seek_key(&storage.layer.vals, val);
    }
    fn rewind_keys(&mut self, storage: &Self::Storage) {
        self.cursor.rewind(&storage.layer);
    }
    fn rewind_vals(&mut self, storage: &Self::Storage) {
        self.cursor.child.rewind(&storage.layer.vals);
    }
}

/// A builder for creating layers from unsorted update tuples.
pub struct OrdIndexedZSetBuilder<K, V, R, O>
where
    K: Ord,
    V: Ord,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    builder: OrderedBuilder<K, OrderedLeafBuilder<V, R>, O>,
}

impl<K, V, R, O> Builder<K, V, (), R, OrdIndexedZSet<K, V, R, O>>
    for OrdIndexedZSetBuilder<K, V, R, O>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone,
    R: MonoidValue,
    O: OrdOffset,
    <O as TryFrom<usize>>::Error: Debug,
    <O as TryInto<usize>>::Error: Debug,
{
    fn new(_time: ()) -> Self {
        OrdIndexedZSetBuilder {
            builder: <OrderedBuilder<K, OrderedLeafBuilder<V, R>, O>>::new(),
        }
    }

    fn with_capacity(_time: (), cap: usize) -> Self {
        OrdIndexedZSetBuilder {
            builder:
                <OrderedBuilder<K, OrderedLeafBuilder<V, R>, O> as TupleBuilder>::with_capacity(cap),
        }
    }

    #[inline]
    fn push(&mut self, (key, val, diff): (K, V, R)) {
        self.builder.push_tuple((key, (val, diff)));
    }

    #[inline(never)]
    fn done(self) -> OrdIndexedZSet<K, V, R, O> {
        OrdIndexedZSet {
            layer: self.builder.done(),
            lower: Antichain::from_elem(()),
            upper: Antichain::new(),
        }
    }
}

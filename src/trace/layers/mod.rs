//! Traits and types for building trie-based indices.
//!
//! The trie structure has each each element of each layer indicate a range of
//! elements in the next layer. Similarly, ranges of elements in the layer
//! itself may correspond to single elements in the layer above.

use crate::algebra::HasZero;

pub mod ordered;
pub mod ordered_leaf;
// pub mod hashed;
// pub mod weighted;
// pub mod unordered;

/// A collection of tuples, and types for building and enumerating them.
///
/// There are some implicit assumptions about the elements in trie-structured
/// data, mostly that the items have some `(key, val)` structure. Perhaps we
/// will nail these down better in the future and get a better name for the
/// trait.
pub trait Trie: Sized {
    /// The type of item from which the type is constructed.
    type Item;
    /// The type of cursor used to navigate the type.
    type Cursor: Cursor<Self>;
    /// The type used to merge instances of the type together.
    type MergeBuilder: MergeBuilder<Trie = Self>;
    /// The type used to assemble instances of the type from its `Item`s.
    type TupleBuilder: TupleBuilder<Trie = Self, Item = Self::Item>;

    /// The number of distinct keys, as distinct from the total number of
    /// tuples.
    fn keys(&self) -> usize;

    /// True if `self.keys()` is zero.
    fn is_empty(&self) -> bool {
        self.keys() == 0
    }

    /// The total number of tuples in the collection.
    fn tuples(&self) -> usize;
    /// Returns a cursor capable of navigating the collection.
    fn cursor(&self) -> Self::Cursor {
        self.cursor_from(0, self.keys())
    }
    /// Returns a cursor over a range of data, commonly used by others to
    /// restrict navigation to sub-collections.
    fn cursor_from(&self, lower: usize, upper: usize) -> Self::Cursor;

    /// Merges two collections into a third.
    ///
    /// Collections are allowed their own semantics for merging. For example,
    /// unordered collections simply collect values, whereas weighted
    /// collections accumulate weights and discard elements whose weights
    /// are zero.
    fn merge(&self, other: &Self) -> Self {
        let mut merger = Self::MergeBuilder::with_capacity(self, other);
        // println!("{:?} and {:?}", self.keys(), other.keys());
        merger.push_merge((self, self.cursor()), (other, other.cursor()));
        merger.done()
    }
}

pub struct TrieSlice<'a, T: Trie>(&'a T, T::Cursor);

impl<T: Trie> HasZero for T {
    fn is_zero(&self) -> bool {
        self.keys() == 0
    }
    fn zero() -> Self {
        <Self as Trie>::TupleBuilder::new().done()
    }
}

/// A type used to assemble collections.
pub trait Builder {
    /// The type of collection produced.
    type Trie: Trie;
    /// Requests a commitment to the offset of the current-most sub-collection.
    ///
    /// This is most often used by parent collections to indicate that some set
    /// of values are now logically distinct from the next set of values,
    /// and that the builder should acknowledge this and report the limit
    /// (to store as an offset in the parent collection).
    fn boundary(&mut self) -> usize;
    /// Finalizes the building process and returns the collection.
    fn done(self) -> Self::Trie;
}

/// A type used to assemble collections by merging other instances.
pub trait MergeBuilder: Builder {
    /// Allocates an instance of the builder with sufficient capacity to contain
    /// the merged data.
    fn with_capacity(other1: &Self::Trie, other2: &Self::Trie) -> Self;
    fn with_key_capacity(cap: usize) -> Self;
    /// Copies sub-collections of `other` into this collection.
    fn copy_range(&mut self, other: &Self::Trie, lower: usize, upper: usize);
    /// Merges two sub-collections into one sub-collection.
    fn push_merge(
        &mut self,
        other1: (&Self::Trie, <Self::Trie as Trie>::Cursor),
        other2: (&Self::Trie, <Self::Trie as Trie>::Cursor),
    ) -> usize;
}

/// A type used to assemble collections from ordered sequences of tuples.
pub trait TupleBuilder: Builder {
    /// The type of item accepted for construction.
    type Item;
    /// Allocates a new builder.
    fn new() -> Self;
    /// Allocates a new builder with capacity for at least `cap` tuples.
    fn with_capacity(cap: usize) -> Self; // <-- unclear how to set child capacities...
    /// Inserts a new into the collection.
    fn push_tuple(&mut self, tuple: Self::Item);
    fn tuples(&self) -> usize;
}

/// A type supporting navigation.
///
/// The precise meaning of this navigation is not defined by the trait. It is
/// likely that having navigated around, the cursor will be different in some
/// other way, but the `Cursor` trait does not explain how this is so.
pub trait Cursor<Storage> {
    /// The type revealed by the cursor.
    type Key;
    type ValueStorage: Trie;

    fn keys(&self) -> usize;
    /// Reveals the current key.
    fn key<'a>(&self, storage: &'a Storage) -> &'a Self::Key;
    fn values<'a>(
        &self,
        storage: &'a Storage,
    ) -> (&'a Self::ValueStorage, <Self::ValueStorage as Trie>::Cursor);
    /// Advances the cursor by one element.
    fn step(&mut self, storage: &Storage);
    /// Advances the cursor until the location where `key` would be expected.
    fn seek(&mut self, storage: &Storage, key: &Self::Key);
    /// Returns `true` if the cursor points at valid data. Returns `false` if
    /// the cursor is exhausted.
    fn valid(&self, storage: &Storage) -> bool;
    /// Rewinds the cursor to its initial state.
    fn rewind(&mut self, storage: &Storage);
    /// Repositions the cursor to a different range of values.
    fn reposition(&mut self, storage: &Storage, lower: usize, upper: usize);
}

/// Reports the number of elements satisfing the predicate.
///
/// This methods *relies strongly* on the assumption that the predicate
/// stays false once it becomes false, a joint property of the predicate
/// and the slice. This allows `advance` to use exponential search to
/// count the number of elements in time logarithmic in the result.
pub fn advance<T, F: Fn(&T) -> bool>(slice: &[T], function: F) -> usize {
    let small_limit = 8;

    // Exponential seach if the answer isn't within `small_limit`.
    if slice.len() > small_limit && function(&slice[small_limit]) {
        // start with no advance
        let mut index = small_limit + 1;
        if index < slice.len() && function(&slice[index]) {
            // advance in exponentially growing steps.
            let mut step = 1;
            while index + step < slice.len() && function(&slice[index + step]) {
                index += step;
                step <<= step;
            }

            // advance in exponentially shrinking steps.
            step >>= 1;
            while step > 0 {
                if index + step < slice.len() && function(&slice[index + step]) {
                    index += step;
                }
                step >>= 1;
            }

            index += 1;
        }

        index
    } else {
        let limit = std::cmp::min(slice.len(), small_limit);
        slice[..limit].iter().filter(|x| function(*x)).count()
    }
}

impl Trie for () {
    type Item = ();
    type Cursor = ();
    type MergeBuilder = ();
    type TupleBuilder = ();

    fn keys(&self) -> usize {
        0
    }
    fn tuples(&self) -> usize {
        0
    }
    fn cursor_from(&self, _lower: usize, _upper: usize) -> Self::Cursor {}
}

impl Builder for () {
    type Trie = ();

    fn boundary(&mut self) -> usize {
        0
    }
    fn done(self) -> Self::Trie {}
}

impl MergeBuilder for () {
    fn with_capacity(_other1: &(), _other2: &()) -> Self {}
    fn with_key_capacity(_cap: usize) -> Self {}
    fn copy_range(&mut self, _other: &Self::Trie, _lower: usize, _upper: usize) {}
    fn push_merge(
        &mut self,
        _other1: (&Self::Trie, <Self::Trie as Trie>::Cursor),
        _other2: (&Self::Trie, <Self::Trie as Trie>::Cursor),
    ) -> usize {
        0
    }
}

impl TupleBuilder for () {
    type Item = ();

    fn new() -> Self {}
    fn with_capacity(_cap: usize) -> Self {}
    fn push_tuple(&mut self, _tuple: Self::Item) {}
    fn tuples(&self) -> usize {
        0
    }
}

impl Cursor<()> for () {
    type Key = ();
    type ValueStorage = ();

    fn keys(&self) -> usize {
        0
    }
    fn key<'a>(&self, _storage: &'a ()) -> &'a Self::Key {
        &()
    }
    fn values<'a>(&self, _storage: &'a ()) -> (&'a (), ()) {
        (&(), ())
    }
    fn step(&mut self, _storage: &()) {}
    fn seek(&mut self, _storage: &(), _key: &Self::Key) {}
    fn valid(&self, _storage: &()) -> bool {
        false
    }
    fn rewind(&mut self, _storage: &()) {}
    fn reposition(&mut self, _storage: &(), _lower: usize, _upper: usize) {}
}

//! Defines a sink operator that inspects every element of its input stream by
//! applying a user-provided callback to it.

use crate::circuit::{
    operator_traits::{Operator, SinkOperator},
    Circuit, Stream,
};
use std::{borrow::Cow, marker::PhantomData};

impl<P, D> Stream<Circuit<P>, D>
where
    D: Clone + 'static,
    P: Clone + 'static,
{
    /// Apply [`Inspect`] operator to `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use dbsp::{
    /// #     circuit::Root,
    /// #     operator::Generator,
    /// # };
    /// let root = Root::build(move |circuit| {
    ///     let mut n = 1;
    ///     let stream = circuit.add_source(Generator::new(move || {
    ///         let res = n;
    ///         n += 1;
    ///         res
    ///     }));
    ///     // Print all values in `stream`.
    ///     stream.inspect(|n| println!("inspect: {}", n));
    /// })
    /// .unwrap();
    /// ```
    pub fn inspect<F>(&self, callback: F)
    where
        F: FnMut(&D) + 'static,
    {
        self.circuit().add_sink(Inspect::new(callback), self);
    }
}

/// Sink operator that consumes a stream of values of type `T` and
/// applies a user-provided callback to each input.
pub struct Inspect<T, F> {
    callback: F,
    phantom: PhantomData<T>,
}

impl<T, F> Inspect<T, F>
where
    F: FnMut(&T),
{
    /// Create a new instance of the `Inspect` operator that will apply
    /// `callback` to each value in the input stream.
    pub fn new(callback: F) -> Self {
        Self {
            callback,
            phantom: PhantomData,
        }
    }
}

impl<T, F> Operator for Inspect<T, F>
where
    T: 'static,
    F: FnMut(&T) + 'static,
{
    fn name(&self) -> Cow<'static, str> {
        Cow::from("Inspect")
    }

    fn fixedpoint(&self) -> bool {
        true
    }
}

impl<T, F> SinkOperator<T> for Inspect<T, F>
where
    T: 'static,
    F: FnMut(&T) + 'static,
{
    fn eval(&mut self, i: &T) {
        (self.callback)(i)
    }
}

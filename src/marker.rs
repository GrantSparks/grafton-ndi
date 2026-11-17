//! Markers for deliberately restricted auto traits.

use std::{cell::Cell, marker::PhantomData};

/// A field that withholds [`Sync`] from the type that holds it, and nothing
/// else: that type keeps [`Send`] and both unwind traits.
#[derive(Default)]
pub(crate) struct NotSync(PhantomData<Cell<()>>);

// It holds nothing, so a panic cannot leave it half-changed.
impl std::panic::RefUnwindSafe for NotSync {}

//! A super-simple bump allocator that allocates off from a fixed-size array.
//!
//! While dropping the `BumpBox` boxes will call the destructor of the contained object,
//! the actual memory will be free only when the unsafe `reset` method is called.
//!
//! The primary use case of this allocator is reduction of Rust future sizes, due to
//! `rustc` not being very intelligent w.r.t. stack usage in async functions.

use core::mem::{self, MaybeUninit};
use core::pin::Pin;
use core::slice;

use embassy_sync::blocking_mutex::raw::RawMutex;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, zeroed, Init};
use rs_matter::utils::sync::blocking::Mutex;

#[macro_export]
macro_rules! alloc {
    ($bump:expr, $obj:expr) => {
        $bump.alloc($obj, concat!(file!(), ":", line!()))
    };
}

#[macro_export]
macro_rules! pin_alloc {
    ($bump:expr, $obj:expr) => {
        $bump.pin_alloc($obj, concat!(file!(), ":", line!()))
    };
}

/// A bump allocator that uses a provided memory chunk
pub struct Bump<const N: usize, M> {
    inner: Mutex<M, RefCell<Inner<N>>>,
}

impl<const N: usize, M: RawMutex> Default for Bump<N, M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, M: RawMutex> Bump<N, M> {
    /// Create a new bump allocator
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(RefCell::new(Inner::new())),
        }
    }

    /// Return an initializer for a new bump allocator
    pub fn init() -> impl Init<Self> {
        init!(Self {
            inner <- Mutex::init(RefCell::init(Inner::init())),
        })
    }

    /// Reset the allocator, making all previously allocated memory available again.
    ///
    /// # Safety
    /// This is unsafe because any previously allocated objects that are still in use
    /// will get their memory corrupted and overwritten with new objects.
    ///
    /// Make sure that NO previously allocated objects are still in use
    /// when calling this method.
    pub unsafe fn reset(&self) {
        self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();

            inner.offset = 0;
        });
    }

    /// Allocate an object and return it pinned in a `Pin<BumpBox<T>>`
    ///
    /// # Arguments
    /// - `object`: The object to allocate
    /// - `location`: A string describing the location of the allocation, for logging purposes
    ///
    /// # Panics
    /// This function will panic if there is not enough memory left in the bump allocator
    pub fn pin_alloc<T>(&self, object: T, location: &str) -> Pin<BumpBox<'_, T>>
    where
        T: Sized,
    {
        let boxed = self.alloc(object, location);

        boxed.into_pin()
    }

    /// Allocate an object and return it in a `BumpBox<T>`
    ///
    /// # Arguments
    /// - `object`: The object to allocate
    /// - `location`: A string describing the location of the allocation, for logging purposes
    ///
    /// # Panics
    /// This function will panic if there is not enough memory left in the bump allocator
    pub fn alloc<T>(&self, object: T, location: &str) -> BumpBox<'_, T>
    where
        T: Sized,
    {
        self.inner.lock(|inner| {
            // SAFETY:
            // The idea is to have a large chunk of memory allocated on the stack,
            // with this function one can reserve a chunk of that memory for an object
            // of type T.
            //
            // To reserve the memory, it will move the offset forward by the size required
            // for T, and return a **mutable** reference to it.
            //
            // Given that it returns a mutable reference to it, there cannot be any other
            // references to that memory location. This is ensured by the offset.

            let size = mem::size_of_val(&object);

            let mut inner = inner.borrow_mut();
            let offset = inner.offset;

            info!(
                "BUMP[{}]: {}b (U:{}b/F:{}b)",
                location,
                size,
                offset,
                inner.memory.len() - offset
            );

            // SAFETY: The lifetime of the returned reference is bound to &self -> it will not outlive the data it is borrowing.
            let value = unsafe {
                let t_buf = inner.allocate_for::<T>(1);

                t_buf[0].write(object)
            };

            BumpBox { value }
        })
    }
}

/// A box-like container that uses bump allocation
pub struct BumpBox<'a, T> {
    value: &'a mut T,
}

impl<T> BumpBox<'_, T> {
    /// Convert the `BumpBox<T>` into a `Pin<BumpBox<T>>`
    pub fn into_pin(self) -> Pin<Self> {
        // It's not possible to move or replace the insides of a `Pin<Box<T>>`
        // when `T: !Unpin`, so it's safe to pin it directly without any
        // additional requirements.
        unsafe { Pin::new_unchecked(self) }
    }
}

impl<T> core::ops::Deref for BumpBox<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value
    }
}

impl<T> core::ops::DerefMut for BumpBox<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value
    }
}

impl<T> Unpin for BumpBox<'_, T> {}

struct Inner<const N: usize> {
    memory: [MaybeUninit<u8>; N],
    offset: usize,
}

impl<const N: usize> Inner<N> {
    const fn new() -> Self {
        Self {
            memory: [const { MaybeUninit::uninit() }; N],
            offset: 0,
        }
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            memory <- zeroed(),
            offset: 0,
        })
    }

    /// Allocate space for `count` objects of type `T`
    ///
    /// # Panics
    ///
    /// If there is not enough memory left in the bump allocator to
    /// allocate the requested objects.
    ///
    /// # Safety
    ///
    /// This function returns a mutable reference to the allocated memory
    /// that lives independently of the lifetime of `self`.
    /// This could result in undefined behavior where the reference outlives
    /// the bump allocator itself.
    ///
    /// The caller must ensure that the returned reference does not outlive
    /// the bump allocator.
    unsafe fn allocate_for<'s, 'b, T>(&'s mut self, count: usize) -> &'b mut [MaybeUninit<T>] {
        // We can only use the memory from the current offset onwards, because
        // the previous memory might be in use by previously allocated objects.
        let remaining = &mut self.memory[self.offset..];
        let remaining_len = remaining.len();
        // The t_buf will be where the caller can place their objects,
        // and r_buf should be the remaining unused memory.
        let (t_buf, r_buf) = align_min::<T>(remaining, count);
        self.offset += remaining_len - r_buf.len();

        // This creates an unbounded lifetime, see the safety section of this function.
        //
        // It is necessary, because technically only one mutable reference can exist
        // to self.memory, but because it is an array, the mutable reference to self.memory
        // can be split into multiple mutable references to its parts.
        slice::from_raw_parts_mut(t_buf.as_mut_ptr(), t_buf.len())
    }
}

fn align_min<T>(
    buf: &mut [MaybeUninit<u8>],
    count: usize,
) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<u8>]) {
    if count == 0 || mem::size_of::<T>() == 0 {
        return (&mut [], buf);
    }

    let (t_leading_buf0, t_buf, _) = unsafe { buf.align_to_mut::<MaybeUninit<T>>() };
    if t_buf.len() < count {
        panic!("Out of bump memory");
    }

    // Shrink `t_buf` to the number of requested items (count)
    let t_buf = &mut t_buf[..count];
    let t_leading_buf0_len = t_leading_buf0.len();
    let t_buf_size = mem::size_of_val(t_buf);

    let (buf0, remaining_buf) = buf.split_at_mut(t_leading_buf0_len + t_buf_size);

    let (t_leading_buf, t_buf, t_remaining_buf) = unsafe { buf0.align_to_mut::<MaybeUninit<T>>() };
    assert_eq!(t_leading_buf0_len, t_leading_buf.len());
    assert_eq!(t_buf.len(), count);
    assert!(t_remaining_buf.is_empty());

    (t_buf, remaining_buf)
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    use alloc::vec::Vec;
    use rs_matter::utils::sync::blocking::raw::StdRawMutex;

    const BUMP_SIZE: usize = 1024;
    const DEFAULT_VALUE: u32 = 0xDEADBEEF;

    #[test]
    fn test_one_concurrent_borrow() {
        static BUMP: Bump<BUMP_SIZE, StdRawMutex> = Bump::new();

        for _ in 0..(BUMP_SIZE / mem::size_of_val(&DEFAULT_VALUE)) {
            let b1 = BUMP.alloc(DEFAULT_VALUE, "test1");

            assert_eq!(*b1, DEFAULT_VALUE);
        }
    }

    #[test]
    fn test_multiple_concurrent_borrow() {
        static BUMP: Bump<BUMP_SIZE, StdRawMutex> = Bump::new();

        let mut all_boxes = Vec::new();
        for i in 0..(BUMP_SIZE / mem::size_of::<usize>()) {
            all_boxes.push(alloc!(BUMP, i));
        }

        for (i, b) in all_boxes.into_iter().enumerate() {
            assert_eq!(*b, i);
        }
    }
}

//! A super-simple bump allocator that allocates off from a fixed-size array.
//!
//! While dropping the `BumpBox` boxes will call the destructor of the contained object,
//! the actual memory will be free only when the unsafe `reset` method is called.
//!
//! The primary use case of this allocator is reduction of Rust future sizes, due to
//! `rustc` not being very intelligent w.r.t. stack usage in async functions.

use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::ptr::NonNull;

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
            let mut inner = inner.borrow_mut();

            let size = core::mem::size_of_val(&object);

            let offset = inner.offset;
            let memory = unsafe { inner.memory.assume_init_mut() };

            info!(
                "BUMP[{}]: {}b (U:{}b/F:{}b)",
                location,
                size,
                offset,
                memory.len() - offset
            );

            let remaining = &mut memory[offset..];
            let remaining_len = remaining.len();

            let (t_buf, r_buf) = align_min::<T>(remaining, 1);

            // Safety: We just allocated the memory and it's properly aligned
            let ptr = unsafe {
                let ptr = t_buf.as_ptr() as *mut T;
                ptr.write(object);

                NonNull::new_unchecked(ptr)
            };

            inner.offset += remaining_len - r_buf.len();

            BumpBox {
                ptr,
                _allocator: PhantomData,
            }
        })
    }
}

/// A box-like container that uses bump allocation
pub struct BumpBox<'a, T> {
    ptr: NonNull<T>,
    _allocator: core::marker::PhantomData<&'a ()>,
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
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> core::ops::DerefMut for BumpBox<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T> Unpin for BumpBox<'_, T> {}

impl<T> Drop for BumpBox<'_, T> {
    fn drop(&mut self) {
        // Safety: The pointer is valid and we own the data
        unsafe {
            self.ptr.as_ptr().drop_in_place();
        }
    }
}

struct Inner<const N: usize> {
    memory: MaybeUninit<[u8; N]>,
    offset: usize,
}

impl<const N: usize> Inner<N> {
    const fn new() -> Self {
        Self {
            memory: MaybeUninit::uninit(),
            offset: 0,
        }
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            memory <- zeroed(),
            offset: 0,
        })
    }
}

fn align_min<T>(buf: &mut [u8], count: usize) -> (&mut [MaybeUninit<T>], &mut [u8]) {
    if count == 0 || core::mem::size_of::<T>() == 0 {
        return (&mut [], buf);
    }

    let (t_leading_buf0, t_buf, _) = unsafe { buf.align_to_mut::<MaybeUninit<T>>() };
    if t_buf.len() < count {
        panic!("Out of bump memory");
    }

    // Shrink `t_buf` to the number of requested items (count)
    let t_buf = &mut t_buf[..count];
    let t_leading_buf0_len = t_leading_buf0.len();
    let t_buf_size = core::mem::size_of_val(t_buf);

    let (buf0, remaining_buf) = buf.split_at_mut(t_leading_buf0_len + t_buf_size);

    let (t_leading_buf, t_buf, t_remaining_buf) = unsafe { buf0.align_to_mut::<MaybeUninit<T>>() };
    assert_eq!(t_leading_buf0_len, t_leading_buf.len());
    assert_eq!(t_buf.len(), count);
    assert!(t_remaining_buf.is_empty());

    (t_buf, remaining_buf)
}

/// A simple bump allocator for fixed-size memory chunks
///
/// This allocator is designed for allocating objects that have a known maximum lifetime.
/// All allocations are freed when the allocator is dropped.
use core::alloc::Layout;
use core::cell::Cell;
use core::mem::MaybeUninit;
use core::pin::Pin;
use core::ptr::NonNull;

/// A bump allocator that uses a provided memory chunk
pub struct BumpAllocator<'a> {
    memory: &'a mut [MaybeUninit<u8>],
    offset: Cell<usize>,
}

impl<'a> BumpAllocator<'a> {
    /// Create a new bump allocator with the provided memory chunk
    pub fn new(memory: &'a mut [MaybeUninit<u8>]) -> Self {
        Self {
            memory,
            offset: Cell::new(0),
        }
    }

    /// Allocate memory for an object and pin it
    pub fn alloc_pin<T>(&mut self, object: T) -> Result<Pin<BumpBox<'_, T>>, AllocError>
    where
        T: Sized,
    {
        let layout = Layout::new::<T>();
        let ptr = self.alloc_raw(layout)?;

        // Safety: We just allocated the memory and it's properly aligned
        unsafe {
            let ptr = ptr.as_ptr() as *mut T;
            ptr.write(object);
            Ok(Pin::new_unchecked(BumpBox {
                ptr: NonNull::new_unchecked(ptr),
                _allocator: core::marker::PhantomData,
            }))
        }
    }

    fn alloc_raw(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let size = layout.size();
        let align = layout.align();

        let current_offset = self.offset.get();

        // Align the offset
        let aligned_offset = (current_offset + align - 1) & !(align - 1);
        let new_offset = aligned_offset + size;

        if new_offset > self.memory.len() {
            return Err(AllocError);
        }

        self.offset.set(new_offset);

        // Safety: We checked bounds and alignment
        let ptr = unsafe { self.memory.as_mut_ptr().add(aligned_offset) as *mut u8 };

        Ok(unsafe { NonNull::new_unchecked(ptr) })
    }

    /// Get the current memory usage
    pub fn used(&self) -> usize {
        self.offset.get()
    }

    /// Get the total capacity
    pub fn capacity(&self) -> usize {
        self.memory.len()
    }
}

/// A box-like container that uses bump allocation
pub struct BumpBox<'a, T> {
    ptr: NonNull<T>,
    _allocator: core::marker::PhantomData<&'a ()>,
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

impl<T: core::future::Future> core::future::Future for BumpBox<'_, T> {
    type Output = T::Output;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        // Safety: We maintain the pin invariant
        let future = unsafe { self.map_unchecked_mut(|s| &mut **s) };
        future.poll(cx)
    }
}

impl<T> Drop for BumpBox<'_, T> {
    fn drop(&mut self) {
        // Safety: The pointer is valid and we own the data
        unsafe {
            self.ptr.as_ptr().drop_in_place();
        }
    }
}

/// Error type for allocation failures
#[derive(Debug)]
pub struct AllocError;

impl core::fmt::Display for AllocError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Bump allocator out of memory")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for AllocError {}

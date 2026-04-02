use core::borrow::{Borrow, BorrowMut};

use cfg_if::cfg_if;

use rs_matter::persist::SharedKvBlobStore;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::storage::Vec;
use rs_matter::utils::sync::{IfMutex, IfMutexGuard};

cfg_if! {
    if #[cfg(feature = "kv-blob-store-65536")] {
        const KV_BLOB_BUF_SIZE: usize = 65536;
    } else if #[cfg(feature = "kv-blob-store-32768")] {
        const KV_BLOB_BUF_SIZE: usize = 32768;
    } else if #[cfg(feature = "kv-blob-store-16384")] {
        const KV_BLOB_BUF_SIZE: usize = 16384;
    } else if #[cfg(feature = "kv-blob-store-8192")] {
        const KV_BLOB_BUF_SIZE: usize = 8192;
    } else if #[cfg(feature = "kv-blob-store-4096")] {
        const KV_BLOB_BUF_SIZE: usize = 4096;
    } else if #[cfg(feature = "kv-blob-store-2048")] {
        const KV_BLOB_BUF_SIZE: usize = 2048;
    } else if #[cfg(feature = "kv-blob-store-1024")] {
        const KV_BLOB_BUF_SIZE: usize = 1024;
    } else {
        pub const KV_BLOB_BUF_SIZE: usize = 4096;
    }
}

/// A type alias for the `SharedKvBlobStore` specialization used in `MatterStack`.
pub type MatterSharedKvBlobStore<'a, S> = SharedKvBlobStore<S, MatterKvBlobStoreBufInstance<'a>>;

/// A buffer for the KV blob store, which is used to read and write blobs from the storage.
pub(crate) struct MatterKvBlobStoreBuf {
    inner: IfMutex<Vec<u8, KV_BLOB_BUF_SIZE>>,
}

impl MatterKvBlobStoreBuf {
    pub(crate) const fn new() -> Self {
        Self {
            inner: IfMutex::new(Vec::new()),
        }
    }

    pub(crate) fn init() -> impl Init<Self> {
        init!(Self {
            inner <- IfMutex::init(Vec::init()),
        })
    }

    pub(crate) fn try_get(&self) -> Option<MatterKvBlobStoreBufInstance<'_>> {
        let mut buf = self.inner.try_lock().ok()?;
        unwrap!(buf.resize_default(KV_BLOB_BUF_SIZE));

        Some(MatterKvBlobStoreBufInstance(buf))
    }
}

/// An instance of the KV blob store buffer, which is used to read and write blobs from the storage.
pub struct MatterKvBlobStoreBufInstance<'a>(IfMutexGuard<'a, Vec<u8, KV_BLOB_BUF_SIZE>>);

impl Borrow<[u8]> for MatterKvBlobStoreBufInstance<'_> {
    fn borrow(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl BorrowMut<[u8]> for MatterKvBlobStoreBufInstance<'_> {
    fn borrow_mut(&mut self) -> &mut [u8] {
        self.0.as_mut_slice()
    }
}

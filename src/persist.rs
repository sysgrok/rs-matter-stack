use core::fmt::Display;

use cfg_if::cfg_if;
use embassy_futures::select::select;
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};

use rs_matter::dm::networks::wireless::{WirelessNetwork, WirelessNetworks};
use rs_matter::error::Error;
use rs_matter::utils::storage::pooled::{BufferAccess, PooledBuffer, PooledBuffers};
use rs_matter::utils::sync::{IfMutex, IfMutexGuard};
use rs_matter::Matter;

use crate::private::Sealed;

#[cfg(feature = "std")]
pub use file::DirKvBlobStore;

/// A persister for Matter that relies on a BLOB key-value storage
/// represented by the `KvBlobStore`/`SharedKvBlobStore` traits.
pub struct MatterPersist<'a, S, C> {
    store: SharedKvBlobStore<'a, S>,
    matter: &'a Matter<'a>,
    networks: C,
}

impl<'a, S, C> MatterPersist<'a, S, C>
where
    S: KvBlobStore,
    C: NetworkPersist,
{
    /// Create a new `MatterPersist` instance.
    pub fn new(store: SharedKvBlobStore<'a, S>, matter: &'a Matter<'a>, networks: C) -> Self {
        Self {
            store,
            matter,
            networks,
        }
    }

    /// Get the underlying `SharedKvBlobStore` instance.
    ///
    /// This can be used to access vendor-specific keys.
    pub fn store(&self) -> &SharedKvBlobStore<'a, S> {
        &self.store
    }

    /// Reset the persist instance, removing all stored data from the non-volatile storage
    /// as well as removing all ACLs, fabrics and Wifi networks from the Matter stack.
    pub async fn reset(&self) -> Result<(), Error> {
        let (mut kv, mut buf) = self.store.get().await;

        kv.remove(MatterStackKey::Fabrics as _, &mut buf).await?;
        kv.remove(MatterStackKey::BasicInfo as _, &mut buf).await?;
        kv.remove(MatterStackKey::Networks as _, &mut buf).await?;

        self.networks.reset()?;

        Ok(())
    }

    /// Load the Matter stack from the non-volatile storage.
    pub async fn load(&self) -> Result<(), Error> {
        let (mut kv, mut buf) = self.store.get().await;

        kv.load(MatterStackKey::Fabrics as _, &mut buf, |data| {
            if let Some(data) = data {
                self.matter.load_fabrics(data)?;
            }

            Ok(())
        })
        .await?;

        kv.load(MatterStackKey::BasicInfo as _, &mut buf, |data| {
            if let Some(data) = data {
                self.matter.load_basic_info(data)?;
            }

            Ok(())
        })
        .await?;

        kv.load(MatterStackKey::Networks as _, &mut buf, |data| {
            if let Some(data) = data {
                self.networks.load(data)?;
            }

            Ok(())
        })
        .await?;

        Ok(())
    }

    /// Return `true` if the Matter stack has changed since the last call to `store`.
    pub fn changed(&self) -> bool {
        self.matter.fabrics_changed() || self.networks.changed()
    }

    /// Store the Matter stack to the non-volatile storage, if it has changed.
    ///
    /// Return `true` if the Matter stack was changed and therefore has been stored.
    pub async fn save(&self) -> Result<bool, Error> {
        if self.changed() {
            let (mut kv, mut buf) = self.store.get().await;

            if self.matter.fabrics_changed() {
                kv.store(MatterStackKey::Fabrics as _, &mut buf, |buf| {
                    self.matter.store_fabrics(buf)
                })
                .await?;
            }

            if self.matter.basic_info_changed() {
                kv.store(MatterStackKey::BasicInfo as _, &mut buf, |buf| {
                    self.matter.store_basic_info(buf)
                })
                .await?;
            }

            if self.networks.changed() {
                kv.store(MatterStackKey::Networks as _, &mut buf, |buf| {
                    self.networks.store(buf)
                })
                .await?;
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Run the persist instance, listening for changes in the Matter stack's state.
    pub async fn run(&self) -> Result<(), Error> {
        loop {
            let wait_fabrics = self.matter.wait_persist();
            let wait_networks = self.networks.wait_state_changed();

            select(wait_fabrics, wait_networks).await;

            self.save().await?;
        }
    }
}

/// A perist API that needs to be implemented by the network impl which is used in the Matter stack.
///
/// The trait is sealed and has only two implementations:
/// - `()` - which is used with the `Eth` network
/// - `&NetworkContext` - which is used with the `WirelessBle` network.
pub trait NetworkPersist: Sealed {
    /// Reset all networks, removing all stored data from the memory
    fn reset(&self) -> Result<(), Error>;

    /// Load the networks from the provided data BLOB
    fn load(&self, data: &[u8]) -> Result<(), Error>;

    /// Save the networks as BLOB using the provided data buffer
    ///
    /// Return the length of the data written in the provided buffer.
    fn store(&self, buf: &mut [u8]) -> Result<usize, Error>;

    /// Check if the networks have changed.
    ///
    /// This method should return `Ok(true)` if the networks have changed since the last call to `changed`.
    fn changed(&self) -> bool;

    /// Wait until the networks have changed.
    ///
    /// This method might return even if the networks have not changed,
    /// so the ultimate litmus test whether the networks did indeed change is trying to call
    /// `store` and then inspecting if it returned something.
    async fn wait_state_changed(&self);
}

impl<const N: usize, M, T> Sealed for &WirelessNetworks<N, M, T> where M: RawMutex {}

impl<const N: usize, M, T> NetworkPersist for &WirelessNetworks<N, M, T>
where
    M: RawMutex,
    T: WirelessNetwork,
{
    fn reset(&self) -> Result<(), Error> {
        WirelessNetworks::reset(self);

        Ok(())
    }

    fn load(&self, data: &[u8]) -> Result<(), Error> {
        WirelessNetworks::load(self, data)
    }

    fn store(&self, buf: &mut [u8]) -> Result<usize, Error> {
        WirelessNetworks::store(self, buf)
    }

    fn changed(&self) -> bool {
        WirelessNetworks::changed(self)
    }

    async fn wait_state_changed(&self) {
        WirelessNetworks::wait_persist(self).await
    }
}

/// A no-op implementation of the `NetworksPersist` trait.
///
/// Used when the Matter stack is configured for Ethernet, as in that case
/// there is no network state that needs to be saved
impl NetworkPersist for () {
    fn reset(&self) -> Result<(), Error> {
        Ok(())
    }

    fn load(&self, _data: &[u8]) -> Result<(), Error> {
        Ok(())
    }

    fn store(&self, _buf: &mut [u8]) -> Result<usize, Error> {
        Ok(0)
    }

    fn changed(&self) -> bool {
        false
    }

    async fn wait_state_changed(&self) {
        core::future::pending().await
    }
}

/// The first key available for the vendor-specific data.
pub const VENDOR_KEYS_START: u16 = 0x1000;

/// The keys currently used by the `rs-matter-stack`.
///
/// All keys with values up to 0xfff are reserved for `rs-matter-stack` use.
/// Keys >= 0x1000 are available for downstream crates.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u16)]
pub enum MatterStackKey {
    Fabrics = 0,
    BasicInfo = 1,
    Networks = 2,
}

impl TryFrom<u16> for MatterStackKey {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(MatterStackKey::Fabrics),
            1 => Ok(MatterStackKey::BasicInfo),
            2 => Ok(MatterStackKey::Networks),
            _ => Err(()),
        }
    }
}

impl Display for MatterStackKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            MatterStackKey::Fabrics => "fabrics",
            MatterStackKey::BasicInfo => "basic-info",
            MatterStackKey::Networks => "networks",
        };

        write!(f, "{}", s)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for MatterStackKey {
    fn format(&self, f: defmt::Formatter<'_>) {
        let s = match self {
            MatterStackKey::Fabrics => "fabrics",
            MatterStackKey::BasicInfo => "basic-info",
            MatterStackKey::Networks => "networks",
        };

        defmt::write!(f, "{}", s)
    }
}

/// A trait representing a key-value BLOB storage.
pub trait KvBlobStore {
    /// Load a BLOB with the specified key from the storage.
    ///
    /// # Arguments
    /// - `key` - the key of the BLOB
    /// - `buf` - a buffer that the `KvBlobStore` implementation might use for its own purposes
    /// - `cb` - a callback that will be called with the loaded data is available
    ///   or with `None` if the BLOB does not exist.
    async fn load<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>;

    /// Store a BLOB with the specified key in the storage.
    ///
    /// # Arguments
    /// - `key` - the key of the BLOB
    /// - `buf` - a buffer that the `KvBlobStore` implementation might use for its own purposes
    /// - `cb` - a callback that will be called with a buffer that the implementation
    ///   should fill with the data to be stored.
    async fn store<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>;

    /// Remove a BLOB with the specified key from the storage.
    ///
    /// # Arguments
    /// - `key` - the key of the BLOB
    /// - `buf` - a buffer that the `KvBlobStore` implementation might use for its own purposes
    async fn remove(&mut self, key: u16, buf: &mut [u8]) -> Result<(), Error>;
}

impl<T> KvBlobStore for &mut T
where
    T: KvBlobStore,
{
    async fn load<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        T::load(self, key, buf, cb).await
    }

    async fn store<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        T::store(self, key, buf, cb).await
    }

    async fn remove(&mut self, key: u16, buf: &mut [u8]) -> Result<(), Error> {
        T::remove(self, key, buf).await
    }
}

/// A noop implementation of the `KvBlobStore` trait.
pub struct DummyKvBlobStore;

impl KvBlobStore for DummyKvBlobStore {
    async fn load<F>(&mut self, _key: u16, _buf: &mut [u8], _cb: F) -> Result<(), Error>
    where
        F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
    {
        Ok(())
    }

    async fn store<F>(&mut self, _key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, Error>,
    {
        // Need to call the callback even if we are not storing for real, so as
        // the callback can mark its internal state as "stored".
        cb(buf)?;

        Ok(())
    }

    async fn remove(&mut self, _key: u16, _buf: &mut [u8]) -> Result<(), Error> {
        Ok(())
    }
}

/// A shared wrapper around a `KvBlobStore` instance.
pub struct SharedKvBlobStore<'a, S> {
    store: IfMutex<NoopRawMutex, S>,
    buf: &'a PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
}

impl<'a, S> SharedKvBlobStore<'a, S>
where
    S: KvBlobStore,
{
    /// Create a new `SharedKvBlobStore` instance.
    ///
    /// # Arguments
    /// - `store` - the wrapped `KvBlobStore` instance
    /// - `buf` - the wrapped buffer
    pub const fn new(store: S, buf: &'a PooledBuffers<1, NoopRawMutex, KvBlobBuffer>) -> Self {
        Self {
            store: IfMutex::new(store),
            buf,
        }
    }

    /// Get the wrapped `KvBlobStore` instance and the wrapped buffer.
    ///
    /// If necessary, awaits the buffer to be available.
    pub async fn get(
        &self,
    ) -> (
        IfMutexGuard<'_, NoopRawMutex, S>,
        PooledBuffer<'_, 1, NoopRawMutex, KvBlobBuffer>,
    ) {
        let store = self.store.lock().await;
        let mut buf = unwrap!(self.buf.get().await);

        unwrap!(buf.resize_default(KV_BLOB_BUF_SIZE));

        (store, buf)
    }
}

#[cfg(feature = "std")]
mod file {
    use std::io::{Read, Write};

    use rs_matter::error::Error;

    use super::KvBlobStore;

    extern crate std;

    /// An implementation of the `KvBlobStore` trait that stores the BLOBs in a directory.
    ///
    /// The BLOBs are stored in files named after the keys in the specified directory.
    #[derive(Debug, Clone)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    pub struct DirKvBlobStore(
        #[cfg_attr(feature = "defmt", defmt(Debug2Format))] std::path::PathBuf,
    );

    impl DirKvBlobStore {
        /// Create a new `DirKvStore` instance, which will persist
        /// its settings in `<tmp-dir>/rs-matter`.
        pub fn new_default() -> Self {
            Self(std::env::temp_dir().join("rs-matter"))
        }

        /// Create a new `DirKvStore` instance.
        pub const fn new(path: std::path::PathBuf) -> Self {
            Self(path)
        }

        /// Load a BLOB with the specified key from the directory.
        pub fn load<'a>(&self, key: u16, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
            let path = self.key_path(key);

            match std::fs::File::open(path) {
                Ok(mut file) => {
                    let mut offset = 0;

                    loop {
                        if offset == buf.len() {
                            Err(rs_matter::error::ErrorCode::NoSpace)?;
                        }

                        let len = file.read(&mut buf[offset..])?;

                        if len == 0 {
                            break;
                        }

                        offset += len;
                    }

                    let data = &buf[..offset];

                    debug!("Key {}: loaded {}B ({:?})", key, data.len(), data);

                    Ok(Some(data))
                }
                Err(_) => Ok(None),
            }
        }

        /// Store a BLOB with the specified key in the directory.
        pub fn store(&self, key: u16, data: &[u8]) -> Result<(), Error> {
            let path = self.key_path(key);

            std::fs::create_dir_all(unwrap!(path.parent()))?;

            let mut file = std::fs::File::create(path)?;

            file.write_all(data)?;

            debug!("Key {}: stored {}B ({:?})", key, data.len(), data);

            Ok(())
        }

        /// Remove a BLOB with the specified key from the directory.
        /// If the BLOB does not exist, this method does nothing.
        pub fn remove(&self, key: u16) -> Result<(), Error> {
            let path = self.key_path(key);

            if std::fs::remove_file(path).is_ok() {
                debug!("Key {}: removed", key);
            }

            Ok(())
        }

        fn key_path(&self, key: u16) -> std::path::PathBuf {
            self.0.join(format!("k_{key:04x}"))
        }
    }

    impl Default for DirKvBlobStore {
        fn default() -> Self {
            Self::new_default()
        }
    }

    impl KvBlobStore for DirKvBlobStore {
        async fn load<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
        where
            F: FnOnce(Option<&[u8]>) -> Result<(), Error>,
        {
            let data = DirKvBlobStore::load(self, key, buf)?;

            cb(data)
        }

        async fn store<F>(&mut self, key: u16, buf: &mut [u8], cb: F) -> Result<(), Error>
        where
            F: FnOnce(&mut [u8]) -> Result<usize, Error>,
        {
            let data_len = cb(buf)?;

            DirKvBlobStore::store(self, key, &buf[..data_len])
        }

        async fn remove(&mut self, key: u16, _buf: &mut [u8]) -> Result<(), Error> {
            DirKvBlobStore::remove(self, key)
        }
    }
}

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
        const KV_BLOB_BUF_SIZE: usize = 4096;
    }
}

/// A buffer for the `KvBlobStore` trait.
pub type KvBlobBuffer = heapless::Vec<u8, KV_BLOB_BUF_SIZE>;

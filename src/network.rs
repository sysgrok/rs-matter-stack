use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::utils::init::{init_from_closure, Init};

use crate::persist::NetworkPersist;
use crate::private::Sealed;

/// User data that can be embedded in the stack network
pub trait Embedding {
    const INIT: Self;

    fn init() -> impl Init<Self>;
}

impl Embedding for () {
    const INIT: Self = ();

    fn init() -> impl Init<Self> {
        unsafe { init_from_closure(|_| Ok(())) }
    }
}

/// A trait modeling a specific network type.
/// `MatterStack` is parameterized by a network type implementing this trait.
///
/// The trait is sealed and has only two implementations: `Eth` and `WirelessBle`.
pub trait Network: Sealed {
    const INIT: Self;

    /// The network peristence context to be used by the `Persist` trait.
    type PersistContext<'a>: NetworkPersist
    where
        Self: 'a;

    /// Optional additional state embedded in the network state
    type Embedding: Embedding + 'static;

    /// Return an in-place initializer for the network type.
    fn init() -> impl Init<Self>;

    /// Return the discovery capabilities of this network when commissioning the device.
    fn discovery_capabilities(&self) -> DiscoveryCapabilities;

    /// Return the persistence context for this network.
    fn persist_context(&self) -> Self::PersistContext<'_>;

    /// Return a reference to the embedded user data.
    fn embedding(&self) -> &Self::Embedding;
}

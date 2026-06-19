use rs_matter::dm::clusters::net_comm::Networks as RsNetworks;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::utils::init::{init_from_closure, Init};

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

    /// Optional additional state embedded in the network state
    type Embedding<'a>: Embedding
    where
        Self: 'a;

    /// The `rs-matter` networks store type owned by the stack's
    /// `InteractionModelState` for this network kind.
    ///
    /// For Ethernet (and other transports where the Matter stack does not manage
    /// network credentials) this is a no-op store; for the wireless (BLE+Wifi/Thread)
    /// network it is a `WirelessNetworks` store.
    type Networks: RsNetworks;

    /// A const initializer for the `rs-matter` networks store (used by the
    /// `const` `MatterStack::new` constructor).
    const NETWORKS: Self::Networks;

    /// Return an in-place initializer for the network type.
    fn init() -> impl Init<Self>;

    /// Return an in-place initializer for the `rs-matter` networks store.
    fn init_networks() -> impl Init<Self::Networks>;

    /// Return the discovery capabilities of this network when commissioning the device.
    fn discovery_capabilities(&self) -> DiscoveryCapabilities;

    /// Return a reference to the embedded user data.
    fn embedding(&self) -> &Self::Embedding<'_>;
}

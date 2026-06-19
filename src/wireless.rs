use core::marker::PhantomData;
use core::pin::pin;

use embassy_futures::select::select3;

use rs_matter::crypto::Crypto;
use rs_matter::dm::clusters::gen_diag::NetifDiag;
use rs_matter::dm::clusters::net_comm::{self, NetCtlError, NetworkType, WirelessCreds};
use rs_matter::dm::clusters::wifi_diag::WirelessDiag;
use rs_matter::dm::clusters::{thread_diag, wifi_diag};
use rs_matter::dm::networks::wireless::{NetCtlState, WirelessNetwork, WirelessNetworks};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::KvBlobStore;
use rs_matter::transport::network::btp::{AdvData, Btp};
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::sync::blocking;
use rs_matter::utils::sync::DynBase;

use crate::ble::GattPeripheral;
use crate::mdns::Mdns;
use crate::nal::NetStack;
use crate::network::{Embedding, Network};
use crate::private::Sealed;
use crate::{pin_alloc, DummyAttrNotifier, MatterStack};

pub use gatt::*;
pub use thread::*;
pub use wifi::*;

mod gatt;
mod thread;
mod wifi;

pub const MAX_WIRELESS_NETWORKS: usize = 2;

/// A type alias for a Matter stack running over either Wifi or Thread (and BLE, during commissioning).
pub type WirelessMatterStack<'a, const B: usize, T, E = ()> = MatterStack<'a, B, WirelessBle<T, E>>;

/// An implementation of the `Network` trait for a Matter stack running over
/// BLE during commissioning, and then over either WiFi or Thread when operating.
///
/// The supported commissioning is either concurrent or non-concurrent (as per the Matter Core spec),
/// where one over the other is decided at runtime with the concrete wireless implementation
/// (`WirelessCoex` or `Wireless` + `Gatt`).
///
/// Non-concurrent commissioning means that the device - at any point in time - either runs Bluetooth
/// or Wifi/Thread, but not both.
///
/// This is done to save memory and to avoid the usage of BLE+Wifi/Thread co-exist drivers on
/// devices which share a single wireless radio for both BLE and Wifi/Thread.
pub struct WirelessBle<T, E = ()>
where
    T: WirelessNetwork,
{
    btp: Btp,
    net_state: blocking::Mutex<RefCell<NetCtlState>>,
    embedding: E,
    // The wireless network type is no longer stored here (the networks store lives
    // in the stack's `InteractionModelState`), but it still parameterizes the
    // `Network::Networks` associated type, so keep it as a phantom marker.
    _network: PhantomData<fn() -> T>,
}

impl<T, E> WirelessBle<T, E>
where
    T: WirelessNetwork,
    E: Embedding,
{
    /// Creates a new instance of the `WirelessBle` network type.
    pub const fn new() -> Self {
        Self {
            btp: Btp::new(),
            net_state: NetCtlState::new_with_mutex(),
            embedding: E::INIT,
            _network: PhantomData,
        }
    }

    /// Return an in-place initializer for the `WirelessBle` network type.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            btp <- Btp::init(),
            net_state <- NetCtlState::init_with_mutex(),
            embedding <- E::init(),
            _network: PhantomData,
        })
    }
}

/// A composite wireless network controller that is the SAME concrete type during
/// both the (BLE) commissioning phase and the operational (Thread/Wifi) phase.
///
/// This exists purely to avoid building two structurally-different Matter handler
/// chains per device. `InteractionModel::handle` and every generated cluster handler
/// adaptor are monomorphized over the whole handler-chain tuple type; if the
/// net-ctl slot has a different type per phase, the entire dispatch tree is
/// compiled twice (~tens of KiB of duplicated `.text` on embedded targets).
///
/// By using `WirelessNetCtl<Q>` in *both* phases — `Commissioning` before the
/// operational controller exists, `Operational(&Q)` afterwards — the chain type
/// is identical across phases and the dispatch tree monomorphizes once.
///
/// The `Commissioning` variant reproduces the behavior of the former
/// `NoopWirelessNetCtl`: `scan` errors with `InvalidAction`, `connect` only
/// checks the creds match the network type, and every diag returns its default.
/// The `Operational` variant delegates to the real controller `Q`.
///
/// `Q` is named identically at every phase via the wireless driver's associated
/// net-ctl type (see the `Thread`/`Gatt` driver traits), so even the
/// commissioning phase — which has no controller value — can still name the type.
pub enum WirelessNetCtl<'a, Q> {
    /// Commissioning phase: no operational controller yet (BLE only).
    Commissioning(NetworkType),
    /// Operational phase: delegate to the real controller.
    Operational(&'a Q),
}

impl<Q> net_comm::NetCtl for WirelessNetCtl<'_, Q>
where
    Q: net_comm::NetCtl,
{
    fn net_type(&self) -> NetworkType {
        match self {
            Self::Commissioning(net_type) => *net_type,
            Self::Operational(q) => q.net_type(),
        }
    }

    fn connect_max_time_seconds(&self) -> u8 {
        match self {
            Self::Commissioning(_) => 0,
            Self::Operational(q) => q.connect_max_time_seconds(),
        }
    }

    fn scan_max_time_seconds(&self) -> u8 {
        match self {
            Self::Commissioning(_) => 0,
            Self::Operational(q) => q.scan_max_time_seconds(),
        }
    }

    fn supported_wifi_bands<F>(&self, f: F) -> Result<(), Error>
    where
        F: FnMut(net_comm::WiFiBandEnum) -> Result<(), Error>,
    {
        match self {
            Self::Commissioning(_) => Ok(()),
            Self::Operational(q) => q.supported_wifi_bands(f),
        }
    }

    fn supported_thread_features(&self) -> net_comm::ThreadCapabilitiesBitmap {
        match self {
            Self::Commissioning(_) => net_comm::ThreadCapabilitiesBitmap::empty(),
            Self::Operational(q) => q.supported_thread_features(),
        }
    }

    fn thread_version(&self) -> u16 {
        match self {
            Self::Commissioning(_) => 0,
            Self::Operational(q) => q.thread_version(),
        }
    }

    async fn scan<F>(&self, network: Option<&[u8]>, f: F) -> Result<(), NetCtlError>
    where
        F: FnMut(&net_comm::NetworkScanInfo) -> Result<(), Error>,
    {
        match self {
            // Matches the former `NoopWirelessNetCtl::scan`.
            Self::Commissioning(_) => Err(NetCtlError::Other(
                rs_matter::error::ErrorCode::InvalidAction.into(),
            )),
            Self::Operational(q) => q.scan(network, f).await,
        }
    }

    async fn connect(&self, creds: &WirelessCreds<'_>) -> Result<(), NetCtlError> {
        match self {
            // Matches the former `NoopWirelessNetCtl::connect`.
            Self::Commissioning(net_type) => Ok(creds.check_match(*net_type)?),
            Self::Operational(q) => q.connect(creds).await,
        }
    }
}

impl<Q> NetChangeNotif for WirelessNetCtl<'_, Q>
where
    Q: NetChangeNotif,
{
    async fn wait_changed(&self) {
        match self {
            Self::Commissioning(_) => core::future::pending().await,
            Self::Operational(q) => q.wait_changed().await,
        }
    }
}

#[cfg(feature = "sync-mutex")]
impl<Q> DynBase for WirelessNetCtl<'_, Q> where Q: Send + Sync {}

#[cfg(not(feature = "sync-mutex"))]
impl<Q> DynBase for WirelessNetCtl<'_, Q> {}

impl<Q> WirelessDiag for WirelessNetCtl<'_, Q>
where
    Q: WirelessDiag,
{
    fn connected(&self) -> Result<bool, Error> {
        match self {
            Self::Commissioning(_) => Ok(false),
            Self::Operational(q) => q.connected(),
        }
    }
}

// For `WifiDiag`/`ThreadDiag`, the `Commissioning` variant reproduces each
// method's trait default (matching the former `NoopWirelessNetCtl`, which impl'd
// both traits empty), and the `Operational` variant delegates to the real
// controller. The defaults are: `Ok(None)` for the scalar accessors, `Ok(())` /
// `f(None)` for the closure-based accessors, and `Nullable::none()` for the
// `WifiDiag` nullable accessors — kept in sync with rs-matter's trait defaults.
impl<Q> wifi_diag::WifiDiag for WirelessNetCtl<'_, Q>
where
    Q: wifi_diag::WifiDiag,
{
    fn bssid(&self, f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => f(None),
            Self::Operational(q) => q.bssid(f),
        }
    }

    fn security_type(
        &self,
    ) -> Result<rs_matter::tlv::Nullable<wifi_diag::SecurityTypeEnum>, Error> {
        match self {
            Self::Commissioning(_) => Ok(rs_matter::tlv::Nullable::none()),
            Self::Operational(q) => q.security_type(),
        }
    }

    fn wi_fi_version(&self) -> Result<rs_matter::tlv::Nullable<wifi_diag::WiFiVersionEnum>, Error> {
        match self {
            Self::Commissioning(_) => Ok(rs_matter::tlv::Nullable::none()),
            Self::Operational(q) => q.wi_fi_version(),
        }
    }

    fn channel_number(&self) -> Result<rs_matter::tlv::Nullable<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(rs_matter::tlv::Nullable::none()),
            Self::Operational(q) => q.channel_number(),
        }
    }

    fn rssi(&self) -> Result<rs_matter::tlv::Nullable<i8>, Error> {
        match self {
            Self::Commissioning(_) => Ok(rs_matter::tlv::Nullable::none()),
            Self::Operational(q) => q.rssi(),
        }
    }
}

impl<Q> thread_diag::ThreadDiag for WirelessNetCtl<'_, Q>
where
    Q: thread_diag::ThreadDiag,
{
    fn channel(&self) -> Result<Option<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.channel(),
        }
    }
    fn routing_role(&self) -> Result<Option<thread_diag::RoutingRoleEnum>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.routing_role(),
        }
    }
    fn network_name(
        &self,
        f: &mut dyn FnMut(Option<&str>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => f(None),
            Self::Operational(q) => q.network_name(f),
        }
    }
    fn pan_id(&self) -> Result<Option<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.pan_id(),
        }
    }
    fn extended_pan_id(&self) -> Result<Option<u64>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.extended_pan_id(),
        }
    }
    fn mesh_local_prefix(
        &self,
        f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => f(None),
            Self::Operational(q) => q.mesh_local_prefix(f),
        }
    }
    fn neighbor_table(
        &self,
        f: &mut dyn FnMut(&thread_diag::NeighborTable) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => Ok(()),
            Self::Operational(q) => q.neighbor_table(f),
        }
    }
    fn route_table(
        &self,
        f: &mut dyn FnMut(&thread_diag::RouteTable) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => Ok(()),
            Self::Operational(q) => q.route_table(f),
        }
    }
    fn partition_id(&self) -> Result<Option<u32>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.partition_id(),
        }
    }
    fn weighting(&self) -> Result<Option<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.weighting(),
        }
    }
    fn data_version(&self) -> Result<Option<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.data_version(),
        }
    }
    fn stable_data_version(&self) -> Result<Option<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.stable_data_version(),
        }
    }
    fn leader_router_id(&self) -> Result<Option<u8>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.leader_router_id(),
        }
    }
    fn ext_address(&self) -> Result<Option<u64>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.ext_address(),
        }
    }
    fn rloc_16(&self) -> Result<Option<u16>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.rloc_16(),
        }
    }
    fn security_policy(&self) -> Result<Option<thread_diag::SecurityPolicy>, Error> {
        match self {
            Self::Commissioning(_) => Ok(None),
            Self::Operational(q) => q.security_policy(),
        }
    }
    fn channel_page0_mask(
        &self,
        f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => f(None),
            Self::Operational(q) => q.channel_page0_mask(f),
        }
    }
    fn operational_dataset_components(
        &self,
        f: &mut dyn FnMut(Option<&thread_diag::OperationalDatasetComponents>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => f(None),
            Self::Operational(q) => q.operational_dataset_components(f),
        }
    }
    fn active_network_faults_list(
        &self,
        f: &mut dyn FnMut(thread_diag::NetworkFaultEnum) -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self {
            Self::Commissioning(_) => Ok(()),
            Self::Operational(q) => q.active_network_faults_list(f),
        }
    }
}

impl<T, E> Default for WirelessBle<T, E>
where
    T: WirelessNetwork,
    E: Embedding,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<T, E> Sealed for WirelessBle<T, E>
where
    T: WirelessNetwork,
    E: Embedding,
{
}

impl<T, E> Network for WirelessBle<T, E>
where
    T: WirelessNetwork,
    E: Embedding,
{
    const INIT: Self = Self::new();

    type Embedding<'a>
        = E
    where
        Self: 'a;

    // The wireless networks store, owned by the stack's `InteractionModelState`.
    type Networks = WirelessNetworks<MAX_WIRELESS_NETWORKS, T>;

    const NETWORKS: Self::Networks = WirelessNetworks::new();

    fn init() -> impl Init<Self> {
        WirelessBle::init()
    }

    fn init_networks() -> impl Init<Self::Networks> {
        WirelessNetworks::init()
    }

    fn discovery_capabilities(&self) -> DiscoveryCapabilities {
        DiscoveryCapabilities::BLE
    }

    fn embedding(&self) -> &Self::Embedding<'_> {
        &self.embedding
    }
}

impl<const B: usize, T, E> MatterStack<'_, B, WirelessBle<T, E>>
where
    T: WirelessNetwork,
    E: Embedding,
{
    /// Reset the Matter instance to the factory defaults by removing all fabrics and basic info settings
    pub async fn reset<S>(&mut self, store: S) -> Result<(), Error>
    where
        S: KvBlobStore,
    {
        let kv = self.matter.kv(store);

        self.matter.reset_persist(&kv).await?;

        // Reset the events counter and the wireless networks store so we don't
        // carry stale state across a factory reset (Matter Core spec R1.5.1,
        // §7.14.1.1 for the events watermark; the networks store holds the
        // commissioned Wifi/Thread credentials).
        self.state.reset_persist(&kv).await?;

        Ok(())
    }

    /// Load the persisted state from the provided `KvBlobStore` implementation.
    pub async fn load<S>(&mut self, store: S) -> Result<(), Error>
    where
        S: KvBlobStore,
    {
        let kv = self.matter.kv(store);

        self.matter.load_persist(&kv).await?;

        // Restore the events counter (so EventNumber stays monotonic across
        // restarts - Matter Core spec R1.5.1, §7.14.1.1 SHALL) and the wireless
        // networks store, both in one call.
        self.state.load_persist(&kv).await?;

        Ok(())
    }

    /// Run the startup sequence of the stack, which includes loading the persisted state
    /// and opening the basic communication window if the device is not commissioned yet.
    pub async fn startup<C, S>(&mut self, crypto: C, kv: S) -> Result<(), Error>
    where
        C: Crypto,
        S: KvBlobStore,
    {
        self.load(kv).await?;

        if !self.is_commissioned() {
            info!("Device is not commissioned yet, opening commissioning window...");

            self.open_basic_comm_window(crypto, &DummyAttrNotifier)?;
        } else {
            info!("Device is already commissioned");
        }

        Ok(())
    }

    /// Run the concurrent (BLE + Wireless) commissioning transport.
    ///
    /// The operational wireless connection manager is no longer run here; it is
    /// driven by the data model engine (`InteractionModel::run`), which was built
    /// with the operational `net_ctl` and the stack's networks store. This method
    /// therefore only runs the BTP coexistence transport.
    async fn run_net_coex<C, S, N, D, G>(
        &self,
        crypto: C,
        net_stack: S,
        netif: N,
        mut mdns: D,
        mut gatt: G,
    ) -> Result<(), Error>
    where
        C: Crypto,
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        D: Mdns,
        G: GattPeripheral,
    {
        self.run_btp_coex(&crypto, &net_stack, &netif, &mut mdns, &mut gatt)
            .await
    }

    async fn run_btp_coex<C, S, N, D, P>(
        &self,
        crypto: C,
        net_stack: S,
        netif: N,
        mut mdns: D,
        mut peripheral: P,
    ) -> Result<(), Error>
    where
        C: Crypto,
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        D: Mdns,
        P: GattPeripheral,
    {
        info!("BLE driver started");

        info!("Running in concurrent commissioning mode (BLE and Wireless)");

        let adv_data = AdvData::new(
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        );

        let mut btp_task = pin_alloc!(
            self.bump,
            peripheral.run(&self.network.btp, "BT", &adv_data)
        );

        let mut net_task = pin_alloc!(
            self.bump,
            self.run_oper_net(
                &crypto,
                &net_stack,
                0, // TODO
                core::future::pending(),
                Some((&self.network.btp, &self.network.btp))
            )
        );

        let mut mdns_task = pin_alloc!(
            self.bump,
            self.run_oper_netif_mdns(&crypto, &net_stack, &netif, &mut mdns)
        );

        select3(&mut btp_task, &mut net_task, &mut mdns_task)
            .coalesce()
            .await
    }

    async fn run_btp<C, P>(&self, crypto: C, mut peripheral: P) -> Result<(), Error>
    where
        C: Crypto,
        P: GattPeripheral,
    {
        info!("BLE driver started");

        info!("Running in non-concurrent commissioning mode (BLE only)");

        let adv_data = AdvData::new(
            self.matter().dev_det(),
            self.matter().dev_comm().discriminator,
        );

        let mut btp_task = pin_alloc!(
            self.bump,
            peripheral.run(&self.network.btp, "BT", &adv_data)
        );

        let mut net_task =
            pin!(self.run_transport_net(&crypto, &self.network.btp, &self.network.btp, NoNetwork));
        let mut oper_net_act_task = pin!(async {
            NetCtlState::wait_prov_ready(&self.network.net_state, &self.network.btp).await;

            // TODO: Workaround for a bug in the `esp-wifi` BLE stack:
            // ====================== PANIC ======================
            // panicked at /home/ivan/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/esp-wifi-0.12.0/src/ble/npl.rs:914:9:
            // timed eventq_get not yet supported - go implement it!
            embassy_time::Timer::after(embassy_time::Duration::from_secs(2)).await;

            Ok(())
        });

        select3(&mut btp_task, &mut net_task, &mut oper_net_act_task)
            .coalesce()
            .await
    }
}

/// A utility type for running a wireless task with a pre-existing wireless interface
/// rather than bringing up / tearing down the wireless interface for the task.
///
/// This utility can only be used with hardware that implements wireless coexist mode
/// (i.e. the Thread/Wifi interface as well as the BLE GATT peripheral are available at the same time).
pub struct PreexistingWireless<S, N, C, M, G> {
    pub(crate) net_stack: S,
    pub(crate) netif: N,
    pub(crate) net_ctl: C,
    pub(crate) mdns: M,
    pub(crate) gatt: G,
}

impl<S, N, C, M, G> PreexistingWireless<S, N, C, M, G> {
    /// Create a new `PreexistingWireless` instance with the given network stack,
    /// network interface, network controller and GATT peripheral.
    pub const fn new(net_stack: S, netif: N, net_ctl: C, mdns: M, gatt: G) -> Self {
        Self {
            net_stack,
            netif,
            net_ctl,
            mdns,
            gatt,
        }
    }
}

pub(crate) struct MatterStackWirelessTask<'a, const B: usize, T, E, C, H, K, U, Q>
where
    T: WirelessNetwork,
    E: Embedding,
{
    stack: &'a MatterStack<'a, B, WirelessBle<T, E>>,
    crypto: C,
    handler: H,
    kv: K,
    user_task: U,
    _net_ctl: PhantomData<fn() -> Q>,
}

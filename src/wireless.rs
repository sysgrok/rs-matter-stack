use core::borrow::BorrowMut;
use core::pin::pin;

use embassy_futures::select::{select, select3};

use rs_matter::crypto::Crypto;
use rs_matter::dm::clusters::gen_diag::NetifDiag;
use rs_matter::dm::clusters::net_comm::{NetCtl, SharedNetworks};
use rs_matter::dm::clusters::wifi_diag::WirelessDiag;
use rs_matter::dm::networks::wireless::{
    NetCtlState, WirelessMgr, WirelessNetwork, WirelessNetworks, MAX_CREDS_SIZE,
};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::KvBlobStore;
use rs_matter::transport::network::btp::{AdvData, Btp};
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::cell::RefCell;
use rs_matter::utils::init::{init, zeroed, Init};
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::sync::{blocking, IfMutex};

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
    networks: SharedNetworks<WirelessNetworks<MAX_WIRELESS_NETWORKS, T>>,
    net_state: blocking::Mutex<RefCell<NetCtlState>>,
    creds_buf: IfMutex<[u8; MAX_CREDS_SIZE]>,
    embedding: E,
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
            networks: SharedNetworks::new(WirelessNetworks::new()),
            net_state: NetCtlState::new_with_mutex(),
            creds_buf: IfMutex::new([0; MAX_CREDS_SIZE]),
            embedding: E::INIT,
        }
    }

    /// Return an in-place initializer for the `WirelessBle` network type.
    pub fn init() -> impl Init<Self> {
        init!(Self {
            btp <- Btp::init(),
            networks <- SharedNetworks::init(WirelessNetworks::init()),
            net_state <- NetCtlState::init_with_mutex(),
            creds_buf <- IfMutex::init(zeroed()),
            embedding <- E::init(),
        })
    }

    /// Return a reference to the networks storage.
    pub fn networks(&self) -> &SharedNetworks<WirelessNetworks<MAX_WIRELESS_NETWORKS, T>> {
        &self.networks
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

    fn init() -> impl Init<Self> {
        WirelessBle::init()
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
    pub async fn reset<S>(&mut self, mut kv: S) -> Result<(), Error>
    where
        S: KvBlobStore,
    {
        let mut buf = unwrap!(self.store_buf.try_get());
        let buf = buf.borrow_mut();

        self.matter.reset_persist(&mut kv, buf).await?;

        self.network
            .networks
            .get_mut()
            .get_mut()
            .reset_persist(kv, buf)
            .await
    }

    /// Load the persisted state from the provided `KvBlobStore` implementation.
    pub async fn load<S>(&mut self, mut kv: S) -> Result<(), Error>
    where
        S: KvBlobStore,
    {
        let mut buf = unwrap!(self.store_buf.try_get());
        let buf = buf.borrow_mut();

        self.matter.load_persist(&mut kv, buf).await?;

        self.network
            .networks
            .get_mut()
            .get_mut()
            .load_persist(kv, buf)
            .await
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

    #[allow(clippy::too_many_arguments)]
    async fn run_net_coex<C, S, N, D, Q, G>(
        &self,
        crypto: C,
        net_stack: S,
        netif: N,
        net_ctl: Q,
        mut mdns: D,
        mut gatt: G,
    ) -> Result<(), Error>
    where
        C: Crypto,
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        Q: NetCtl + WirelessDiag + NetChangeNotif,
        D: Mdns,
        G: GattPeripheral,
    {
        let mut buf = self.network.creds_buf.lock().await;

        let mut mgr = WirelessMgr::new(&self.network.networks, &net_ctl, &mut buf);

        let mut net_task = pin_alloc!(
            self.bump,
            self.run_btp_coex(&crypto, &net_stack, &netif, &mut mdns, &mut gatt)
        );
        let mut mgr_task = pin_alloc!(self.bump, mgr.run());

        select(&mut net_task, &mut mgr_task).coalesce().await
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

pub(crate) struct MatterStackWirelessTask<'a, const B: usize, T, E, C, H, S, U>
where
    T: WirelessNetwork,
    E: Embedding,
{
    stack: &'a MatterStack<'a, B, WirelessBle<T, E>>,
    crypto: C,
    handler: H,
    kv: S,
    user_task: U,
}

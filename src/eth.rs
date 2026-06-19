use core::future::Future;

use embassy_futures::select::select4;

use rs_matter::crypto::{Crypto, RngCore};
use rs_matter::dm::clusters::gen_comm::CommPolicy;
use rs_matter::dm::clusters::gen_diag::{GenDiag, NetifDiag};
use rs_matter::dm::clusters::net_comm::{DummyNetworks, NetworkType};
use rs_matter::dm::clusters::sw_diag::SwDiag;
use rs_matter::dm::clusters::time_sync::TimeSync;
use rs_matter::dm::endpoints::{eth_sys_handler, EthSysHandler, ROOT_ENDPOINT_ID};
use rs_matter::dm::networks::wireless::NoopWirelessNetCtl;
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::{ChainedHandler, DataModel, Endpoint, EpClMatcher};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::{KvBlobStore, KvBlobStoreAccess};
use rs_matter::root_endpoint;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, init_from_closure, Init};
use rs_matter::utils::select::Coalesce;

use crate::mdns::Mdns;
use crate::nal::NetStack;
use crate::network::{Embedding, Network};
use crate::private::Sealed;
use crate::{pin_alloc, DummyAttrNotifier, MatterStack, UserTask};

/// An implementation of the `Network` trait for Ethernet.
///
/// Note that "Ethernet" - in the context of this crate - means
/// not just the Ethernet transport, but also any other IP-based transport
/// (like Wifi or Thread), where the Matter stack would not be concerned
/// with the management of the network transport (as in re-connecting to the
/// network on lost signal, managing network credentials and so on).
///
/// The expectation is nevertheless that for production use-cases
/// the `Eth` network would really only be used for Ethernet.
pub struct Eth<E = ()> {
    embedding: E,
}

impl<E> Sealed for Eth<E> {}

impl<E> Network for Eth<E>
where
    E: Embedding,
{
    const INIT: Self = Self { embedding: E::INIT };

    type Embedding<'a>
        = E
    where
        E: 'a;

    // Ethernet does not manage network credentials, so use the no-op networks store.
    type Networks = DummyNetworks;

    const NETWORKS: Self::Networks = DummyNetworks;

    fn init() -> impl Init<Self> {
        init!(Self {
            embedding <- E::init(),
        })
    }

    fn init_networks() -> impl Init<Self::Networks> {
        unsafe {
            init_from_closure(|slot: *mut DummyNetworks| {
                slot.write(DummyNetworks);
                Ok(())
            })
        }
    }

    fn discovery_capabilities(&self) -> DiscoveryCapabilities {
        DiscoveryCapabilities::IP
    }

    fn embedding(&self) -> &Self::Embedding<'_> {
        &self.embedding
    }
}

// A type alias for a Matter stack running over Ethernet.
pub type EthMatterStack<'a, const B: usize, E = ()> = MatterStack<'a, B, Eth<E>>;

/// A trait representing a task that needs access to the operational Ethernet interface
/// (Network stack and Netif) to perform its work.
pub trait EthernetTask {
    /// Run the task with the given network stack, network interface and mDNS
    async fn run<S, N, M>(&mut self, net_stack: S, netif: N, mdns: M) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        M: Mdns;
}

impl<T> EthernetTask for &mut T
where
    T: EthernetTask,
{
    fn run<S, N, M>(
        &mut self,
        net_stack: S,
        netif: N,
        mdns: M,
    ) -> impl Future<Output = Result<(), Error>>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        M: Mdns,
    {
        (*self).run(net_stack, netif, mdns)
    }
}

/// A trait for running a task within a context where the ethernet interface is initialized and operable
pub trait Ethernet {
    /// Setup Ethernet and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: EthernetTask;
}

impl<T> Ethernet for &mut T
where
    T: Ethernet,
{
    fn run<A>(&mut self, task: A) -> impl Future<Output = Result<(), Error>>
    where
        A: EthernetTask,
    {
        (*self).run(task)
    }
}

/// A utility type for running an ethernet task with a pre-existing ethernet interface
/// rather than bringing up / tearing down the ethernet interface for the task.
pub struct PreexistingEthernet<S, N, M> {
    stack: S,
    netif: N,
    mdns: M,
}

impl<S, N, M> PreexistingEthernet<S, N, M> {
    /// Create a new `PreexistingEthernet` instance with the given network interface, UDP stack and mDNS.
    pub const fn new(stack: S, netif: N, mdns: M) -> Self {
        Self { stack, netif, mdns }
    }
}

impl<S, N, M> Ethernet for PreexistingEthernet<S, N, M>
where
    S: NetStack,
    N: NetifDiag + NetChangeNotif,
    M: Mdns,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: EthernetTask,
    {
        task.run(&self.stack, &self.netif, &mut self.mdns).await
    }
}

/// A specialization of the `MatterStack` for Ethernet.
impl<const B: usize, E> MatterStack<'_, B, Eth<E>>
where
    E: Embedding,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub const fn root_endpoint() -> Endpoint<'static> {
        const ENDPOINT: Endpoint<'static> = root_endpoint!(eth);

        ENDPOINT
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    fn root_handler<'a>(
        &self,
        comm_policy: &'a dyn CommPolicy,
        gen_diag: &'a dyn GenDiag,
        netif_diag: &'a dyn NetifDiag,
        time_sync: &'a dyn TimeSync,
        sw_diag: &'a dyn SwDiag,
        rand: impl RngCore + Copy,
    ) -> EthSysHandler<'a> {
        eth_sys_handler(comm_policy, gen_diag, netif_diag, time_sync, sw_diag, rand)
    }

    /// Reset the Matter instance to the factory defaults by removing all fabrics and basic info settings
    pub async fn reset<S>(&mut self, store: S) -> Result<(), Error>
    where
        S: KvBlobStore,
    {
        let kv = self.matter.kv(store);

        self.matter.reset_persist(&kv).await?;

        // Reset the events counter (and the - no-op for Ethernet - networks store)
        // so we don't carry a stale watermark across a factory reset
        // (Matter Core spec R1.5.1, §7.14.1.1).
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

        // Restore the events counter (and the - no-op for Ethernet - networks
        // store) so EventNumber stays monotonic across restarts
        // (Matter Core spec R1.5.1, §7.14.1.1 SHALL).
        self.state.load_persist(&kv).await?;

        Ok(())
    }

    /// Run the startup sequence of the stack, which includes loading the persisted state
    /// and opening the basic communication window if the device is not commissioned yet.
    pub async fn startup<C, S>(&mut self, crypto: C, store: S) -> Result<(), Error>
    where
        C: Crypto,
        S: KvBlobStore,
    {
        self.load(store).await?;

        if !self.is_commissioned() {
            info!("Device is not commissioned yet, opening commissioning window...");

            self.open_basic_comm_window(crypto, &DummyAttrNotifier)?;
        } else {
            info!("Device is already commissioned");
        }

        Ok(())
    }

    /// Run the Matter stack for a pre-existing Ethernet network.
    ///
    /// # Arguments
    /// - `net_stack` - a user-provided network stack implementation
    /// - `netif` - a user-provided `Netif` implementation for the Ethernet network
    /// - `mdns` - a user-provided mDNS implementation
    /// - `crypto` - a user-provided crypto implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `kv` - a user-provided `KvBlobStoreAccess` implementation for loading the persisted state of the stack
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    #[allow(clippy::too_many_arguments)]
    pub fn run_preex<'t, U, N, M, C, H, K, X>(
        &'t self,
        net_stack: U,
        netif: N,
        mdns: M,
        crypto: C,
        handler: H,
        kv: K,
        user: X,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        U: NetStack + 't,
        N: NetifDiag + NetChangeNotif + 't,
        M: Mdns + 't,
        C: Crypto + 't,
        H: DataModel + 't,
        K: KvBlobStoreAccess + 't,
        X: UserTask + 't,
    {
        self.run(
            PreexistingEthernet::new(net_stack, netif, mdns),
            crypto,
            handler,
            kv,
            user,
        )
    }

    /// Run the Matter stack for an Ethernet network.
    ///
    /// # Arguments
    /// - `ethernet` - a user-provided `Ethernet` implementation
    /// - `crypto` - a user-provided crypto implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `kv` - a user-provided `KvBlobStoreAccess` implementation for loading the persisted state of the stack
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<N, C, H, K, X>(
        &self,
        mut ethernet: N,
        crypto: C,
        handler: H,
        kv: K,
        user: X,
    ) -> Result<(), Error>
    where
        N: Ethernet,
        C: Crypto,
        H: DataModel,
        K: KvBlobStoreAccess,
        X: UserTask,
    {
        let _lock = self.run_lock.lock().await;

        info!("Matter Stack memory: {}b", core::mem::size_of_val(self));

        // Since this is the last code executed in the method, resetting the allocator should be safe
        // because all boxes returned by it should be dropped by then
        let _defer = scopeguard::guard((), |_| unsafe {
            self.bump.reset();
        });

        self.matter().reset_transport()?;

        let net_task = pin_alloc!(
            self.bump,
            self.run_ethernet(&mut ethernet, crypto, handler, &kv, user)
        );

        net_task.await
    }

    fn run_ethernet<'t, N, C, H, K, X>(
        &'t self,
        ethernet: &'t mut N,
        crypto: C,
        handler: H,
        kv: K,
        user: X,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        N: Ethernet + 't,
        C: Crypto + 't,
        H: DataModel + 't,
        K: KvBlobStoreAccess + 't,
        X: UserTask + 't,
    {
        Ethernet::run(
            ethernet,
            MatterStackEthernetTask {
                stack: self,
                crypto,
                handler,
                kv,
                user_task: user,
            },
        )
    }
}

struct MatterStackEthernetTask<'a, const B: usize, E, C, H, K, X>
where
    E: Embedding,
    C: Crypto,
    H: DataModel,
    K: KvBlobStoreAccess,
    X: UserTask,
{
    stack: &'a MatterStack<'a, B, Eth<E>>,
    crypto: C,
    handler: H,
    kv: K,
    user_task: X,
}

impl<const B: usize, E, C, H, K, X> EthernetTask for MatterStackEthernetTask<'_, B, E, C, H, K, X>
where
    E: Embedding,
    C: Crypto,
    H: DataModel,
    K: KvBlobStoreAccess,
    X: UserTask,
{
    async fn run<N, I, M>(&mut self, net_stack: N, netif: I, mut mdns: M) -> Result<(), Error>
    where
        N: NetStack,
        I: NetifDiag + NetChangeNotif,
        M: Mdns,
    {
        info!("Ethernet driver started");

        // The sys-handler chain (built per phase so per-phase `NetCtl` /
        // diag implementations can vary) covers every cluster on the
        // root endpoint; route anything on EP0 to it, and any other
        // endpoint to the user's handler. `&self.handler` doubles as
        // the `Metadata` provider (`(M, H)` form for `InteractionModel::new`).
        let sys = self
            .stack
            .root_handler(&false, &(), &netif, &(), &(), self.crypto.weak_rand()?);
        let combined = ChainedHandler::new(
            EpClMatcher::new(Some(ROOT_ENDPOINT_ID), None),
            sys,
            &self.handler,
        );
        // Ethernet does not manage networks, so use the inert wireless net-ctl;
        // the engine's connection-manager branch then stays dormant.
        let im = self.stack.im(
            &self.crypto,
            (&self.handler, combined),
            &self.kv,
            NoopWirelessNetCtl::new(NetworkType::Ethernet),
        );

        let mut net_task = pin_alloc!(
            self.stack.bump,
            self.stack.run_oper_net(
                &self.crypto,
                &net_stack,
                0, // TODO
                core::future::pending(),
                Option::<(NoNetwork, NoNetwork)>::None,
            )
        );

        let mut mdns_task = pin_alloc!(
            self.stack.bump,
            self.stack
                .run_oper_netif_mdns(&self.crypto, &net_stack, &netif, &mut mdns)
        );

        let mut im_task = pin_alloc!(self.stack.bump, self.stack.run_im_with_bump(&im));

        let mut user_task = pin_alloc!(self.stack.bump, self.user_task.run(&net_stack, &netif));

        select4(&mut net_task, &mut mdns_task, &mut im_task, &mut user_task)
            .coalesce()
            .await
    }
}

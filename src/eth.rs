use core::future::Future;

use embassy_futures::select::{select, select4};

use rs_matter::dm::clusters::gen_comm::CommPolicy;
use rs_matter::dm::clusters::gen_diag::{GenDiag, NetifDiag};
use rs_matter::dm::clusters::net_comm::NetworkType;
use rs_matter::dm::endpoints::{self, with_eth, with_sys, EthHandler, SysHandler};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::{AsyncHandler, AsyncMetadata, Endpoint};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;

use crate::mdns::Mdns;
use crate::nal::NetStack;
use crate::network::{Embedding, Network};
use crate::persist::{KvBlobStore, SharedKvBlobStore};
use crate::private::Sealed;
use crate::{pin_alloc, MatterStack, UserTask};

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
    E: Embedding + 'static,
{
    const INIT: Self = Self { embedding: E::INIT };

    type PersistContext<'a> = ();

    type Embedding = E;

    fn persist_context(&self) -> Self::PersistContext<'_> {}

    fn embedding(&self) -> &Self::Embedding {
        &self.embedding
    }

    fn init() -> impl Init<Self> {
        init!(Self {
            embedding <- E::init(),
        })
    }
}

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
    E: Embedding + 'static,
{
    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    pub const fn root_endpoint() -> Endpoint<'static> {
        endpoints::root_endpoint(NetworkType::Ethernet)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for Ethernet network.
    fn root_handler<'a, H>(
        &self,
        gen_diag: &'a dyn GenDiag,
        comm_policy: &'a dyn CommPolicy,
        netif_diag: &'a dyn NetifDiag,
        handler: H,
    ) -> EthHandler<'a, SysHandler<'a, H>> {
        with_eth(
            gen_diag,
            netif_diag,
            self.matter().rand(),
            with_sys(comm_policy, self.matter().rand(), handler),
        )
    }

    /// Reset the Matter instance to the factory defaults putting it into a
    /// Commissionable mode.
    pub fn reset(&self) -> Result<(), Error> {
        // TODO: Reset fabrics and ACLs

        Ok(())
    }

    /// Run the Matter stack for a pre-existing Ethernet network.
    ///
    /// Parameters:
    /// - `netif` - a user-provided `Netif` implementation for the Ethernet network
    /// - `net_stack` - a user-provided network stack implementation
    /// - `mdns` - a user-provided mDNS implementation
    /// - `persist` - a user-provided `Persist` implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub fn run_preex<'t, U, N, M, S, H, X>(
        &'t self,
        net_stack: U,
        netif: N,
        mdns: M,
        store: &'t SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        U: NetStack + 't,
        N: NetifDiag + NetChangeNotif + 't,
        M: Mdns + 't,
        S: KvBlobStore + 't,
        H: AsyncHandler + AsyncMetadata + 't,
        X: UserTask + 't,
    {
        self.run(
            PreexistingEthernet::new(net_stack, netif, mdns),
            store,
            handler,
            user,
        )
    }

    /// Run the Matter stack for an Ethernet network.
    ///
    /// Parameters:
    /// - `ethernet` - a user-provided `Ethernet` implementation
    /// - `persist` - a user-provided `Persist` implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<N, S, H, X>(
        &self,
        mut ethernet: N,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        N: Ethernet,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: UserTask,
    {
        let _lock = self.run_lock.lock().await;

        info!("Matter Stack memory: {}b", core::mem::size_of_val(self));

        // Since this is the last code executed in the method, resetting the allocator should be safe
        // because all boxes returned by it should be dropped by then
        let _defer = scopeguard::guard((), |_| unsafe {
            self.bump.reset();
        });

        let persist = self.create_persist(store);

        persist.load().await?;

        self.matter().reset_transport()?;

        if !self.is_commissioned().await? {
            self.matter()
                .enable_basic_commissioning(DiscoveryCapabilities::IP, 0)
                .await?; // TODO
        }

        let mut net_task = pin_alloc!(self.bump, self.run_ethernet(&mut ethernet, handler, user));
        let mut persist_task = pin_alloc!(self.bump, self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    fn run_ethernet<'t, N, H, X>(
        &'t self,
        ethernet: &'t mut N,
        handler: H,
        user: X,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        N: Ethernet + 't,
        H: AsyncHandler + AsyncMetadata + 't,
        X: UserTask + 't,
    {
        Ethernet::run(ethernet, MatterStackEthernetTask(self, handler, user))
    }
}

struct MatterStackEthernetTask<'a, const B: usize, E, H, X>(&'a MatterStack<'a, B, Eth<E>>, H, X)
where
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask;

impl<const B: usize, E, H, X> EthernetTask for MatterStackEthernetTask<'_, B, E, H, X>
where
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<S, C, M>(&mut self, net_stack: S, netif: C, mut mdns: M) -> Result<(), Error>
    where
        S: NetStack,
        C: NetifDiag + NetChangeNotif,
        M: Mdns,
    {
        info!("Ethernet driver started");

        let handler = self.0.root_handler(&(), &true, &netif, &self.1);

        let mut net_task = pin_alloc!(
            self.0.bump,
            self.0.run_oper_net(
                &net_stack,
                core::future::pending(),
                Option::<(NoNetwork, NoNetwork)>::None,
            )
        );

        let mut mdns_task = pin_alloc!(
            self.0.bump,
            self.0.run_oper_netif_mdns(&net_stack, &netif, &mut mdns)
        );

        let mut handler_task = pin_alloc!(
            self.0.bump,
            self.0.run_handler_with_bump((&self.1, handler))
        );

        let mut user_task = pin_alloc!(self.0.bump, self.2.run(&net_stack, &netif));

        select4(
            &mut net_task,
            &mut mdns_task,
            &mut handler_task,
            &mut user_task,
        )
        .coalesce()
        .await
    }
}

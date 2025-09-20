use core::mem::MaybeUninit;
use core::pin::pin;

use embassy_futures::select::{select, select3, select4};
use embassy_sync::blocking_mutex::raw::RawMutex;

use crate::bump_alloc::BumpAllocator;

use rs_matter::dm::clusters::gen_comm::CommPolicy;
use rs_matter::dm::clusters::gen_diag::GenDiag;
use rs_matter::dm::clusters::net_comm::{NetCtl, NetCtlStatus, NetworkType};
use rs_matter::dm::clusters::thread_diag::ThreadDiag;
use rs_matter::dm::endpoints::{self, with_sys, with_thread, SysHandler, ThreadHandler};
use rs_matter::dm::networks::wireless::{
    self, NetCtlWithStatusImpl, NoopWirelessNetCtl, WirelessMgr,
};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::{clusters::gen_diag::NetifDiag, AsyncHandler};
use rs_matter::dm::{AsyncMetadata, Endpoint};
use rs_matter::error::Error;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::transport::network::btp::GattPeripheral;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::select::Coalesce;

use crate::mdns::Mdns;
use crate::nal::NetStack;
use crate::network::Embedding;
use crate::persist::{KvBlobStore, SharedKvBlobStore};
use crate::wireless::{GattTask, MatterStackWirelessTask, MatterStackWirelessTaskWithMemory};
use crate::UserTask;

use super::{Gatt, PreexistingWireless, WirelessMatterStack};

/// A type alias for a Matter stack running over Thread (and BLE, during commissioning).
pub type ThreadMatterStack<'a, M, E = ()> = WirelessMatterStack<'a, M, wireless::Thread, E>;

impl<M, E> WirelessMatterStack<'_, M, wireless::Thread, E>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
{
    /// Run the Matter stack for an already pre-existing wireless network where the BLE and the operational network can co-exist.
    ///
    /// Parameters:
    /// - `net_stack` - a user-provided `NetStack` implementation
    /// - `netif` - a user-provided `Netif` implementation
    /// - `controller` - a user-provided `Controller` implementation
    /// - `mdns` - a user-provided `Mdns` implementation
    /// - `gatt` - a user-provided `GattPeripheral` implementation
    /// - `store` - a `SharedKvBlobStore` implementation wrapping a user-provided `KvBlobStore`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    #[allow(clippy::too_many_arguments)]
    pub async fn run_preex<U, N, C, D, G, S, H, X>(
        &'static self,
        net_stack: U,
        netif: N,
        net_ctl: C,
        mdns: D,
        gatt: G,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: X,
    ) -> Result<(), Error>
    where
        U: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        D: Mdns,
        G: GattPeripheral,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        X: UserTask,
    {
        self.run_coex(
            PreexistingWireless::new(net_stack, netif, net_ctl, mdns, gatt),
            store,
            handler,
            user,
        )
        .await
    }

    /// Run the Matter stack for a wireless network where the BLE and the operational network can co-exist.
    ///
    /// Parameters:
    /// - `wireless` - a user-provided `Wireless` implementation
    /// - `store` - a `SharedKvBlobStore` implementation wrapping a user-provided `KvBlobStore`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run_coex<W, S, H, U>(
        &'static self,
        thread: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: ThreadCoex,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let persist = self.create_persist(store);

        persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_thread_coex(thread, handler, user));
        let mut persist_task = pin!(self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    /// Run the Matter stack for a wireless network where the BLE and the operational network cannot co-exist.
    ///
    /// Parameters:
    /// - `wireless` - a user-provided `Wireless` + `Gatt` implementation
    /// - `store` - a `SharedKvBlobStore` implementation wrapping a user-provided `KvBlobStore`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<W, S, H, U>(
        &'static self,
        thread: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: Thread + Gatt,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let persist = self.create_persist(store);

        persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_thread(thread, handler, user));
        let mut persist_task = pin!(self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    /// Run the Matter stack for a Thread network with bump allocation.
    /// Uses the provided memory buffer for allocations instead of heap.
    ///
    /// Parameters:
    /// - `thread` - a user-provided Thread implementation
    /// - `store` - a user-provided persistent storage implementation
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    /// - `memory` - a memory buffer for internal allocations (recommended size: 16KB+)
    pub async fn run_with_memory<W, S, H, U>(
        &'static self,
        thread: W,
        store: &SharedKvBlobStore<'_, S>,
        handler: H,
        user: U,
        memory: &mut [MaybeUninit<u8>],
    ) -> Result<(), Error>
    where
        W: Thread + Gatt,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        info!("Matter Stack memory: {}B", core::mem::size_of_val(self));

        let persist = self.create_persist(store);

        persist.load().await?;

        self.matter().reset_transport()?;

        let mut net_task = pin!(self.run_thread_with_memory(thread, handler, user, memory));
        let mut persist_task = pin!(self.run_psm(&persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    async fn run_thread_coex<W, H, U>(
        &'static self,
        mut thread: W,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: ThreadCoex,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        thread
            .run(MatterStackWirelessTask(self, handler, user))
            .await
    }

    async fn run_thread<W, H, U>(
        &'static self,
        mut thread: W,
        handler: H,
        mut user: U,
    ) -> Result<(), Error>
    where
        W: Thread + Gatt,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                self.matter()
                    .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                    .await?; // TODO

                Gatt::run(
                    &mut thread,
                    MatterStackWirelessTask(self, &handler, &mut user),
                )
                .await?;
            }

            if commissioned {
                self.matter().disable_commissioning()?;
            }

            Thread::run(
                &mut thread,
                MatterStackWirelessTask(self, &handler, &mut user),
            )
            .await?;
        }
    }

    async fn run_thread_with_memory<W, H, U>(
        &'static self,
        mut thread: W,
        handler: H,
        mut user: U,
        memory: &mut [MaybeUninit<u8>],
    ) -> Result<(), Error>
    where
        W: Thread + Gatt,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                self.matter()
                    .enable_basic_commissioning(DiscoveryCapabilities::BLE, 0)
                    .await?; // TODO

                Gatt::run(
                    &mut thread,
                    MatterStackWirelessTaskWithMemory(self, &handler, &mut user, memory),
                )
                .await?;
            }

            if commissioned {
                self.matter().disable_commissioning()?;
            }

            Thread::run(
                &mut thread,
                MatterStackWirelessTaskWithMemory(self, &handler, &mut user, memory),
            )
            .await?;
        }
    }

    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Thread network.
    pub const fn root_endpoint() -> Endpoint<'static> {
        endpoints::root_endpoint(NetworkType::Thread)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    fn root_handler<'a, N, H>(
        &'a self,
        gen_diag: &'a dyn GenDiag,
        netif_diag: &'a dyn NetifDiag,
        net_ctl: &'a N,
        comm_policy: &'a dyn CommPolicy,
        handler: H,
    ) -> ThreadHandler<'a, &'a N, SysHandler<'a, H>>
    where
        N: NetCtl + NetCtlStatus + ThreadDiag,
    {
        with_thread(
            gen_diag,
            netif_diag,
            net_ctl,
            &self.network.networks,
            self.matter().rand(),
            with_sys(comm_policy, self.matter().rand(), handler),
        )
    }
}

/// A trait representing a task that needs access to the operational wireless interface (Wifi or Thread)
/// (network stack, Netif and Wireless controller) to perform its work.
pub trait ThreadTask {
    /// Run the task with the given network interface, UDP stack, wireless controller and mDNS
    async fn run<S, N, C, M>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mdns: M,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        M: Mdns;
}

impl<T> ThreadTask for &mut T
where
    T: ThreadTask,
{
    async fn run<S, N, C, M>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mdns: M,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        M: Mdns,
    {
        T::run(*self, net_stack, netif, net_ctl, mdns).await
    }
}

/// A trait for running a task within a context where the wireless interface is initialized and operable
pub trait Thread {
    /// Setup the radio to operate in wireless (Wifi or Thread) mode
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: ThreadTask;
}

impl<T> Thread for &mut T
where
    T: Thread,
{
    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: ThreadTask,
    {
        T::run(self, task).await
    }
}

/// A trait representing a task that needs access to the operational wireless interface (Wifi or Thread)
/// as well as to the commissioning BTP GATT peripheral.
///
/// Typically, tasks performing the Matter concurrent commissioning workflow will implement this trait.
pub trait ThreadCoexTask {
    /// Run the task with the given network stack, network interface, wireless controller and mDNS
    async fn run<S, N, C, M, G>(
        &mut self,
        net_stack: S,
        netif: N,
        net_task: C,
        mdns: M,
        gatt: G,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        M: Mdns,
        G: GattPeripheral;
}

impl<T> ThreadCoexTask for &mut T
where
    T: ThreadCoexTask,
{
    async fn run<S, N, C, M, G>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mdns: M,
        gatt: G,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        M: Mdns,
        G: GattPeripheral,
    {
        T::run(*self, net_stack, netif, net_ctl, mdns, gatt).await
    }
}

/// A trait for running a task within a context where both the wireless interface (Thread or Wifi)
/// is initialized and operable, as well as the BLE GATT peripheral is also operable.
///
/// Typically, tasks performing the Matter concurrent commissioning workflow will ran by implementations
/// of this trait.
pub trait ThreadCoex {
    /// Setup the radio to operate in wireless coexist mode (Wifi or Thread + BLE)
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: ThreadCoexTask;
}

impl<T> ThreadCoex for &mut T
where
    T: ThreadCoex,
{
    async fn run<A>(&mut self, task: A) -> Result<(), Error>
    where
        A: ThreadCoexTask,
    {
        T::run(self, task).await
    }
}

impl<S, N, C, M, P> Thread for PreexistingWireless<S, N, C, M, P>
where
    S: NetStack,
    N: NetifDiag + NetChangeNotif,
    C: NetCtl + ThreadDiag + NetChangeNotif,
    M: Mdns,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: ThreadTask,
    {
        task.run(&self.net_stack, &self.netif, &self.net_ctl, &mut self.mdns)
            .await
    }
}

impl<S, N, C, M, P> ThreadCoex for PreexistingWireless<S, N, C, M, P>
where
    S: NetStack,
    N: NetifDiag + NetChangeNotif,
    C: NetCtl + ThreadDiag + NetChangeNotif,
    M: Mdns,
    P: GattPeripheral,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: ThreadCoexTask,
    {
        task.run(
            &self.net_stack,
            &self.netif,
            &self.net_ctl,
            &mut self.mdns,
            &mut self.gatt,
        )
        .await
    }
}

impl<M, E, H, X> GattTask for MatterStackWirelessTask<'static, M, wireless::Thread, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
{
    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral,
    {
        let net_ctl = NetCtlWithStatusImpl::new(
            &self.0.network.net_state,
            NoopWirelessNetCtl::new(NetworkType::Thread),
        );

        let mut btp_task = pin!(self.0.run_btp(peripheral));

        let handler = self.0.root_handler(&(), &(), &net_ctl, &false, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        select(&mut btp_task, &mut handler_task).coalesce().await
    }
}

impl<M, E, H, X> ThreadTask for MatterStackWirelessTask<'static, M, wireless::Thread, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<S, N, C, D>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mut mdns: D,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        D: Mdns,
    {
        info!("Thread driver started");

        let mut buf = self.0.network.creds_buf.lock().await;

        let mut mgr = WirelessMgr::new(&self.0.network.networks, &net_ctl, &mut buf);

        let stack = &mut self.0;

        let mut net_task = pin!(stack.run_oper_net(
            &net_stack,
            &netif,
            &mut mdns,
            core::future::pending(),
            Option::<(NoNetwork, NoNetwork)>::None
        ));
        let mut mgr_task = pin!(mgr.run());

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self
            .0
            .root_handler(&(), &netif, &net_ctl_s, &false, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        let mut user_task = pin!(self.2.run(&net_stack, &netif));

        select4(
            &mut net_task,
            &mut mgr_task,
            &mut handler_task,
            &mut user_task,
        )
        .coalesce()
        .await
    }
}

impl<M, E, H, X> ThreadCoexTask for MatterStackWirelessTask<'static, M, wireless::Thread, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<S, N, C, D, G>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mut mdns: D,
        mut gatt: G,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        D: Mdns,
        G: GattPeripheral,
    {
        info!("Thread and BLE drivers started");

        let stack = &mut self.0;

        let mut net_task =
            pin!(stack.run_net_coex(&net_stack, &netif, &net_ctl, &mut mdns, &mut gatt));

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self.0.root_handler(&(), &netif, &net_ctl_s, &true, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        let mut user_task = pin!(self.2.run(&net_stack, &netif));

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }
}

impl<M, E, H, X> GattTask
    for MatterStackWirelessTaskWithMemory<'static, M, wireless::Thread, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
{
    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral,
    {
        let net_ctl = NetCtlWithStatusImpl::new(
            &self.0.network.net_state,
            NoopWirelessNetCtl::new(NetworkType::Thread),
        );

        // Create bump allocator from provided memory
        let mut allocator = BumpAllocator::new(self.3);

        // Use bump allocator instead of stack allocation for largest futures
        let btp_task = allocator
            .alloc_pin(self.0.run_btp(peripheral))
            .map_err(|_| {
                rs_matter::error::Error::new(rs_matter::error::ErrorCode::NoMemory)
            })?;

        let handler = self.0.root_handler(&(), &(), &net_ctl, &false, &self.1);
        let handler_task = allocator
            .alloc_pin(self.0.run_handler((&self.1, handler)))
            .map_err(|_| {
                rs_matter::error::Error::new(rs_matter::error::ErrorCode::NoMemory)
            })?;

        info!(
            "Bump allocator usage: {}/{} bytes",
            allocator.used(),
            allocator.capacity()
        );

        select(btp_task, handler_task).coalesce().await
    }
}

impl<M, E, H, X> ThreadTask
    for MatterStackWirelessTaskWithMemory<'static, M, wireless::Thread, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<S, N, C, D>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mut mdns: D,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        D: Mdns,
    {
        info!("Thread driver started with bump allocator");

        let mut buf = self.0.network.creds_buf.lock().await;

        let mut mgr = WirelessMgr::new(&self.0.network.networks, &net_ctl, &mut buf);

        let stack = &mut self.0;

        // Create bump allocator from provided memory
        let mut allocator = BumpAllocator::new(self.3);

        // Use bump allocator instead of stack allocation for largest futures
        let net_task = allocator
            .alloc_pin(stack.run_oper_net(
                &net_stack,
                &netif,
                &mut mdns,
                core::future::pending(),
                Option::<(NoNetwork, NoNetwork)>::None,
            ))
            .map_err(|_| {
                rs_matter::error::Error::new(rs_matter::error::ErrorCode::NoMemory)
            })?;

        let mut mgr_task = pin!(mgr.run());

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self
            .0
            .root_handler(&(), &netif, &net_ctl_s, &false, &self.1);
        let handler_task = allocator
            .alloc_pin(self.0.run_handler((&self.1, handler)))
            .map_err(|_| {
                rs_matter::error::Error::new(rs_matter::error::ErrorCode::NoMemory)
            })?;

        let mut user_task = pin!(self.2.run(&net_stack, &netif));

        info!(
            "Bump allocator usage: {}/{} bytes",
            allocator.used(),
            allocator.capacity()
        );

        select4(net_task, &mut mgr_task, handler_task, &mut user_task)
            .coalesce()
            .await
    }
}

impl<M, E, H, X> ThreadCoexTask
    for MatterStackWirelessTaskWithMemory<'static, M, wireless::Thread, E, H, X>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
    H: AsyncMetadata + AsyncHandler,
    X: UserTask,
{
    async fn run<S, N, C, D, G>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mut mdns: D,
        mut gatt: G,
    ) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + ThreadDiag + NetChangeNotif,
        D: Mdns,
        G: GattPeripheral,
    {
        info!("Thread and BLE drivers started with bump allocator");

        let stack = &mut self.0;

        // Create bump allocator from provided memory
        let mut allocator = BumpAllocator::new(self.3);

        // Use bump allocator instead of stack allocation for largest futures
        let net_task = allocator
            .alloc_pin(stack.run_net_coex(&net_stack, &netif, &net_ctl, &mut mdns, &mut gatt))
            .map_err(|_| {
                rs_matter::error::Error::new(rs_matter::error::ErrorCode::NoMemory)
            })?;

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self.0.root_handler(&(), &netif, &net_ctl_s, &true, &self.1);
        let handler_task = allocator
            .alloc_pin(self.0.run_handler((&self.1, handler)))
            .map_err(|_| {
                rs_matter::error::Error::new(rs_matter::error::ErrorCode::NoMemory)
            })?;

        let mut user_task = pin!(self.2.run(&net_stack, &netif));

        info!(
            "Bump allocator usage: {}/{} bytes",
            allocator.used(),
            allocator.capacity()
        );

        select3(net_task, handler_task, &mut user_task)
            .coalesce()
            .await
    }
}

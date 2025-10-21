use core::future::Future;
use core::pin::pin;

use embassy_futures::select::{select, select3, select4};
use embassy_sync::blocking_mutex::raw::RawMutex;

use rs_matter::dm::clusters::gen_comm::CommPolicy;
use rs_matter::dm::clusters::gen_diag::GenDiag;
use rs_matter::dm::clusters::net_comm::{NetCtl, NetCtlStatus, NetworkType};
use rs_matter::dm::clusters::wifi_diag::WifiDiag;
use rs_matter::dm::endpoints::{self, with_sys, with_wifi, SysHandler, WifiHandler};
use rs_matter::dm::networks::wireless::{
    self, NetCtlWithStatusImpl, NoopWirelessNetCtl, WirelessMgr,
};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::{clusters::gen_diag::NetifDiag, AsyncHandler};
use rs_matter::dm::{AsyncMetadata, Endpoint};
use rs_matter::error::Error;
use rs_matter::transport::network::btp::GattPeripheral;
use rs_matter::transport::network::NoNetwork;
use rs_matter::utils::select::Coalesce;

use crate::mdns::Mdns;
use crate::nal::NetStack;
use crate::network::Embedding;
use crate::persist::KvBlobStore;
use crate::wireless::{MatterStackWirelessTask, WirelessMatterPersist};
use crate::{pin_alloc, UserTask};

use super::{Gatt, GattTask, PreexistingWireless, WirelessMatterStack};

/// A type alias for a Matter stack running over Wifi (and BLE, during commissioning).
pub type WifiMatterStack<'a, const B: usize, M, E = ()> =
    WirelessMatterStack<'a, B, M, wireless::Wifi, E>;

/// A type alias for the Matter Persister created by calling `WifiMatterStack::create_persist`.
pub type WifiMatterPersist<'a, S, M> = WirelessMatterPersist<'a, S, M, wireless::Wifi>;

impl<const B: usize, M, E> WirelessMatterStack<'_, B, M, wireless::Wifi, E>
where
    M: RawMutex + Send + Sync + 'static,
    E: Embedding + 'static,
{
    /// Run the Matter stack for an already pre-established wireless network where the BLE and the Wifi stacks can co-exist.
    ///
    /// Parameters:
    /// - `net_stack` - a user-provided `NetStack` implementation
    /// - `netif` - a user-provided `Netif` implementation
    /// - `controller` - a user-provided `Controller` implementation
    /// - `mdns` - a user-provided `Mdns` implementation
    /// - `gatt` - a user-provided `GattPeripheral` implementation
    /// - `persist` - a `WifiMatterPersist` implementation instantiated on the stack with `create_persist`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    #[allow(clippy::too_many_arguments)]
    pub async fn run_preex<'t, U, N, C, D, G, S, H, X>(
        &'static self,
        net_stack: U,
        netif: N,
        net_ctl: C,
        mdns: D,
        gatt: G,
        persist: &'t WifiMatterPersist<'_, S, M>,
        handler: H,
        user: X,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        U: NetStack + 't,
        N: NetifDiag + NetChangeNotif + 't,
        C: NetCtl + WifiDiag + NetChangeNotif + 't,
        D: Mdns + 't,
        G: GattPeripheral + 't,
        S: KvBlobStore + 't,
        H: AsyncHandler + AsyncMetadata + 't,
        X: UserTask + 't,
    {
        self.run_coex(
            PreexistingWireless::new(net_stack, netif, net_ctl, mdns, gatt),
            persist,
            handler,
            user,
        )
    }

    /// Run the Matter stack for a wireless network where the BLE and the Wifi stacks can co-exist.
    ///
    /// Parameters:
    /// - `wifi` - a user-provided `WifiCoex` implementation
    /// - `persist` - a `WifiMatterPersist` implementation instantiated on the stack with `create_persist`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run_coex<W, S, H, U>(
        &'static self,
        mut wifi: W,
        persist: &WifiMatterPersist<'_, S, M>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: WifiCoex,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        let _lock = self.run_lock.lock().await;

        info!("Matter Stack memory: {}b", core::mem::size_of_val(self));

        // Since this is the last code executed in the method, resetting the allocator should be safe
        // because all boxes returned by it should be dropped by then
        let _defer = scopeguard::guard((), |_| unsafe {
            self.bump.reset();
        });

        self.matter().reset_transport()?;

        let mut net_task = pin_alloc!(self.bump, self.run_wifi_coex(&mut wifi, handler, user));
        let mut persist_task = pin_alloc!(self.bump, self.run_psm(persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    /// Run the Matter stack for a wireless network where the BLE and the Wifi stacks cannot co-exist.
    ///
    /// Parameters:
    /// - `wifi` - a user-provided `Wifi` + `Gatt` implementation
    /// - `persist` - a `WifiMatterPersist` implementation instantiated on the stack with `create_persist`
    /// - `handler` - a user-provided DM handler implementation
    /// - `user` - a user-provided future that will be polled only when the netif interface is up
    pub async fn run<W, S, H, U>(
        &'static self,
        wifi: W,
        persist: &WifiMatterPersist<'_, S, M>,
        handler: H,
        user: U,
    ) -> Result<(), Error>
    where
        W: Wifi + Gatt,
        S: KvBlobStore,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        let _lock = self.run_lock.lock().await;

        info!("Matter Stack memory: {}b", core::mem::size_of_val(self));

        // Since this is the last code executed in the method, resetting the allocator should be safe
        // because all boxes returned by it should be dropped by then
        let _defer = scopeguard::guard((), |_| unsafe {
            self.bump.reset();
        });

        self.matter().reset_transport()?;

        let mut net_task = pin_alloc!(self.bump, self.run_wifi(wifi, handler, user));
        let mut persist_task = pin_alloc!(self.bump, self.run_psm(persist));

        select(&mut net_task, &mut persist_task).coalesce().await
    }

    fn run_wifi_coex<'t, W, H, U>(
        &'static self,
        wifi: &'t mut W,
        handler: H,
        user: U,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        W: WifiCoex + 't,
        H: AsyncHandler + AsyncMetadata + 't,
        U: UserTask + 't,
    {
        wifi.run(MatterStackWirelessTask(self, handler, user))
    }

    async fn run_wifi<W, H, U>(
        &'static self,
        mut wifi: W,
        handler: H,
        mut user: U,
    ) -> Result<(), Error>
    where
        W: Wifi + Gatt,
        H: AsyncHandler + AsyncMetadata,
        U: UserTask,
    {
        loop {
            let commissioned = self.is_commissioned().await?;

            if !commissioned {
                Gatt::run(
                    &mut wifi,
                    MatterStackWirelessTask(self, &handler, &mut user),
                )
                .await?;
            }

            if commissioned {
                self.matter().close_comm_window()?;
            }

            Wifi::run(
                &mut wifi,
                MatterStackWirelessTask(self, &handler, &mut user),
            )
            .await?;
        }
    }

    /// Return a metadata for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    pub const fn root_endpoint() -> Endpoint<'static> {
        endpoints::root_endpoint(NetworkType::Wifi)
    }

    /// Return a handler for the root (Endpoint 0) of the Matter Node
    /// configured for BLE+Wifi network.
    fn root_handler<'a, C, H>(
        &'a self,
        gen_diag: &'a dyn GenDiag,
        netif_diag: &'a dyn NetifDiag,
        net_ctl: &'a C,
        comm_policy: &'a dyn CommPolicy,
        handler: H,
    ) -> WifiHandler<'a, &'a C, SysHandler<'a, H>>
    where
        C: NetCtl + NetCtlStatus + WifiDiag,
    {
        with_wifi(
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
/// (Netif, UDP stack and Wireless controller) to perform its work.
pub trait WifiTask {
    /// Run the task with the given network stack, network interface, wireless controller and mDNS
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
        C: NetCtl + WifiDiag + NetChangeNotif,
        M: Mdns;
}

impl<T> WifiTask for &mut T
where
    T: WifiTask,
{
    fn run<S, N, C, M>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mdns: M,
    ) -> impl Future<Output = Result<(), Error>>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
        M: Mdns,
    {
        T::run(*self, net_stack, netif, net_ctl, mdns)
    }
}

/// A trait for running a task within a context where the wireless interface is initialized and operable
pub trait Wifi {
    /// Setup the radio to operate in wireless (Wifi or Thread) mode
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: WifiTask;
}

impl<T> Wifi for &mut T
where
    T: Wifi,
{
    fn run<A>(&mut self, task: A) -> impl Future<Output = Result<(), Error>>
    where
        A: WifiTask,
    {
        T::run(self, task)
    }
}

/// A trait representing a task that needs access to the operational wireless interface (Wifi or Thread)
/// as well as to the commissioning BTP GATT peripheral.
///
/// Typically, tasks performing the Matter concurrent commissioning workflow will implement this trait.
pub trait WifiCoexTask {
    /// Run the task with the given network stack, network interface, wireless controller and mDNS
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
        C: NetCtl + WifiDiag + NetChangeNotif,
        M: Mdns,
        G: GattPeripheral;
}

impl<T> WifiCoexTask for &mut T
where
    T: WifiCoexTask,
{
    fn run<S, N, C, M, G>(
        &mut self,
        net_stack: S,
        netif: N,
        net_ctl: C,
        mdns: M,
        gatt: G,
    ) -> impl Future<Output = Result<(), Error>>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
        C: NetCtl + WifiDiag + NetChangeNotif,
        M: Mdns,
        G: GattPeripheral,
    {
        T::run(*self, net_stack, netif, net_ctl, mdns, gatt)
    }
}

/// A trait for running a task within a context where both the wireless interface (Thread or Wifi)
/// is initialized and operable, as well as the BLE GATT peripheral is also operable.
///
/// Typically, tasks performing the Matter concurrent commissioning workflow will ran by implementations
/// of this trait.
pub trait WifiCoex {
    /// Setup the radio to operate in wireless coexist mode (Wifi or Thread + BLE)
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: WifiCoexTask;
}

impl<T> WifiCoex for &mut T
where
    T: WifiCoex,
{
    fn run<A>(&mut self, task: A) -> impl Future<Output = Result<(), Error>>
    where
        A: WifiCoexTask,
    {
        T::run(self, task)
    }
}

impl<S, N, C, M, P> Wifi for PreexistingWireless<S, N, C, M, P>
where
    S: NetStack,
    N: NetifDiag + NetChangeNotif,
    C: NetCtl + WifiDiag + NetChangeNotif,
    M: Mdns,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: WifiTask,
    {
        task.run(&self.net_stack, &self.netif, &self.net_ctl, &mut self.mdns)
            .await
    }
}

impl<S, N, C, M, P> WifiCoex for PreexistingWireless<S, N, C, M, P>
where
    S: NetStack,
    N: NetifDiag + NetChangeNotif,
    C: NetCtl + WifiDiag + NetChangeNotif,
    M: Mdns,
    P: GattPeripheral,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: WifiCoexTask,
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

impl<const B: usize, M, E, H, U> GattTask
    for MatterStackWirelessTask<'static, B, M, wireless::Wifi, E, H, U>
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
            NoopWirelessNetCtl::new(NetworkType::Wifi),
        );

        let mut btp_task = pin!(self.0.run_btp(peripheral));

        let handler = self.0.root_handler(&(), &(), &net_ctl, &false, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        select(&mut btp_task, &mut handler_task).coalesce().await
    }
}

impl<const B: usize, M, E, H, X> WifiTask
    for MatterStackWirelessTask<'static, B, M, wireless::Wifi, E, H, X>
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
        C: NetCtl + WifiDiag + NetChangeNotif,
        D: Mdns,
    {
        info!("Wifi driver started");

        let mut buf = self.0.network.creds_buf.lock().await;

        let mut mgr = WirelessMgr::new(&self.0.network.networks, &net_ctl, &mut buf);

        let stack = &mut self.0;

        let mut net_task = pin!(stack.run_oper_net(
            &net_stack,
            core::future::pending(),
            Option::<(NoNetwork, NoNetwork)>::None
        ));

        let mut mdns_task = pin!(stack.run_oper_netif_mdns(&net_stack, &netif, &mut mdns));

        let mut mgr_task = pin!(mgr.run());

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self
            .0
            .root_handler(&(), &netif, &net_ctl_s, &false, &self.1);
        let mut handler_task = pin!(self.0.run_handler((&self.1, handler)));

        let mut user_task = pin!(self.2.run(&net_stack, &netif));

        select4(
            &mut net_task,
            &mut mdns_task,
            &mut mgr_task,
            select(&mut handler_task, &mut user_task).coalesce(),
        )
        .coalesce()
        .await
    }
}

impl<const B: usize, M, E, H, X> WifiCoexTask
    for MatterStackWirelessTask<'static, B, M, wireless::Wifi, E, H, X>
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
        C: NetCtl + WifiDiag + NetChangeNotif,
        D: Mdns,
        G: GattPeripheral,
    {
        info!("Wifi and BLE drivers started");

        let stack = &mut self.0;
        let bump = &stack.bump;

        let mut net_task = pin_alloc!(
            bump,
            stack.run_net_coex(&net_stack, &netif, &net_ctl, &mut mdns, &mut gatt)
        );

        let net_ctl_s = NetCtlWithStatusImpl::new(&self.0.network.net_state, &net_ctl);

        let handler = self.0.root_handler(&(), &netif, &net_ctl_s, &true, &self.1);
        let mut handler_task = pin_alloc!(bump, self.0.run_handler_with_bump((&self.1, handler)));

        let mut user_task = pin_alloc!(bump, self.2.run(&net_stack, &netif));

        select3(&mut net_task, &mut handler_task, &mut user_task)
            .coalesce()
            .await
    }
}

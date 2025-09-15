#![no_std]
#![allow(async_fn_in_trait)]
#![allow(unknown_lints)]
#![allow(renamed_and_removed_lints)]
#![allow(unexpected_cfgs)]
#![allow(clippy::declare_interior_mutable_const)]
#![allow(clippy::uninlined_format_args)]
#![warn(clippy::large_futures)]
#![warn(clippy::large_stack_frames)]
#![warn(clippy::large_types_passed_by_value)]

use core::future::Future;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;

use cfg_if::cfg_if;
use edge_nal::{UdpBind, UdpSplit};
use embassy_futures::select::{select, select4, Either4};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use persist::{KvBlobBuffer, KvBlobStore, MatterPersist, NetworkPersist};

use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::dev_att::DevAttDataFetcher;
use rs_matter::dm::clusters::gen_diag::NetifDiag;
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::subscriptions::Subscriptions;
use rs_matter::dm::IMBuffer;
use rs_matter::dm::{AsyncHandler, AsyncMetadata};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::respond::DefaultResponder;
use rs_matter::transport::network::{Address, ChainedNetwork, NetworkReceive, NetworkSend};
use rs_matter::utils::epoch::Epoch;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::rand::Rand;
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::{BasicCommData, Matter, MATTER_PORT};

use crate::mdns::Mdns;
use crate::nal::NetStack;
use crate::network::Network;
use crate::persist::SharedKvBlobStore;

#[cfg(feature = "std")]
#[allow(unused_imports)]
#[macro_use]
extern crate std;

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

// This mod MUST go first, so that the others see its macros.
pub(crate) mod fmt;

pub mod eth;
pub mod matter;
pub mod mdns;
pub mod nal;
pub mod network;
pub mod persist;
pub mod rand;
pub mod udp;
pub mod utils;
pub mod wireless;

mod private {
    /// A marker super-trait for sealed traits
    pub trait Sealed {}

    impl Sealed for () {}
}

cfg_if! {
    if #[cfg(feature = "max-subscriptions-32")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 32;
    } else if #[cfg(feature = "max-subscriptions-16")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 16;
    } else if #[cfg(feature = "max-subscriptions-8")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 8;
    } else if #[cfg(feature = "max-subscriptions-7")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 7;
    } else if #[cfg(feature = "max-subscriptions-6")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 6;
    } else if #[cfg(feature = "max-subscriptions-5")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 5;
    } else if #[cfg(feature = "max-subscriptions-4")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 4;
    } else if #[cfg(feature = "max-subscriptions-3")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 3;
    } else if #[cfg(feature = "max-subscriptions-2")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 2;
    } else if #[cfg(feature = "max-subscriptions-1")] {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 1;
    } else {
        /// Max number of subscriptions
        const MAX_SUBSCRIPTIONS: usize = 3;
    }
}

cfg_if! {
    if #[cfg(feature = "max-im-buffers-64")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 64;
    } else if #[cfg(feature = "max-im-buffers-32")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 32;
    } else if #[cfg(feature = "max-im-buffers-16")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 16;
    } else if #[cfg(feature = "max-im-buffers-10")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 10;
    } else if #[cfg(feature = "max-im-buffers-9")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 9;
    } else if #[cfg(feature = "max-im-buffers-8")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 8;
    } else if #[cfg(feature = "max-im-buffers-7")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 7;
    } else if #[cfg(feature = "max-im-buffers-6")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 6;
    } else if #[cfg(feature = "max-im-buffers-5")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 5;
    } else if #[cfg(feature = "max-im-buffers-4")] {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 4;
    } else {
        /// Max number of IM buffers
        const MAX_IM_BUFFERS: usize = 10;
    }
}

cfg_if! {
    if #[cfg(feature = "max-responders-32")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 32;
    } else if #[cfg(feature = "max-responders-16")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 16;
    } else if #[cfg(feature = "max-responders-8")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 8;
    } else if #[cfg(feature = "max-responders-7")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 7;
    } else if #[cfg(feature = "max-responders-6")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 6;
    } else if #[cfg(feature = "max-responders-5")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 5;
    } else if #[cfg(feature = "max-responders-4")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 4;
    } else if #[cfg(feature = "max-responders-3")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 3;
    } else if #[cfg(feature = "max-responders-2")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 2;
    } else if #[cfg(feature = "max-responders-1")] {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 1;
    } else {
        /// Max number of concurrent responders
        const MAX_RESPONDERS: usize = 4;
    }
}

const MAX_BUSY_RESPONDERS: usize = 2;

/// The `MatterStack` struct is the main entry point for the Matter stack.
///
/// It wraps the actual `rs-matter` Matter instance and provides a simplified API for running the stack.
pub struct MatterStack<'a, N>
where
    N: Network,
{
    matter: Matter<'a>,
    buffers: PooledBuffers<MAX_IM_BUFFERS, NoopRawMutex, IMBuffer>,
    subscriptions: Subscriptions<MAX_SUBSCRIPTIONS>,
    store_buf: PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
    #[allow(unused)]
    network: N,
    //netif_conf: Signal<NoopRawMutex, Option<NetifConf>>,
}

impl<'a, N> MatterStack<'a, N>
where
    N: Network,
{
    /// Create a new `MatterStack` instance.
    #[cfg(feature = "std")]
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    pub const fn new_default(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
    ) -> Self {
        Self::new(
            dev_det,
            dev_comm,
            dev_att,
            rs_matter::utils::epoch::sys_epoch,
            rs_matter::utils::rand::sys_rand,
        )
    }

    /// Create a new `MatterStack` instance.
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    pub const fn new(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
        epoch: Epoch,
        rand: Rand,
    ) -> Self {
        Self {
            matter: Matter::new(dev_det, dev_comm, dev_att, epoch, rand, MATTER_PORT),
            buffers: PooledBuffers::new(0),
            subscriptions: Subscriptions::new(),
            store_buf: PooledBuffers::new(0),
            network: N::INIT,
            //netif_conf: Signal::new(None),
        }
    }

    /// Create a new `MatterStack` instance.
    #[cfg(feature = "std")]
    #[allow(clippy::large_stack_frames)]
    pub fn init_default(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
    ) -> impl Init<Self> {
        Self::init(
            dev_det,
            dev_comm,
            dev_att,
            rs_matter::utils::epoch::sys_epoch,
            rs_matter::utils::rand::sys_rand,
        )
    }

    #[allow(clippy::large_stack_frames)]
    pub fn init(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DevAttDataFetcher,
        epoch: Epoch,
        rand: Rand,
    ) -> impl Init<Self> {
        init!(Self {
            matter <- Matter::init(
                dev_det,
                dev_comm,
                dev_att,
                epoch,
                rand,
                MATTER_PORT,
            ),
            buffers <- PooledBuffers::init(0),
            subscriptions <- Subscriptions::init(),
            store_buf <- PooledBuffers::init(0),
            network <- N::init(),
            //netif_conf: Signal::new(None),
        })
    }

    /// Create a new `SharedKvBlobStore` instance wrapping the
    /// provided `KvBlobStore` implementation and the internal store buffer
    /// available in the `MatterStack` instance.
    pub fn create_shared_store<S>(&self, store: S) -> SharedKvBlobStore<'_, S>
    where
        S: KvBlobStore,
    {
        SharedKvBlobStore::new(store, &self.store_buf)
    }

    /// Create a new `MatterPersist` instance for the Matter stack.
    fn create_persist<'t, S>(
        &'t self,
        store: &'t SharedKvBlobStore<'t, S>,
    ) -> MatterPersist<'t, S, N::PersistContext<'t>>
    where
        S: KvBlobStore,
    {
        MatterPersist::new(store, self.matter(), self.network().persist_context())
    }

    /// A utility method to replace the initial Device Attestation Data Fetcher with another one.
    ///
    /// Reasoning and use-cases explained in the documentation of `replace_mdns`.
    pub fn replace_dev_att(&mut self, dev_att: &'a dyn DevAttDataFetcher) {
        self.matter.replace_dev_att(dev_att);
    }

    /// Get a reference to the `Matter` instance.
    pub const fn matter(&self) -> &Matter<'a> {
        &self.matter
    }

    pub const fn store_buf(&self) -> &PooledBuffers<1, NoopRawMutex, KvBlobBuffer> {
        &self.store_buf
    }

    /// Get a reference to the `Network` instance.
    /// Useful when the user instantiates `MatterStack` with a custom network type.
    pub const fn network(&self) -> &N {
        &self.network
    }

    /// Notifies the Matter instance that there is a change in the state
    /// of one of the clusters.
    ///
    /// User is expected to call this method when user-provided clusters
    /// change their state.
    ///
    /// This is necessary so as the Matter instance can notify clients
    /// that have active subscriptions to some of the changed clusters.
    pub fn notify_changed(&self) {
        self.subscriptions.notify_changed();
    }

    // /// User code hook to get the state of the netif passed to the
    // /// `run_with_netif` method.
    // ///
    // /// Useful when user code needs to bring up/down its own IP services depending on
    // /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    // pub async fn get_netif_conf(&self) -> Option<NetifConf> {
    //     self.netif_conf
    //         .wait(|netif_conf| Some(netif_conf.clone()))
    //         .await
    // }

    // fn update_netif_conf(&self, netif_conf: Option<&NetifConf>) -> bool {
    //     self.netif_conf.modify(|global_ip_info| {
    //         if global_ip_info.as_ref() != netif_conf {
    //             *global_ip_info = netif_conf.cloned();
    //             (true, true)
    //         } else {
    //             (false, false)
    //         }
    //     })
    // }

    // /// User code hook to detect changes to the IP state of the netif passed to the
    // /// `run_with_netif` method.
    // ///
    // /// Useful when user code needs to bring up/down its own IP services depending on
    // /// when the netif controlled by Matter goes up, down or changes its IP configuration.
    // pub async fn wait_netif_changed(
    //     &self,
    //     prev_netif_info: Option<&NetifConf>,
    // ) -> Option<NetifConf> {
    //     self.netif_conf
    //         .wait(|netif_info| (netif_info.as_ref() != prev_netif_info).then(|| netif_info.clone()))
    //         .await
    // }

    /// Return information whether the Matter instance is already commissioned.
    pub async fn is_commissioned(&self) -> Result<bool, Error> {
        Ok(self.matter().is_commissioned())
    }

    /// This method is a specialization of `run_transport_net` over the UDP transport (both IPv4 and IPv6).
    /// It calls `run_transport_net` and in parallel runs the mDNS service.
    ///
    /// The netif instance is necessary, so that the loop can monitor the network and bring up/down
    /// the main UDP transport and the mDNS service when the netif goes up/down or changes its IP addresses.
    ///
    /// Parameters:
    /// - `net_stack` - a user-provided network stack that implements `UdpBind`, `UdpConnect`, `TcpBind`, `TcpConnect`, and `Dns`
    /// - `netif` - a user-provided `Netif` implementation
    /// - `mdns` - a user-provided mDNS implementation that implements `Mdns`
    /// - `until` - the method will return once this future becomes ready
    /// - `comm` - a tuple of additional and optional `NetworkReceive` and `NetworkSend` transport implementations
    ///   (useful when a second transport needs to run in parallel with the operational Matter transport,
    ///   i.e. when using concurrent commissisoning)
    async fn run_oper_net<U, I, M, X, R, S>(
        &self,
        net_stack: U,
        netif: I,
        mut mdns: M,
        until: X,
        mut comm: Option<(R, S)>,
    ) -> Result<(), Error>
    where
        U: NetStack,
        I: NetifDiag + NetChangeNotif,
        M: Mdns,
        X: Future<Output = Result<(), Error>>,
        R: NetworkReceive,
        S: NetworkSend,
    {
        #[derive(Clone, Debug, Eq, PartialEq, Hash)]
        #[cfg_attr(feature = "defmt", derive(defmt::Format))]
        struct Netif {
            ipv6: Ipv6Addr,
            ipv4: Ipv4Addr,
            mac: [u8; 8],
            operational: bool,
            netif_index: u32,
        }

        impl Netif {
            pub const fn new() -> Self {
                Self {
                    ipv6: Ipv6Addr::UNSPECIFIED,
                    ipv4: Ipv4Addr::UNSPECIFIED,
                    mac: [0; 8],
                    operational: false,
                    netif_index: 0,
                }
            }
        }

        fn load_netif<C>(net_ctl: C, netif: &mut Netif) -> Result<(), Error>
        where
            C: NetifDiag,
        {
            netif.operational = false;
            netif.ipv6 = Ipv6Addr::UNSPECIFIED;
            netif.ipv4 = Ipv4Addr::UNSPECIFIED;
            netif.mac = [0; 8];

            net_ctl.netifs(&mut |ni| {
                if ni.operational && !ni.ipv6_addrs.is_empty() {
                    netif.operational = true;
                    netif.ipv6 = ni.ipv6_addrs[0];
                    netif.ipv4 = ni
                        .ipv4_addrs
                        .first()
                        .copied()
                        .unwrap_or(Ipv4Addr::UNSPECIFIED);
                    netif.mac = *ni.hw_addr;
                    netif.netif_index = ni.netif_index;
                }

                Ok(())
            })
        }

        async fn wait_changed<C>(
            net_ctl: C,
            cur_netif: &Netif,
            new_netif: &mut Netif,
        ) -> Result<(), Error>
        where
            C: NetifDiag + NetChangeNotif,
        {
            loop {
                load_netif(&net_ctl, new_netif)?;

                if &*new_netif != cur_netif {
                    trace!("Change detected: {:?}", new_netif);
                    break Ok(());
                }

                trace!("No change");
                net_ctl.wait_changed().await;
            }
        }

        let mut until_task = pin!(until);

        // let _guard = scopeguard::guard((), |_| {
        //     self.update_netif_conf(None);
        // });

        let mut new_netif = Netif::new();
        load_netif(&netif, &mut new_netif)?;

        loop {
            let cur_netif = new_netif.clone();

            let mut netif_changed_task = pin!(wait_changed(&netif, &cur_netif, &mut new_netif));

            let result = if cur_netif.operational {
                info!("Netif up: {:?}", cur_netif);

                let udp_bind = unwrap!(net_stack.udp_bind());

                let mut socket = udp_bind
                    .bind(SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::UNSPECIFIED,
                        MATTER_PORT,
                        0,
                        0,
                    )))
                    .await
                    .map_err(|_| ErrorCode::StdIoError)?;

                let (recv, send) = socket.split();

                let mut mdns_task = pin!(mdns.run(
                    self.matter(),
                    &udp_bind,
                    &cur_netif.mac,
                    cur_netif.ipv4,
                    cur_netif.ipv6,
                    cur_netif.netif_index,
                ));

                if let Some((comm_recv, comm_send)) = comm.as_mut() {
                    info!("Running operational and extra networks");

                    let mut netw_task = pin!(self.run_transport_net(
                        ChainedNetwork::new(Address::is_udp, udp::Udp(send), comm_send),
                        ChainedNetwork::new(Address::is_udp, udp::Udp(recv), comm_recv),
                    ));

                    select4(
                        &mut netw_task,
                        &mut mdns_task,
                        &mut netif_changed_task,
                        &mut until_task,
                    )
                    .await
                } else {
                    info!("Running operational network");

                    let mut netw_task =
                        pin!(self.run_transport_net(udp::Udp(send), udp::Udp(recv),));

                    select4(
                        &mut netw_task,
                        &mut mdns_task,
                        &mut netif_changed_task,
                        &mut until_task,
                    )
                    .await
                }
            } else if let Some((comm_recv, comm_send)) = comm.as_mut() {
                info!("Running commissioning network only");

                let mut netw_task = pin!(self.run_transport_net(comm_send, comm_recv));

                select4(
                    &mut netw_task,
                    core::future::pending(),
                    &mut netif_changed_task,
                    &mut until_task,
                )
                .await
            } else {
                info!("Netif down");

                select4(
                    core::future::pending(),
                    core::future::pending(),
                    &mut netif_changed_task,
                    &mut until_task,
                )
                .await
            };

            match result {
                Either4::Third(_) => info!("IP network change detected"),
                Either4::First(r) | Either4::Second(r) | Either4::Fourth(r) => break r,
            }
        }
    }

    async fn run_handler<H>(&self, handler: H) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        // TODO
        // Reset the Matter transport buffers and all sessions first
        // self.matter().reset_transport()?;

        select(self.run_responder(&handler), handler.run())
            .coalesce()
            .await
    }

    async fn run_psm<S, C>(&self, persist: &MatterPersist<'_, S, C>) -> Result<(), Error>
    where
        S: KvBlobStore,
        C: NetworkPersist,
    {
        persist.run().await
    }

    async fn run_responder<H>(&self, handler: H) -> Result<(), Error>
    where
        H: AsyncHandler + AsyncMetadata,
    {
        let responder =
            DefaultResponder::new(self.matter(), &self.buffers, &self.subscriptions, handler);

        info!(
            "Responder memory: Responder={}B, Runner={}B",
            core::mem::size_of_val(&responder),
            core::mem::size_of_val(&responder.run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>())
        );

        // Run the responder with up to MAX_RESPONDERS handlers (i.e. MAX_RESPONDERS exchanges can be handled simultenously)
        // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
        responder
            .run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>()
            .await?;

        Ok(())
    }

    async fn run_transport_net<S, R>(&self, send: S, recv: R) -> Result<(), Error>
    where
        S: NetworkSend,
        R: NetworkReceive,
    {
        self.matter().run_transport(send, recv).await?;

        Ok(())
    }
}

/// A trait representing a user task that needs access to the operational network interface
/// (Netif and net stack) to perform its work.
///
/// Note that the task would be started only when `rs-matter`
/// brings up the operational interface (eth, wifi or thread)
/// and if the interface goes down, the user task would be stopped.
/// Upon re-connection, the task would be started again.
pub trait UserTask {
    /// Run the task with the given network stack and network interface
    async fn run<S, N>(&mut self, net_stack: S, netif: N) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif;
}

impl<T> UserTask for &mut T
where
    T: UserTask,
{
    async fn run<S, N>(&mut self, net_stack: S, netif: N) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
    {
        (*self).run(net_stack, netif).await
    }
}

impl UserTask for () {
    async fn run<S, N>(&mut self, _net_stack: S, _netif: N) -> Result<(), Error>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
    {
        core::future::pending::<()>().await;

        Ok(())
    }
}

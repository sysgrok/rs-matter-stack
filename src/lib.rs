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

use core::fmt::Debug;
use core::future::Future;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use core::pin::pin;

use cfg_if::cfg_if;

use edge_nal::{UdpBind, UdpSplit};

use embassy_futures::select::{select, select3, select_slice};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::Duration;

use persist::{KvBlobBuffer, KvBlobStore, MatterPersist, NetworkPersist};

use rs_matter::crypto::Crypto;
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::dev_att::DeviceAttestation;
use rs_matter::dm::clusters::gen_diag::NetifDiag;
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::dm::subscriptions::Subscriptions;
use rs_matter::dm::{
    AsyncHandler, AsyncMetadata, AttrId, ChangeNotify, ClusterId, DataModel, EndptId, IMBuffer,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::pairing::qr::QrTextType;
use rs_matter::respond::{DefaultResponder, ExchangeHandler, Responder};
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::transport::network::{Address, ChainedNetwork, NetworkReceive, NetworkSend};
use rs_matter::utils::epoch::Epoch;
use rs_matter::utils::init::{init, Init};
use rs_matter::utils::select::Coalesce;
use rs_matter::utils::storage::pooled::PooledBuffers;
use rs_matter::utils::sync::IfMutex;
use rs_matter::{BasicCommData, Matter, MATTER_PORT};

use crate::bump::Bump;
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

pub mod bump;
pub mod eth;
pub mod matter;
pub mod mdns;
pub mod nal;
pub mod network;
pub mod persist;
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

pub(crate) type MatterStackDataModel<'a, C, H> =
    DataModel<'a, MAX_SUBSCRIPTIONS, C, PooledBuffers<MAX_IM_BUFFERS, NoopRawMutex, IMBuffer>, H>;

/// The `MatterStack` struct is the main entry point for the Matter stack.
///
/// It wraps the actual `rs-matter` Matter instance and provides a simplified API for running the stack.
pub struct MatterStack<'a, const B: usize, N>
where
    N: Network,
{
    matter: Matter<'a>,
    buffers: PooledBuffers<MAX_IM_BUFFERS, NoopRawMutex, IMBuffer>,
    subscriptions: Subscriptions<MAX_SUBSCRIPTIONS>,
    store_buf: PooledBuffers<1, NoopRawMutex, KvBlobBuffer>,
    bump: Bump<B, NoopRawMutex>,
    run_lock: IfMutex<NoopRawMutex, ()>,
    #[allow(unused)]
    network: N,
    //netif_conf: Signal<NoopRawMutex, Option<NetifConf>>,
}

impl<'a, const B: usize, N> MatterStack<'a, B, N>
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
        dev_att: &'a dyn DeviceAttestation,
    ) -> Self {
        Self::new(
            dev_det,
            dev_comm,
            dev_att,
            rs_matter::utils::epoch::sys_epoch,
        )
    }

    /// Create a new `MatterStack` instance.
    #[allow(clippy::large_stack_frames)]
    #[inline(always)]
    pub const fn new(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DeviceAttestation,
        epoch: Epoch,
    ) -> Self {
        Self {
            matter: Matter::new(dev_det, dev_comm, dev_att, epoch, MATTER_PORT),
            buffers: PooledBuffers::new(0),
            subscriptions: Subscriptions::new(),
            store_buf: PooledBuffers::new(0),
            bump: Bump::new(),
            run_lock: IfMutex::new(()),
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
        dev_att: &'a dyn DeviceAttestation,
    ) -> impl Init<Self> {
        Self::init(
            dev_det,
            dev_comm,
            dev_att,
            rs_matter::utils::epoch::sys_epoch,
        )
    }

    #[allow(clippy::large_stack_frames)]
    pub fn init(
        dev_det: &'a BasicInfoConfig,
        dev_comm: BasicCommData,
        dev_att: &'a dyn DeviceAttestation,
        epoch: Epoch,
    ) -> impl Init<Self> {
        init!(Self {
            matter <- Matter::init(
                dev_det,
                dev_comm,
                dev_att,
                epoch,
                MATTER_PORT,
            ),
            buffers <- PooledBuffers::init(0),
            subscriptions <- Subscriptions::init(),
            store_buf <- PooledBuffers::init(0),
            bump <- Bump::init(),
            run_lock <- IfMutex::init(()),
            network <- N::init(),
            //netif_conf: Signal::new(None),
        })
    }

    /// A utility method to replace the initial Device Attestation Data Fetcher with another one.
    ///
    /// Reasoning and use-cases explained in the documentation of `replace_mdns`.
    pub fn replace_dev_att(&mut self, dev_att: &'a dyn DeviceAttestation) {
        self.matter.replace_dev_att(dev_att);
    }

    /// Create a new `MatterPersist` instance for the Matter stack.
    ///
    /// # Arguments
    /// - `store` - a reference to a `KvBlobStore` instance
    pub fn create_persist<'t, S>(&'t self, store: S) -> MatterPersist<'t, S, N::PersistContext<'t>>
    where
        S: KvBlobStore + 't,
    {
        MatterPersist::new(
            SharedKvBlobStore::new(store, &self.store_buf),
            self.matter(),
            self.network().persist_context(),
        )
    }

    /// An "all in one" persistence initializer method.
    ///
    /// # Arguments
    /// - `crypto` - a user-provided crypto implementation
    /// - `store` - a reference to a `KvBlobStore` instance
    ///
    /// This method does the following:
    /// - Create the Matter Persister for that stack;
    /// - Load the stack from the just-created persister;
    /// - Check if the state of the device designates a not-yet-commissioned device and if so:
    ///   - Open the commissioning window for 15 minutes;
    ///   - Print the device pairing code and QR text to the console
    ///   - Print the device QR code to the console
    ///
    /// This method is useful primarily for development/demo purposes. In production scenarios,
    /// it is likely that the user would require more fine-graned control over when to open
    /// the commissioning window, with what timeout, whether to print the QR code, etc.
    pub async fn create_persist_with_comm_window<'t, C, S>(
        &'t self,
        crypto: C,
        store: S,
    ) -> Result<MatterPersist<'t, S, N::PersistContext<'t>>, Error>
    where
        C: Crypto,
        S: KvBlobStore + 't,
    {
        // The data model is not created yet, so we don't have to notify anything
        struct DummyNotify;

        impl ChangeNotify for DummyNotify {
            fn notify(&self, _endpoint_id: EndptId, _cluster_id: ClusterId, _attr_id: AttrId) {}
        }

        let persist = self.create_persist(store);

        persist.load().await?;

        if !self.matter().is_commissioned() {
            info!("Device is not commissioned yet, opening commissioning window...");

            self.matter().open_basic_comm_window(
                MAX_COMM_WINDOW_TIMEOUT_SECS,
                crypto,
                &DummyNotify,
            )?;
            self.matter()
                .print_standard_qr_text(self.network.discovery_capabilities())?;
            self.matter().print_standard_qr_code(
                QrTextType::Unicode,
                self.network.discovery_capabilities(),
            )?;
        } else {
            info!("Device is already commissioned");
        }

        Ok(persist)
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
    /// of one of the clusters' attributes.
    ///
    /// User is expected to call this method when a user-provided cluster attribute
    /// changes its state.
    ///
    /// This is necessary so as the Matter instance can notify clients
    /// that have active subscriptions to some of the changed clusters.
    ///
    /// # Arguments
    /// - `endpoint_id` - the endpoint ID where the cluster is located
    /// - `cluster_id` - the ID of the cluster that has changed
    /// - `attr_id` - the ID of the attribute that has changed
    pub fn notify_attribute_changed(
        &self,
        endpoint_id: EndptId,
        cluster_id: ClusterId,
        attr_id: AttrId,
    ) {
        self.subscriptions
            .notify_attribute_changed(endpoint_id, cluster_id, attr_id);
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
    /// It calls `run_transport_net`.
    ///
    /// #Arguments
    /// - `crypto` - a user-provided crypto implementation, necessary for the secure sessions establishment that happens in the operational network
    /// - `net_stack` - a user-provided network stack that implements `UdpBind`, `UdpConnect`, `TcpBind`, `TcpConnect`, and `Dns`
    /// - `netif` - a user-provided `Netif` implementation
    /// - `until` - the method will return once this future becomes ready
    /// - `comm` - a tuple of additional and optional `NetworkReceive` and `NetworkSend` transport implementations
    ///   (useful when a second transport needs to run in parallel with the operational Matter transport,
    ///   i.e. when using concurrent commissisoning)
    async fn run_oper_net<C, U, X, R, S>(
        &self,
        crypto: C,
        net_stack: U,
        until: X,
        mut comm: Option<(R, S)>,
    ) -> Result<(), Error>
    where
        C: Crypto,
        U: NetStack,
        X: Future<Output = Result<(), Error>>,
        R: NetworkReceive,
        S: NetworkSend,
    {
        fn map_err<E: Debug>(e: E) -> Error {
            warn!("Matter UDP network error: {:?}", debug2format!(e));
            ErrorCode::StdIoError.into() // TODO
        }

        let udp_bind = unwrap!(net_stack.udp_bind());

        let mut socket = udp_bind
            .bind(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::UNSPECIFIED,
                MATTER_PORT,
                0,
                0,
            )))
            .await
            .map_err(map_err)?;

        let (recv, send) = socket.split();

        let mut until_task = pin!(until);

        if let Some((comm_recv, comm_send)) = comm.as_mut() {
            info!("Running operational and commissioning networks");

            let mut netw_task = pin!(self.run_transport_net(
                &crypto,
                ChainedNetwork::new(Address::is_udp, udp::Udp(send), comm_send),
                ChainedNetwork::new(Address::is_udp, udp::Udp(recv), comm_recv),
            ));

            select(&mut netw_task, &mut until_task).coalesce().await
        } else {
            info!("Running operational network");

            let mut netw_task =
                pin!(self.run_transport_net(&crypto, udp::Udp(send), udp::Udp(recv)));

            select(&mut netw_task, &mut until_task).coalesce().await
        }
    }

    /// This method runs the mDNS service.
    ///
    /// The netif instance is necessary, so that the loop can monitor the network and bring up/down
    /// the mDNS service when the netif goes up/down or changes its IP addresses.
    ///
    /// This is necessary because mDNS needs to know the current IP addresses and
    /// also needs to stop when the netif goes down.
    ///
    /// # Arguments
    /// - `crypto` - a user-provided crypto implementation
    /// - `notify` - a user-provided `ChangeNotify`; typically, the Data Model
    /// - `net_stack` - a user-provided network stack that implements `UdpBind`, `UdpConnect`, `TcpBind`, `TcpConnect`, and `Dns`
    /// - `netif` - a user-provided `Netif` implementation
    /// - `mdns` - a user-provided mDNS implementation
    async fn run_oper_netif_mdns<C, U, I, M>(
        &self,
        crypto: C,
        notify: &dyn ChangeNotify,
        net_stack: U,
        netif: I,
        mut mdns: M,
    ) -> Result<(), Error>
    where
        C: Crypto,
        U: NetStack,
        I: NetifDiag + NetChangeNotif,
        M: Mdns,
    {
        #[derive(Clone, Debug, Eq, PartialEq, Hash)]
        #[cfg_attr(feature = "defmt", derive(defmt::Format))]
        struct NetifState {
            ipv6: Ipv6Addr,
            ipv4: Ipv4Addr,
            mac: [u8; 8],
            operational: bool,
            netif_index: u32,
        }

        impl NetifState {
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

        fn load_netif_state<I>(net_diag: I, state: &mut NetifState) -> Result<(), Error>
        where
            I: NetifDiag,
        {
            state.operational = false;
            state.ipv6 = Ipv6Addr::UNSPECIFIED;
            state.ipv4 = Ipv4Addr::UNSPECIFIED;
            state.mac = [0; 8];

            net_diag.netifs(&mut |ni| {
                if ni.operational && !ni.ipv6_addrs.is_empty() {
                    state.operational = true;
                    state.ipv6 = ni.ipv6_addrs[0];
                    state.ipv4 = ni
                        .ipv4_addrs
                        .first()
                        .copied()
                        .unwrap_or(Ipv4Addr::UNSPECIFIED);
                    state.mac = *ni.hw_addr;
                    state.netif_index = ni.netif_index;
                }

                Ok(())
            })
        }

        async fn wait_changed<I>(
            net_diag: I,
            cur_state: &NetifState,
            new_state: &mut NetifState,
        ) -> Result<(), Error>
        where
            I: NetifDiag + NetChangeNotif,
        {
            loop {
                load_netif_state(&net_diag, new_state)?;

                if &*new_state != cur_state {
                    info!(
                        "Netif change detected.\n    Old: {:?}\n    New: {:?}",
                        cur_state, new_state
                    );
                    break Ok(());
                }

                trace!("No change");
                net_diag.wait_changed().await;
            }
        }

        // let _guard = scopeguard::guard((), |_| {
        //     self.update_netif_conf(None);
        // });

        let mut new_state = NetifState::new();
        load_netif_state(&netif, &mut new_state)?;

        loop {
            let cur_state = new_state.clone();

            let mut netif_changed_task = pin!(wait_changed(&netif, &cur_state, &mut new_state));

            let mut mdns_task = pin!(async {
                if cur_state.operational {
                    info!("Netif up: {:?}", cur_state);

                    let udp_bind = unwrap!(net_stack.udp_bind());

                    info!("Running mDNS");

                    loop {
                        let _result = mdns
                            .run(
                                self.matter(),
                                &crypto,
                                notify,
                                &udp_bind,
                                &cur_state.mac,
                                cur_state.ipv4,
                                cur_state.ipv6,
                                cur_state.netif_index,
                            )
                            .await;

                        warn!("mDNS failed with {:?}, retrying in 5s...", _result);
                        embassy_time::Timer::after(Duration::from_secs(5)).await;
                    }
                } else {
                    info!("Netif down");
                    core::future::pending::<()>().await;
                }

                Ok(())
            });

            select(&mut netif_changed_task, &mut mdns_task)
                .coalesce()
                .await?;
        }
    }

    #[inline(always)]
    fn dm<C, H>(&self, crypto: C, handler: H) -> MatterStackDataModel<'_, C, H>
    where
        C: Crypto,
        H: AsyncHandler + AsyncMetadata,
    {
        MatterStackDataModel::new(
            self.matter(),
            crypto,
            &self.buffers,
            &self.subscriptions,
            handler,
        )
    }

    async fn run_dm<C, H>(&self, dm: &MatterStackDataModel<'_, C, H>) -> Result<(), Error>
    where
        C: Crypto,
        H: AsyncHandler + AsyncMetadata,
    {
        // TODO
        // Reset the Matter transport buffers and all sessions first
        // self.matter().reset_transport()?;

        let mut responder = pin!(self.run_responder(dm));
        let mut dm_job = pin!(dm.run());

        select(&mut responder, &mut dm_job).coalesce().await
    }

    async fn run_dm_with_bump<C, H>(&self, dm: &MatterStackDataModel<'_, C, H>) -> Result<(), Error>
    where
        C: Crypto,
        H: AsyncHandler + AsyncMetadata,
    {
        // TODO
        // Reset the Matter transport buffers and all sessions first
        // self.matter().reset_transport()?;

        let mut responder = pin_alloc!(self.bump, self.run_responder_with_bump(dm));
        let mut dm_job = pin!(dm.run());

        select(&mut responder, &mut dm_job).coalesce().await
    }

    fn run_psm<'t, S, C>(
        &'t self,
        persist: &'t MatterPersist<'_, S, C>,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        S: KvBlobStore,
        C: NetworkPersist,
    {
        persist.run()
    }

    async fn run_responder<C, H>(&self, dm: &MatterStackDataModel<'_, C, H>) -> Result<(), Error>
    where
        C: Crypto,
        H: AsyncHandler + AsyncMetadata,
    {
        let responder = DefaultResponder::new(dm);

        // Run the responder with up to MAX_RESPONDERS handlers (i.e. MAX_RESPONDERS exchanges can be handled simultenously)
        // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
        pin!(responder.run::<MAX_RESPONDERS, MAX_BUSY_RESPONDERS>()).await?;

        Ok(())
    }

    async fn run_responder_with_bump<C, H>(
        &self,
        dm: &MatterStackDataModel<'_, C, H>,
    ) -> Result<(), Error>
    where
        C: Crypto,
        H: AsyncHandler + AsyncMetadata,
    {
        let responder = DefaultResponder::new(dm);

        let mut actual = pin_alloc!(
            self.bump,
            self.run_one_responder_with_bump::<MAX_RESPONDERS, _>(responder.responder())
        );
        let mut busy = pin_alloc!(
            self.bump,
            self.run_one_responder_with_bump::<MAX_BUSY_RESPONDERS, _>(responder.busy_responder())
        );
        let mut sub = pin_alloc!(self.bump, responder.process_subscriptions());

        select3(&mut actual, &mut busy, &mut sub).coalesce().await
    }

    /// Run a responder with Q handlers using the provided bump allocator.
    async fn run_one_responder_with_bump<const Q: usize, T>(
        &self,
        responder: &Responder<'_, T>,
    ) -> Result<(), Error>
    where
        T: ExchangeHandler,
    {
        info!("{}: Creating {} handlers", responder.name(), Q);

        let mut handlers = heapless::Vec::<_, Q>::new();
        debug!(
            "{}: Handlers size: {}B",
            responder.name(),
            core::mem::size_of_val(&handlers)
        );

        for handler_id in 0..Q {
            unwrap!(handlers
                .push(pin_alloc!(self.bump, responder.handle(handler_id)))
                .map_err(|_| ())); // Cannot fail because the vector has size N
        }

        let handlers = pin!(handlers);
        let handlers = unsafe { handlers.map_unchecked_mut(|handlers| handlers.as_mut_slice()) };

        select_slice(handlers).await.0
    }

    fn run_transport_net<'t, C, S, R>(
        &'t self,
        crypto: C,
        send: S,
        recv: R,
    ) -> impl Future<Output = Result<(), Error>> + 't
    where
        C: Crypto + 't,
        S: NetworkSend + 't,
        R: NetworkReceive + 't,
    {
        self.matter().run_transport(crypto, send, recv)
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
    fn run<S, N>(&mut self, net_stack: S, netif: N) -> impl Future<Output = Result<(), Error>>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
    {
        (*self).run(net_stack, netif)
    }
}

impl UserTask for () {
    fn run<S, N>(&mut self, _net_stack: S, _netif: N) -> impl Future<Output = Result<(), Error>>
    where
        S: NetStack,
        N: NetifDiag + NetChangeNotif,
    {
        core::future::pending::<Result<(), Error>>()
    }
}

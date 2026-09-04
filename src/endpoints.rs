//! Root endpoint (Endpoint 0) handler chains, split by ownership.
//!
//! This module follows the shape of `rs_matter::dm::endpoints`, but splits the
//! system clusters of the root endpoint into two chains:
//!
//! - [`root_handler`] - the clusters the *user* owns: Descriptor, Basic
//!   Information, Administrator Commissioning, Operational Credentials, Access
//!   Control, Group Key Management, Software Diagnostics and Time
//!   Synchronization. None of these depends on the operational network, so the
//!   user builds this chain once, chains it with the application clusters (and
//!   any additional root endpoint clusters) and hands the result to the stack.
//!
//! - [`eth_net_handler`] / [`wifi_net_handler`] / [`thread_net_handler`] - the
//!   clusters the *stack* owns, chained by the stack on top of the user handler:
//!   Network Commissioning, the network-type diagnostics cluster (Ethernet, Wifi
//!   or Thread), General Diagnostics and General Commissioning. These are the
//!   clusters whose inputs only the stack knows: the network controller and the
//!   network interface (which, with non-concurrent commissioning, exist only
//!   in the operational phase and are re-created on each BLE <-> Wifi/Thread
//!   handover) and the commissioning policy (which depends on whether the stack
//!   runs concurrent or non-concurrent commissioning).
//!
//! The metadata of the root endpoint is unaffected by the split: `root_endpoint!`
//! keeps listing all clusters, as the Descriptor cluster and the Interaction
//! Model dispatch are driven by the metadata, not by the handler chain.

use rs_matter::crypto::RngCore;
use rs_matter::dm::clusters::acl::{self, AclHandler, ClusterHandler as _};
use rs_matter::dm::clusters::adm_comm::{self, AdminCommHandler, ClusterHandler as _};
use rs_matter::dm::clusters::basic_info::{self, BasicInfoHandler, ClusterHandler as _};
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _, DescHandler};
use rs_matter::dm::clusters::eth_diag::{self, ClusterHandler as _, EthDiagHandler};
use rs_matter::dm::clusters::gen_comm::{self, ClusterHandler as _, CommPolicy, GenCommHandler};
use rs_matter::dm::clusters::gen_diag::{
    self, ClusterHandler as _, GenDiag, GenDiagHandler, NetifDiag,
};
use rs_matter::dm::clusters::grp_key_mgmt::{self, ClusterHandler as _, GrpKeyMgmtHandler};
use rs_matter::dm::clusters::net_comm::{
    self, ClusterAsyncHandler as _, NetCommHandler, NetCtl, NetCtlStatus,
};
use rs_matter::dm::clusters::noc::{self, ClusterHandler as _, NocHandler};
use rs_matter::dm::clusters::sw_diag::{self, ClusterHandler as _, SwDiag, SwDiagHandler};
use rs_matter::dm::clusters::thread_diag::{
    self, ClusterHandler as _, ThreadDiag, ThreadDiagHandler,
};
use rs_matter::dm::clusters::time_sync::{self, ClusterHandler as _, TimeSyncHandler};
use rs_matter::dm::clusters::wifi_diag::{
    self, AlwaysConnected, ClusterHandler as _, WifiDiag, WifiDiagHandler, WirelessDiag,
};
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::networks::eth::EthNetCtl;
use rs_matter::dm::{
    Async, ChainedHandler, ClusterId, Dataver, EmptyHandler, EndptId, MatchContext, Matcher,
};
use rs_matter::handler_chain_type;

/// A type alias for the handler chain returned by `root_handler()`.
///
/// This is a synchronous chain; wrap it in `Async` to chain it with
/// asynchronous handlers.
pub type RootHandler<'a> = handler_chain_type!(
    FnMatcher => desc::HandlerAdaptor<DescHandler<'a>>,
    FnMatcher => basic_info::HandlerAdaptor<BasicInfoHandler>,
    FnMatcher => adm_comm::HandlerAdaptor<AdminCommHandler>,
    FnMatcher => noc::HandlerAdaptor<NocHandler>,
    FnMatcher => acl::HandlerAdaptor<AclHandler>,
    FnMatcher => grp_key_mgmt::HandlerAdaptor<GrpKeyMgmtHandler>,
    FnMatcher => sw_diag::HandlerAdaptor<SwDiagHandler<'a>>,
    FnMatcher => time_sync::HandlerAdaptor<TimeSyncHandler<'a>>
);

/// A type alias for the handler chain returned by `eth_net_handler()`.
pub type EthNetHandler<'a, H> =
    NetHandler<'a, EthNetCtl<'a>, eth_diag::HandlerAdaptor<EthDiagHandler>, H>;

/// A type alias for the handler chain returned by `wifi_net_handler()`.
pub type WifiNetHandler<'a, T, H> =
    NetHandler<'a, T, wifi_diag::HandlerAdaptor<WifiDiagHandler<'a>>, H>;

/// A type alias for the handler chain returned by `thread_net_handler()`.
pub type ThreadNetHandler<'a, T, H> =
    NetHandler<'a, T, thread_diag::HandlerAdaptor<ThreadDiagHandler<'a>>, H>;

/// A type alias for the handler chain returned by `net_handler()`.
///
/// `H` is the handler the operational network clusters are chained on top of;
/// it receives everything the network clusters do not match.
///
/// The non-async clusters are kept in a single non-async sub-chain behind a
/// single `Async` wrapper, so that the async-to-sync adaptation is
/// monomorphized once rather than once per cluster.
pub type NetHandler<'a, T, N, H> = handler_chain_type!(
    FnMatcher => net_comm::HandlerAsyncAdaptor<NetCommHandler<'a, T>>,
    FnMatcher => Async<handler_chain_type!(
        FnMatcher => gen_comm::HandlerAdaptor<GenCommHandler<'a>>,
        FnMatcher => gen_diag::HandlerAdaptor<GenDiagHandler<'a>>,
        FnMatcher => N
    )>
    | H
);

/// A matcher over a plain function of the endpoint and cluster IDs.
///
/// A context without an endpoint or without a cluster (a global operation)
/// matches; everything else is decided by the function. The function is a
/// `fn` pointer rather than a closure type so that the matcher is nameable in
/// `handler_chain_type!`; use non-capturing closures (constants are not
/// captures) to build it:
///
/// ```ignore
/// FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == DescHandler::CLUSTER.id)
/// ```
///
/// This is a newtype only because `Matcher` is not defined in this crate; once
/// the matcher moves to `rs-matter`, it becomes a type alias for the bare
/// function pointer and the wrapper at the call sites goes away.
#[derive(Debug, Copy, Clone)]
pub struct FnMatcher(pub fn(EndptId, ClusterId) -> bool);

impl Matcher for FnMatcher {
    fn matches(&self, ctx: impl MatchContext) -> bool {
        let Some(endpt_id) = ctx.endpt() else {
            return true;
        };
        let Some(cluster_id) = ctx.cluster() else {
            return true;
        };

        (self.0)(endpt_id, cluster_id)
    }
}

/// Return the user-owned system handler for the root endpoint (Endpoint 0).
///
/// The returned chain services every root endpoint system cluster except the
/// operational network ones (Network Commissioning, the network-type diagnostics
/// cluster, General Diagnostics and General Commissioning), which the stack
/// chains on top of the user handler by itself.
///
/// # Arguments:
/// - `sw_diag`: The `SwDiag` implementation (pass `&()` for the
///   no-op default: heap counters report `0`).
/// - `rand`: A random number generator.
pub fn root_handler<'a, R: RngCore>(sw_diag: &'a dyn SwDiag, mut rand: R) -> RootHandler<'a> {
    EmptyHandler
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == TimeSyncHandler::CLUSTER.id),
            TimeSyncHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == SwDiagHandler::CLUSTER.id),
            SwDiagHandler::new(Dataver::new_rand(&mut rand), sw_diag).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == GrpKeyMgmtHandler::CLUSTER.id),
            GrpKeyMgmtHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == AclHandler::CLUSTER.id),
            AclHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == NocHandler::CLUSTER.id),
            NocHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == AdminCommHandler::CLUSTER.id),
            AdminCommHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == BasicInfoHandler::CLUSTER.id),
            BasicInfoHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
        .chain(
            FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == DescHandler::CLUSTER.id),
            DescHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        )
}

/// Return the operational network handler for the root endpoint (Endpoint 0),
/// chained on top of `next`.
/// Use this handler for devices that use Ethernet as the Matter Operational Network.
///
/// # Arguments:
/// - `comm_policy`: The `CommPolicy` implementation.
/// - `gen_diag`: The `GenDiag` implementation.
/// - `netif_diag`: The `NetifDiag` implementation.
/// - `next`: The handler to chain on top of; receives everything not matched here.
/// - `rand`: A random number generator.
pub fn eth_net_handler<'a, R: RngCore, H>(
    comm_policy: &'a dyn CommPolicy,
    gen_diag: &'a dyn GenDiag,
    netif_diag: &'a dyn NetifDiag,
    next: H,
    mut rand: R,
) -> EthNetHandler<'a, H> {
    net_handler(
        comm_policy,
        gen_diag,
        netif_diag,
        EthNetCtl::new_default(),
        &AlwaysConnected,
        FnMatcher(|e, c| {
            e == ROOT_ENDPOINT_ID
                && (c == GenCommHandler::CLUSTER.id
                    || c == GenDiagHandler::CLUSTER.id
                    || c == EthDiagHandler::CLUSTER.id)
        }),
        EthDiagHandler::new(Dataver::new_rand(&mut rand)).adapt(),
        next,
        rand,
    )
}

/// Return the operational network handler for the root endpoint (Endpoint 0),
/// chained on top of `next`.
/// Use this handler for devices that use Wifi as the Matter Operational Network.
///
/// # Arguments:
/// - `comm_policy`: The `CommPolicy` implementation.
/// - `gen_diag`: The `GenDiag` implementation.
/// - `netif_diag`: The `NetifDiag` implementation.
/// - `wifi_diag`: The `WifiDiag` implementation.
/// - `net_ctl`: The `NetCtl` implementation.
/// - `next`: The handler to chain on top of; receives everything not matched here.
/// - `rand`: A random number generator.
#[allow(clippy::too_many_arguments)]
pub fn wifi_net_handler<'a, R: RngCore, T, H>(
    comm_policy: &'a dyn CommPolicy,
    gen_diag: &'a dyn GenDiag,
    netif_diag: &'a dyn NetifDiag,
    wifi_diag: &'a dyn WifiDiag,
    net_ctl: T,
    next: H,
    mut rand: R,
) -> WifiNetHandler<'a, T, H>
where
    T: NetCtl + NetCtlStatus,
{
    net_handler(
        comm_policy,
        gen_diag,
        netif_diag,
        net_ctl,
        wifi_diag,
        FnMatcher(|e, c| {
            e == ROOT_ENDPOINT_ID
                && (c == GenCommHandler::CLUSTER.id
                    || c == GenDiagHandler::CLUSTER.id
                    || c == WifiDiagHandler::CLUSTER.id)
        }),
        WifiDiagHandler::new(Dataver::new_rand(&mut rand), wifi_diag).adapt(),
        next,
        rand,
    )
}

/// Return the operational network handler for the root endpoint (Endpoint 0),
/// chained on top of `next`.
/// Use this handler for devices that use Thread as the Matter Operational Network.
///
/// # Arguments:
/// - `comm_policy`: The `CommPolicy` implementation.
/// - `gen_diag`: The `GenDiag` implementation.
/// - `netif_diag`: The `NetifDiag` implementation.
/// - `thread_diag`: The `ThreadDiag` implementation.
/// - `net_ctl`: The `NetCtl` implementation.
/// - `next`: The handler to chain on top of; receives everything not matched here.
/// - `rand`: A random number generator.
#[allow(clippy::too_many_arguments)]
pub fn thread_net_handler<'a, R: RngCore, T, H>(
    comm_policy: &'a dyn CommPolicy,
    gen_diag: &'a dyn GenDiag,
    netif_diag: &'a dyn NetifDiag,
    thread_diag: &'a dyn ThreadDiag,
    net_ctl: T,
    next: H,
    mut rand: R,
) -> ThreadNetHandler<'a, T, H>
where
    T: NetCtl + NetCtlStatus,
{
    net_handler(
        comm_policy,
        gen_diag,
        netif_diag,
        net_ctl,
        thread_diag,
        FnMatcher(|e, c| {
            e == ROOT_ENDPOINT_ID
                && (c == GenCommHandler::CLUSTER.id
                    || c == GenDiagHandler::CLUSTER.id
                    || c == ThreadDiagHandler::CLUSTER.id)
        }),
        ThreadDiagHandler::new(Dataver::new_rand(&mut rand), thread_diag).adapt(),
        next,
        rand,
    )
}

/// Return the operational network handler for the root endpoint (Endpoint 0),
/// chained on top of `next`.
///
/// Use `eth_net_handler()`, `wifi_net_handler()` or `thread_net_handler()` instead to get the
/// appropriate Network Diagnostic handler included in the handler.
///
/// `net_matcher` must match exactly General Commissioning, General Diagnostics
/// and the `netw_diag` cluster on the root endpoint.
#[allow(clippy::too_many_arguments)]
fn net_handler<'a, R: RngCore, T, N, H>(
    comm_policy: &'a dyn CommPolicy,
    gen_diag: &'a dyn GenDiag,
    netif_diag: &'a dyn NetifDiag,
    net_ctl: T,
    wireless_diag: &'a dyn WirelessDiag,
    net_matcher: FnMatcher,
    netw_diag: N,
    next: H,
    mut rand: R,
) -> NetHandler<'a, T, N, H>
where
    T: NetCtl + NetCtlStatus,
{
    ChainedHandler::new(
        net_matcher,
        Async(
            // The network diagnostics link is the bottom of the sub-chain:
            // everything the group matcher lets through that the General
            // Commissioning and General Diagnostics links above it did not
            // claim is the network diagnostics cluster, so the group matcher
            // is exact here too.
            ChainedHandler::new(net_matcher, netw_diag, EmptyHandler)
                .chain(
                    FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == GenDiagHandler::CLUSTER.id),
                    GenDiagHandler::new(Dataver::new_rand(&mut rand), gen_diag, netif_diag).adapt(),
                )
                .chain(
                    FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == GenCommHandler::CLUSTER.id),
                    GenCommHandler::new(Dataver::new_rand(&mut rand), comm_policy).adapt(),
                ),
        ),
        next,
    )
    .chain(
        FnMatcher(|e, c| e == ROOT_ENDPOINT_ID && c == NetCommHandler::<T>::CLUSTER.id),
        NetCommHandler::new(Dataver::new_rand(&mut rand), net_ctl, wireless_diag).adapt(),
    )
}

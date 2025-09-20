//! An example utilizing the `EthMatterStack` struct.
//! As the name suggests, this Matter stack assembly uses Ethernet as the main transport,
//! as well as for commissioning.
//!
//! Notice that it might be that rather than Ethernet, the actual L2 transport is Wifi.
//! From the POV of Matter - this case is indistinguishable from Ethernet as long as the
//! Matter stack is not concerned with connecting to the Wifi network, managing
//! its credentials etc. and can assume it "pre-exists".
//!
//! The example implements a fictitious Light device (an On-Off Matter cluster).
#![recursion_limit = "256"]
#![feature(maybe_uninit_uninit_array_transpose)]

use core::mem::MaybeUninit;
use core::pin::pin;

use embassy_futures::select::select;
use embassy_time::{Duration, Timer};

use env_logger::Target;
use log::info;

use rs_matter_stack::bump_alloc::BumpAlloc;
use rs_matter_stack::eth::EthMatterStack;
use rs_matter_stack::matter::dm::clusters::desc;
use rs_matter_stack::matter::dm::clusters::desc::ClusterHandler as _;
use rs_matter_stack::matter::dm::clusters::on_off;
use rs_matter_stack::matter::dm::clusters::on_off::ClusterHandler as _;
use rs_matter_stack::matter::dm::devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter_stack::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_stack::matter::dm::networks::unix::UnixNetifs;
use rs_matter_stack::matter::dm::{Async, Dataver, Endpoint, Node};
use rs_matter_stack::matter::dm::{EmptyHandler, EpClMatcher};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::utils::init::InitMaybeUninit;
use rs_matter_stack::matter::utils::select::Coalesce;
use rs_matter_stack::matter::{clusters, devices};
use rs_matter_stack::mdns::ZeroconfMdns;
use rs_matter_stack::persist::DirKvBlobStore;

use static_cell::StaticCell;

fn main() -> Result<(), Error> {
    env_logger::Builder::from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    )
    .target(Target::Stdout)
    .init();

    info!("Starting...");

    // Initialize the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK
        .uninit()
        .init_with(EthMatterStack::init_default(
            &TEST_DEV_DET,
            TEST_DEV_COMM,
            &TEST_DEV_ATT,
        ));

    // Our "light" on-off cluster.
    // Can be anything implementing `rs_matter::dm::AsyncHandler`
    let on_off = on_off::OnOffHandler::new(Dataver::new_rand(stack.matter().rand()));

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = EmptyHandler
        .chain(
            EpClMatcher::new(
                Some(LIGHT_ENDPOINT_ID),
                Some(on_off::OnOffHandler::CLUSTER.id),
            ),
            Async(on_off::HandlerAdaptor(&on_off)),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(desc::DescHandler::CLUSTER.id)),
            Async(desc::DescHandler::new(Dataver::new_rand(stack.matter().rand())).adapt()),
        );

    static ALLOC_MEM: StaticCell<[u8; 90000]> = StaticCell::new();
    let alloc_mem = ALLOC_MEM.uninit();
    let ptr = alloc_mem.as_mut_ptr() as *mut MaybeUninit<u8>;
    let p: &'static mut [MaybeUninit<u8>] = unsafe { core::slice::from_raw_parts_mut(ptr as _, 90000) };
    let bump_alloc = BumpAlloc::new(p);

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but saves some memory due to `rustc`
    // not being very intelligent w.r.t. stack usage in async functions
    let store = stack.create_shared_store(DirKvBlobStore::new_default());
    let mut matter = pin!(stack.run_preex2(
        // The Matter stack needs UDP sockets to communicate with other Matter devices
        edge_nal_std::Stack::new(),
        // Will try to find a default network interface
        UnixNetifs,
        // Will use the mDNS implementation based on the `zeroconf` crate
        ZeroconfMdns,
        // Will persist in `<tmp-dir>/rs-matter`
        &store,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user task future to run
        (),
        &bump_alloc,
    ));

    // Just for demoing purposes:
    //
    // Run a sample loop that simulates state changes triggered by the HAL
    // Changes will be properly communicated to the Matter controllers
    // (i.e. Google Home, Alexa) and other Matter devices thanks to subscriptions
    let mut device = pin!(async {
        loop {
            // Simulate user toggling the light with a physical switch every 5 seconds
            Timer::after(Duration::from_secs(5)).await;

            // Toggle
            on_off.set(!on_off.get());

            // Let the Matter stack know that we have changed
            // the state of our Light device
            stack.notify_changed();

            info!("Light toggled");
        }
    });

    // Schedule the Matter run & the device loop together
    futures_lite::future::block_on(select(&mut matter, &mut device).coalesce())
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
static MATTER_STACK: StaticCell<EthMatterStack> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EthMatterStack::<()>::root_endpoint(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters: clusters!(desc::DescHandler::CLUSTER, on_off::OnOffHandler::CLUSTER),
        },
    ],
};

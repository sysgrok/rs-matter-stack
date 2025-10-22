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

use core::pin::pin;

use log::info;

use rs_matter::dm::clusters::on_off::test::TestOnOffDeviceLogic;
use rs_matter::dm::clusters::on_off::OnOffHooks;
use rs_matter_stack::eth::EthMatterStack;
use rs_matter_stack::matter::dm::clusters::desc;
use rs_matter_stack::matter::dm::clusters::desc::ClusterHandler as _;
use rs_matter_stack::matter::dm::clusters::on_off;
use rs_matter_stack::matter::dm::devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter_stack::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_stack::matter::dm::networks::unix::UnixNetifs;
use rs_matter_stack::matter::dm::{Async, Dataver, Endpoint, Node};
use rs_matter_stack::matter::dm::{EmptyHandler, EpClMatcher};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::utils::init::InitMaybeUninit;
use rs_matter_stack::matter::{clusters, devices};
use rs_matter_stack::mdns::ZeroconfMdns;
use rs_matter_stack::persist::DirKvBlobStore;

use static_cell::StaticCell;

/// The amount of memory where all futures will be allocated.
/// This does NOT include the Matter stack itself.
///
/// The futures of `rs-matter-stack` created during the execution of the `run*` methods
/// are allocated in a special way using a small bump allocator which results
/// in a much lower memory usage by those.
///
/// If - for your platform - this size is not enough, increase it until
/// the program runs without panics during the stack initialization.
const BUMP_SIZE: usize = 40000; //20000;

fn main() -> Result<(), Error> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

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
    // It will toggle the light state every 5 seconds
    let on_off = on_off::OnOffHandler::new_standalone(
        Dataver::new_rand(stack.matter().rand()),
        LIGHT_ENDPOINT_ID,
        TestOnOffDeviceLogic::new(true),
    );

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = EmptyHandler
        .chain(
            EpClMatcher::new(
                Some(LIGHT_ENDPOINT_ID),
                Some(TestOnOffDeviceLogic::CLUSTER.id),
            ),
            on_off::HandlerAsyncAdaptor(&on_off),
        )
        // Each Endpoint needs a Descriptor cluster too
        // Just use the one that `rs-matter` provides out of the box
        .chain(
            EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(desc::DescHandler::CLUSTER.id)),
            Async(desc::DescHandler::new(Dataver::new_rand(stack.matter().rand())).adapt()),
        );

    // Create the persister & load any previously saved state
    let persist = futures_lite::future::block_on(
        stack.create_persist_with_comm_window(DirKvBlobStore::new_default()),
    )?;

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but reduces the size of the final future
    let matter = pin!(stack.run_preex(
        // The Matter stack needs UDP sockets to communicate with other Matter devices
        edge_nal_std::Stack::new(),
        // Will try to find a default network interface
        UnixNetifs,
        // Will use the mDNS implementation based on the `zeroconf` crate
        ZeroconfMdns,
        // Will persist in `<tmp-dir>/rs-matter`
        &persist,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // No user task future to run
        (),
    ));

    // Schedule the Matter run
    futures_lite::future::block_on(matter)
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
static MATTER_STACK: StaticCell<EthMatterStack<BUMP_SIZE, ()>> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    id: 0,
    endpoints: &[
        EthMatterStack::<0, ()>::root_endpoint(),
        Endpoint {
            id: LIGHT_ENDPOINT_ID,
            device_types: devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters: clusters!(desc::DescHandler::CLUSTER, TestOnOffDeviceLogic::CLUSTER),
        },
    ],
};

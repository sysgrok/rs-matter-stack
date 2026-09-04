//! An ICD (Intermittently Connected Device) variant of the `light` example.
//!
//! Same fictitious Light device as `light.rs`, but built with the `icd`
//! feature enabled: the root endpoint carries the ICD Management cluster,
//! and a small background task demonstrates driving a Check-In from the
//! app side using the primitives `MatterStack::icd()` exposes. `rs-matter-stack`
//! only owns the cluster + its persistence lifecycle - it does not decide
//! *when* to check in, so this task's "every 30s, nudge the first
//! registered client" policy is deliberately naive and only meant to show
//! the wiring; a real ICD device would drive this from its own sleep/wake
//! loop instead of a fixed timer.
#![recursion_limit = "256"]

use core::pin::pin;

use log::info;

use rs_matter_stack::ble::BluerGattPeripheral;
use rs_matter_stack::matter::crypto::{default_crypto, Crypto, RngCore};
use rs_matter_stack::matter::dm::clusters::app::on_off;
use rs_matter_stack::matter::dm::clusters::app::on_off::test::TestOnOffDeviceLogic;
use rs_matter_stack::matter::dm::clusters::app::on_off::OnOffHooks;
use rs_matter_stack::matter::dm::clusters::desc::{ClusterHandler as _, DescHandler};
use rs_matter_stack::matter::dm::clusters::icd_mgmt::IcdModeConfig;
use rs_matter_stack::matter::dm::clusters::net_comm::NetworkType;
use rs_matter_stack::matter::dm::devices::test::DAC_PRIVKEY;
use rs_matter_stack::matter::dm::devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter_stack::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
use rs_matter_stack::matter::dm::networks::unix::UnixNetifs;
use rs_matter_stack::matter::dm::networks::wireless::NoopWirelessNetCtl;
use rs_matter_stack::matter::dm::{Async, Dataver, Endpoint, Node};
use rs_matter_stack::matter::dm::{EmptyHandler, EpClMatcher};
use rs_matter_stack::matter::error::Error;
use rs_matter_stack::matter::persist::DirKvBlobStore;
use rs_matter_stack::matter::transport::network::mdns::zeroconf::ZeroconfMdns;
use rs_matter_stack::matter::utils::init::InitMaybeUninit;
use rs_matter_stack::matter::{clusters, devices};
use rs_matter_stack::wireless::PreexistingWireless;
use rs_matter_stack::wireless::WifiMatterStack;

use static_cell::StaticCell;

/// See `light.rs` for what this is for.
const BUMP_SIZE: usize = 23500;

/// This device's advertised ICD mode timings.
///
/// - Stays idle for up to 60s between wake-ups.
/// - Once awake, stays active for at least 1s.
/// - The threshold below which network activity alone keeps it active: 1s
///   (this is also the Check-In application payload).
/// - No `UserActiveModeTriggerHint`: this fictitious light has no physical
///   trigger (button, NFC, ...) a user could use to wake it.
const ICD_MODE: IcdModeConfig = IcdModeConfig {
    idle_mode_duration_s: 60,
    active_mode_duration_ms: 1000,
    active_mode_threshold_ms: 1000,
    user_active_mode_trigger_hint: 0,
    user_active_mode_trigger_instruction: "",
};

fn main() -> Result<(), Error> {
    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

    info!("Starting...");

    // The default crypto provider - also our source of the one-time Check-In
    // counter seed (real entropy, read once at boot).
    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let mut rand = crypto.weak_rand()?;
    let icd_counter_seed = rand.next_u32();

    // Initialize the Matter stack (can be done only once),
    // as we'll run it in this thread
    let stack = MATTER_STACK.uninit().init_with(WifiMatterStack::init(
        &TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        ICD_MODE,
        icd_counter_seed,
    ));

    // Our "light" on-off cluster.
    // It will toggle the light state every 5 seconds
    let on_off = on_off::OnOffHandler::new_standalone(
        Dataver::new_rand(&mut rand),
        LIGHT_ENDPOINT_ID,
        TestOnOffDeviceLogic::new(true),
    );

    // Chain our endpoint clusters with the
    // (root) Endpoint 0 system clusters in the final handler
    let handler = EmptyHandler
        // Our on-off cluster, on Endpoint 1
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
            EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(DescHandler::CLUSTER.id)),
            Async(DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
        );

    // Create the KV BLOB store and load any previously saved state of `rs-matter`
    // (this is also where the ICD registrations and Check-In counter get re-hydrated from).
    let mut store = DirKvBlobStore::new_default();
    futures_lite::future::block_on(stack.startup(&crypto, &mut store))?;

    // Wrap the KV BLOB store as a shared reference, so that it can be used both by `rs-matter` and the user
    let kv = stack.matter().kv(store);

    // Run the Matter stack with our handler
    // Using `pin!` is completely optional, but reduces the size of the final future
    let matter = pin!(stack.run_coex(
        PreexistingWireless::new(
            // The Matter stack needs UDP sockets to communicate with other Matter devices
            edge_nal_std::Stack::new(),
            // Will try to find a default network interface
            UnixNetifs,
            // A dummy wireless controller that does nothing
            NoopWirelessNetCtl::new(NetworkType::Wifi),
            // Will use the mDNS implementation based on the `zeroconf` crate
            ZeroconfMdns::new(),
            // The Bluetooth transport implementation based on the `bluer` crate.
            BluerGattPeripheral::new(None),
        ),
        // The crypto provider, used for all the cryptographic operations of the stack
        &crypto,
        // Our `AsyncHandler` + `AsyncMetadata` impl
        (NODE, handler),
        // Will persist in `<tmp-dir>/rs-matter`
        kv,
        // Our background task, driving Check-Ins for as long as the operational
        // network is up. `MatterStack` only owns the cluster + persistence; it
        // is entirely up to us to decide when a Check-In actually goes out.
        CheckInTask {
            stack,
            crypto: &crypto,
        },
    ));

    // Schedule the Matter run & the device loop together
    futures_lite::future::block_on(async_compat::Compat::new(matter))
}

/// Every 30s, nudge the first registered Check-In client (if any) - a
/// deliberately naive stand-in for "the device decided it's safe to sleep
/// and something might have missed it". A real sleepy device would call
/// this from its own wake logic instead of a fixed timer, and would likely
/// want `Icd::send_check_in` (which nudges every client with no live
/// subscription) rather than always targeting the first registration.
struct CheckInTask<'a, C> {
    stack: &'a WifiMatterStack<'a, BUMP_SIZE>,
    crypto: &'a C,
}

impl<C> rs_matter_stack::UserTask for CheckInTask<'_, C>
where
    C: Crypto,
{
    async fn run<S, N>(&mut self, _net_stack: S, _netif: N) -> Result<(), Error>
    where
        S: rs_matter_stack::nal::NetStack,
        N: rs_matter_stack::matter::dm::clusters::gen_diag::NetifDiag
            + rs_matter_stack::matter::dm::networks::NetChangeNotif,
    {
        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(30)).await;

            let target = self
                .stack
                .icd()
                .with_registrations(|regs| regs.first().map(|r| (r.fab_idx, r.check_in_node_id)));

            if let Some((fab_idx, node_id)) = target {
                let counter = self.stack.icd().next_counter();
                let mut buf = [0u8; 64];

                match self
                    .stack
                    .icd()
                    .send_one_check_in(
                        self.stack.matter(),
                        self.crypto,
                        fab_idx,
                        node_id,
                        counter,
                        &mut buf,
                    )
                    .await
                {
                    Ok(()) => info!("Sent a Check-In to fabric {fab_idx}, node {node_id:x}"),
                    Err(e) => log::warn!("Check-In failed: {e:?}"),
                }
            }
        }
    }
}

/// The Matter stack is allocated statically to avoid
/// program stack blowups.
/// It is also a mandatory requirement when the `WifiBle` stack variation is used.
static MATTER_STACK: StaticCell<WifiMatterStack<BUMP_SIZE>> = StaticCell::new();

/// Endpoint 0 (the root endpoint) always runs
/// the hidden Matter system clusters, so we pick ID=1
const LIGHT_ENDPOINT_ID: u16 = 1;

/// The Matter Light device Node
const NODE: Node = Node {
    endpoints: &[
        WifiMatterStack::<0, ()>::root_endpoint(),
        Endpoint::new(
            LIGHT_ENDPOINT_ID,
            devices!(DEV_TYPE_ON_OFF_LIGHT),
            clusters!(DescHandler::CLUSTER, TestOnOffDeviceLogic::CLUSTER),
        ),
    ],
};

use core::future::Future;

use rs_matter::error::Error;
use rs_matter::transport::network::btp::{AdvData, Btp};

#[cfg(all(feature = "os", target_os = "linux"))]
pub use bluer::*;
#[cfg(feature = "zbus")]
pub use bluez::*;

/// A trait modeling a platform-specific / BLE-stack specific GATT peripheral.
///
/// Running the peripheral means it would advertise the Matter BTP service
/// and then - once a peer connects - will drive the passed `Btp` instance
/// via write requests and indications to the Matter C1 and C2 characteristics.
pub trait GattPeripheral {
    /// Run the Matter GATT app
    ///
    /// # Arguments
    /// - `btp`: the BTP instance to drive with the GATT app
    /// - `service_name`: the name to advertise for the BTP service
    /// - `service_adv`: the advertisement data to use for the BTP service
    async fn run(
        &mut self,
        btp: &Btp,
        service_name: &str,
        service_adv: &AdvData,
    ) -> Result<(), Error>;
}

impl<T> GattPeripheral for &mut T
where
    T: GattPeripheral,
{
    fn run(
        &mut self,
        btp: &Btp,
        service_name: &str,
        service_adv: &AdvData,
    ) -> impl Future<Output = Result<(), Error>> {
        (*self).run(btp, service_name, service_adv)
    }
}

#[cfg(all(feature = "os", target_os = "linux"))]
#[allow(clippy::large_futures)]
mod bluer {
    use rs_matter::error::Error;
    use rs_matter::transport::network::btp::{AdvData, Btp};

    pub struct BluerGattPeripheral<'a>(Option<&'a str>);

    impl<'a> BluerGattPeripheral<'a> {
        /// Create a new `BluerGattPeripheral` that will use the Bluetooth adapter with the given name.
        /// If `adapter_name` is `None`, the default adapter will be used.
        pub const fn new(adapter_name: Option<&'a str>) -> Self {
            Self(adapter_name)
        }
    }

    impl super::GattPeripheral for BluerGattPeripheral<'_> {
        async fn run(
            &mut self,
            btp: &Btp,
            service_name: &str,
            service_adv: &AdvData,
        ) -> Result<(), Error> {
            rs_matter::transport::network::btp::bluer::run_peripheral(
                self.0,
                service_name,
                service_adv,
                btp,
            )
            .await
        }
    }
}

#[cfg(feature = "zbus")]
#[allow(clippy::large_futures)]
mod bluez {
    use rs_matter::error::Error;
    use rs_matter::transport::network::btp::{AdvData, Btp};
    use rs_matter::utils::zbus::Connection;

    pub struct BluezGattPeripheral<'a> {
        connection: &'a Connection,
        adapter_name: Option<&'a str>,
    }

    impl<'a> BluezGattPeripheral<'a> {
        /// Create a new `BluezGattPeripheral` that will use the Bluetooth adapter with the given name.
        /// If `adapter_name` is `None`, the default adapter will be used.
        pub const fn new(connection: &'a Connection, adapter_name: Option<&'a str>) -> Self {
            Self {
                connection,
                adapter_name,
            }
        }
    }

    impl super::GattPeripheral for BluezGattPeripheral<'_> {
        async fn run(
            &mut self,
            btp: &Btp,
            service_name: &str,
            service_adv: &AdvData,
        ) -> Result<(), Error> {
            rs_matter::transport::network::btp::bluez::run_peripheral(
                self.connection,
                self.adapter_name,
                service_name,
                service_adv,
                btp,
            )
            .await
        }
    }
}

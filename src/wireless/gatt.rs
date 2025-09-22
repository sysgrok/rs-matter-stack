use core::future::Future;

use rs_matter::error::Error;
use rs_matter::transport::network::btp::GattPeripheral;

use super::PreexistingWireless;

/// A trait representing a task that needs access to the BLE GATT peripheral to perform its work
/// (e.g. the first part of a non-concurrent commissioning flow)
pub trait GattTask {
    /// Run the task with the given GATT peripheral
    async fn run<P>(&mut self, peripheral: P) -> Result<(), Error>
    where
        P: GattPeripheral;
}

impl<T> GattTask for &mut T
where
    T: GattTask,
{
    fn run<P>(&mut self, peripheral: P) -> impl Future<Output = Result<(), Error>>
    where
        P: GattPeripheral,
    {
        T::run(*self, peripheral)
    }
}

/// A trait for running a task within a context where the BLE peripheral is initialized and operable
/// (e.g. the first part of a non-concurrent commissioning workflow)
pub trait Gatt {
    /// Setup the radio to operate in wireless (Wifi or Thread) mode
    /// and run the given task
    async fn run<T>(&mut self, task: T) -> Result<(), Error>
    where
        T: GattTask;
}

impl<T> Gatt for &mut T
where
    T: Gatt,
{
    fn run<A>(&mut self, task: A) -> impl Future<Output = Result<(), Error>>
    where
        A: GattTask,
    {
        T::run(self, task)
    }
}

impl<U, N, C, M, P> Gatt for PreexistingWireless<U, N, C, M, P>
where
    P: GattPeripheral,
{
    async fn run<T>(&mut self, mut task: T) -> Result<(), Error>
    where
        T: GattTask,
    {
        task.run(&mut self.gatt).await
    }
}

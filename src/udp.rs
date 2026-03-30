//! UDP transport implementation for edge-nal

use core::fmt::Debug;
use core::net::IpAddr;

use edge_nal::{MulticastV6, Readable, UdpReceive, UdpSend};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::{Address, NetworkMulticast, NetworkReceive, NetworkSend};
use rs_matter::utils::sync::IfMutex;

/// UDP transport implementation for edge-nal
pub struct Udp<T>(pub T);

impl<T> NetworkSend for Udp<T>
where
    T: UdpSend,
{
    async fn send_to(&mut self, data: &[u8], addr: Address) -> Result<(), Error> {
        if let Address::Udp(remote) = addr {
            self.0.send(remote, data).await.map_err(map_err)?;

            Ok(())
        } else {
            Err(ErrorCode::NoNetworkInterface.into())
        }
    }
}

impl<T> NetworkReceive for Udp<T>
where
    T: UdpReceive + Readable,
{
    async fn wait_available(&mut self) -> Result<(), Error> {
        self.0.readable().await.map_err(map_err)?;

        Ok(())
    }

    async fn recv_from(&mut self, buffer: &mut [u8]) -> Result<(usize, Address), Error> {
        let (size, addr) = self.0.receive(buffer).await.map_err(map_err)?;

        Ok((size, Address::Udp(addr)))
    }
}

impl<T> NetworkMulticast for Udp<(T, u32)>
where
    T: MulticastV6,
{
    async fn join(&mut self, addr: IpAddr) -> Result<(), Error> {
        match addr {
            IpAddr::V6(remote) => self.0 .0.join_v6(remote, self.0 .1).await.map_err(map_err),
            IpAddr::V4(remote) => {
                warn!("IPv4 multicast is not supported: {:?}", remote);
                Ok(())
            }
        }
    }

    async fn leave(&mut self, addr: IpAddr) -> Result<(), Error> {
        match addr {
            IpAddr::V6(remote) => self.0 .0.leave_v6(remote, self.0 .1).await.map_err(map_err),
            IpAddr::V4(remote) => {
                warn!("IPv4 multicast is not supported: {:?}", remote);
                Ok(())
            }
        }
    }
}

/// UDP send transport implementation for edge-nal where the send half of the socket is protected by a mutex
pub struct UdpSharedSend<'a, T>(pub &'a IfMutex<T>);

impl<T> NetworkSend for UdpSharedSend<'_, T>
where
    T: UdpSend,
{
    async fn send_to(&mut self, data: &[u8], addr: Address) -> Result<(), Error> {
        let mut socket = self.0.lock().await;

        if let Address::Udp(remote) = addr {
            socket.send(remote, data).await.map_err(map_err)?;

            Ok(())
        } else {
            Err(ErrorCode::NoNetworkInterface.into())
        }
    }
}

/// UDP multicast transport implementation for edge-nal where the multicast operations are protected by a mutex
pub struct UdpSharedMulticast<'a, T>(pub &'a IfMutex<T>, pub u32);

impl<T> NetworkMulticast for UdpSharedMulticast<'_, T>
where
    T: MulticastV6,
{
    async fn join(&mut self, addr: IpAddr) -> Result<(), Error> {
        let mut socket = self.0.lock().await;

        match addr {
            IpAddr::V6(remote) => socket.join_v6(remote, self.1).await.map_err(map_err),
            IpAddr::V4(remote) => {
                warn!("IPv4 multicast is not supported: {:?}", remote);
                Ok(())
            }
        }
    }

    async fn leave(&mut self, addr: IpAddr) -> Result<(), Error> {
        let mut socket = self.0.lock().await;

        match addr {
            IpAddr::V6(remote) => socket.leave_v6(remote, self.1).await.map_err(map_err),
            IpAddr::V4(remote) => {
                warn!("IPv4 multicast is not supported: {:?}", remote);
                Ok(())
            }
        }
    }
}

fn map_err<E: Debug>(e: E) -> Error {
    warn!("Network error: {:?}", debug2format!(e));
    ErrorCode::StdIoError.into() // TODO
}

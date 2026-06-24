//! UDP transport implementation for edge-nal

use core::fmt::Debug;
use core::net::{IpAddr, Ipv4Addr};

use edge_nal::{MulticastV4, MulticastV6, Readable, UdpReceive, UdpSend};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::{Address, NetworkMulticast, NetworkReceive, NetworkSend};

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

pub struct Multicast<M4, M6> {
    m4: M4,
    m4_network: Ipv4Addr,
    m6: M6,
    m6_interface: u32,
}

impl<M4, M6> Multicast<M4, M6>
where
    M4: MulticastV4,
    M6: MulticastV6,
{
    pub const fn new(m4: M4, m4_network: Ipv4Addr, m6: M6, m6_interface: u32) -> Self {
        Self {
            m4,
            m4_network,
            m6,
            m6_interface,
        }
    }
}

impl<M4, M6> NetworkMulticast for Udp<Multicast<M4, M6>>
where
    M4: MulticastV4,
    M6: MulticastV6,
{
    async fn join(&mut self, addr: IpAddr) -> Result<(), Error> {
        match addr {
            IpAddr::V4(remote) => self
                .0
                .m4
                .join_v4(remote, self.0.m4_network)
                .await
                .map_err(map_err),
            IpAddr::V6(remote) => self
                .0
                .m6
                .join_v6(remote, self.0.m6_interface)
                .await
                .map_err(map_err),
        }
    }

    async fn leave(&mut self, addr: IpAddr) -> Result<(), Error> {
        match addr {
            IpAddr::V4(remote) => self
                .0
                .m4
                .leave_v4(remote, self.0.m4_network)
                .await
                .map_err(map_err),
            IpAddr::V6(remote) => self
                .0
                .m6
                .leave_v6(remote, self.0.m6_interface)
                .await
                .map_err(map_err),
        }
    }
}

fn map_err<E: Debug>(e: E) -> Error {
    warn!("Network error: {:?}", debug2format!(e));
    ErrorCode::StdIoError.into() // TODO
}

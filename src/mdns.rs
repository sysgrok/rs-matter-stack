use core::fmt::Debug;
use core::future::Future;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

use edge_nal::{UdpBind, UdpSplit};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::mdns::builtin::BuiltinMdnsResponder;
use rs_matter::Matter;

use crate::udp;

/// A trait for running an mDNS responder.
pub trait Mdns {
    /// Run the mDNS responder with the given UDP binding, MAC address, IPv4 and IPv6 addresses, and interface index.
    ///
    /// NOTE: This trait might change once `rs-matter` starts supporting mDNS resolvers
    async fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        udp: U,
        mac: &[u8],
        ipv4: Ipv4Addr,
        ipv6: Ipv6Addr,
        interface: u32,
    ) -> Result<(), Error>
    where
        U: UdpBind;
}

impl<T> Mdns for &mut T
where
    T: Mdns,
{
    fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        udp: U,
        mac: &[u8],
        ipv4: Ipv4Addr,
        ipv6: Ipv6Addr,
        interface: u32,
    ) -> impl Future<Output = Result<(), Error>>
    where
        U: UdpBind,
    {
        (*self).run(matter, udp, mac, ipv4, ipv6, interface)
    }
}

/// A built-in mDNS responder for Matter, utilizing the `rs-matter` built-in mDNS implementation.
pub struct BuiltinMdns;

impl BuiltinMdns {
    /// A utility to prep and run the built-in `rs-matter` mDNS responder for Matter via the `edge-nal` UDP traits.
    ///
    /// Arguments:
    /// - `matter`: A reference to the `Matter` instance.
    /// - `udp`: An object implementing the `UdpBind` trait for binding UDP sockets.
    /// - `mac`: The MAC address of the host, used to generate the hostname.
    /// - `ipv4`: The IPv4 address of the host.
    /// - `ipv6`: The IPv6 address of the host.
    /// - `interface`: The interface index for the host, used for IPv6 multicast.
    pub async fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        udp: U,
        mac: &[u8],
        ipv4: Ipv4Addr,
        ipv6: Ipv6Addr,
        interface: u32,
    ) -> Result<(), Error>
    where
        U: UdpBind,
    {
        use core::fmt::Write as _;

        use edge_nal::{MulticastV4, MulticastV6};

        use rs_matter::transport::network::mdns::builtin::Host;
        use rs_matter::transport::network::mdns::{
            MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_PORT,
        };

        let mut socket = udp
            .bind(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::UNSPECIFIED,
                MDNS_PORT,
                0,
                0,
            )))
            .await
            .map_err(map_err)?;

        socket
            .join_v4(MDNS_IPV4_BROADCAST_ADDR, ipv4)
            .await
            .map_err(map_err)?;

        socket
            .join_v6(MDNS_IPV6_BROADCAST_ADDR, interface)
            .await
            .map_err(map_err)?;

        let (recv, send) = socket.split();

        let mut hostname = heapless::String::<16>::new();
        if mac.len() == 6 {
            write_unwrap!(
                hostname,
                "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                mac[0],
                mac[1],
                mac[2],
                mac[3],
                mac[4],
                mac[5]
            );
        } else if mac.len() == 8 {
            write_unwrap!(
                hostname,
                "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                mac[0],
                mac[1],
                mac[2],
                mac[3],
                mac[4],
                mac[5],
                mac[6],
                mac[7]
            );
        } else {
            panic!("Invalid MAC address length: should be 6 or 8 bytes");
        }

        BuiltinMdnsResponder::new(matter)
            .run(
                udp::Udp(send),
                udp::Udp(recv),
                &Host {
                    id: 0,
                    hostname: &hostname,
                    ip: ipv4,
                    ipv6,
                },
                Some(ipv4),
                Some(interface),
            )
            .await?;

        Ok(())
    }
}

impl Mdns for BuiltinMdns {
    fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        udp: U,
        mac: &[u8],
        ipv4: Ipv4Addr,
        ipv6: Ipv6Addr,
        interface: u32,
    ) -> impl Future<Output = Result<(), Error>>
    where
        U: UdpBind,
    {
        Self::run(self, matter, udp, mac, ipv4, ipv6, interface)
    }
}

fn map_err<E: Debug>(e: E) -> Error {
    warn!("mDNS network error: {:?}", debug2format!(e));
    ErrorCode::StdIoError.into() // TODO
}

/// An mDNS responder for Matter using the Avahi zbus mDNS implementation.
#[cfg(feature = "zbus")]
pub struct AvahiMdns<'a> {
    connection: &'a rs_matter::utils::zbus::Connection,
}

#[cfg(feature = "zbus")]
impl<'a> AvahiMdns<'a> {
    /// Create a new instance of the Avahi mDNS responder.
    pub const fn new(connection: &'a rs_matter::utils::zbus::Connection) -> Self {
        Self { connection }
    }

    pub async fn run(&mut self, matter: &Matter<'_>) -> Result<(), Error> {
        rs_matter::transport::network::mdns::avahi::AvahiMdnsResponder::new(matter)
            .run(self.connection)
            .await
    }
}

#[cfg(feature = "zbus")]
impl Mdns for AvahiMdns<'_> {
    fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        _udp: U,
        _mac: &[u8],
        _ipv4: Ipv4Addr,
        _ipv6: Ipv6Addr,
        _interface: u32,
    ) -> impl Future<Output = Result<(), Error>>
    where
        U: UdpBind,
    {
        Self::run(self, matter)
    }
}

/// An mDNS responder for Matter using the systemd-resolved zbus mDNS implementation.
#[cfg(feature = "zbus")]
pub struct ResolveMdns<'a> {
    connection: &'a rs_matter::utils::zbus::Connection,
}

#[cfg(feature = "zbus")]
impl<'a> ResolveMdns<'a> {
    /// Create a new instance of the systemd-resolved mDNS responder.
    pub const fn new(connection: &'a rs_matter::utils::zbus::Connection) -> Self {
        Self { connection }
    }

    pub async fn run(&mut self, matter: &Matter<'_>) -> Result<(), Error> {
        rs_matter::transport::network::mdns::resolve::ResolveMdnsResponder::new(matter)
            .run(self.connection)
            .await
    }
}

#[cfg(feature = "zbus")]
impl Mdns for ResolveMdns<'_> {
    fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        _udp: U,
        _mac: &[u8],
        _ipv4: Ipv4Addr,
        _ipv6: Ipv6Addr,
        _interface: u32,
    ) -> impl Future<Output = Result<(), Error>>
    where
        U: UdpBind,
    {
        Self::run(self, matter)
    }
}

/// An mDNS responder for Matter using the `zeroconf` crate.
#[cfg(feature = "zeroconf")]
pub struct ZeroconfMdns;

#[cfg(feature = "zeroconf")]
impl ZeroconfMdns {
    pub async fn run(&mut self, matter: &Matter<'_>) -> Result<(), Error> {
        rs_matter::transport::network::mdns::zeroconf::ZeroconfMdnsResponder::new(matter)
            .run()
            .await
    }
}

#[cfg(feature = "zeroconf")]
impl Mdns for ZeroconfMdns {
    fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        _udp: U,
        _mac: &[u8],
        _ipv4: Ipv4Addr,
        _ipv6: Ipv6Addr,
        _interface: u32,
    ) -> impl Future<Output = Result<(), Error>>
    where
        U: UdpBind,
    {
        Self::run(self, matter)
    }
}

/// An mDNS responder for Matter using the `astro-dnssd` crate.
#[cfg(feature = "astro-dnssd")]
pub struct AstroMdns;

#[cfg(feature = "astro-dnssd")]
impl AstroMdns {
    pub async fn run(&mut self, matter: &Matter<'_>) -> Result<(), Error> {
        rs_matter::transport::network::mdns::astro::AstroMdnsResponder::new(matter)
            .run()
            .await
    }
}

#[cfg(feature = "astro-dnssd")]
impl Mdns for AstroMdns {
    async fn run<U>(
        &mut self,
        matter: &Matter<'_>,
        _udp: U,
        _mac: &[u8],
        _ipv4: Ipv4Addr,
        _ipv6: Ipv6Addr,
        _interface: u32,
    ) -> Result<(), Error>
    where
        U: UdpBind,
    {
        Self::run(self, matter).await
    }
}

/// Utilities for using `edge-mdns` as an mDNS responder for `rs-matter`.
///
/// `rs-matter` does have a built-in mDNS imlementation, and that implementation is
/// in fact primarily maintained by the same author who maintains `edge-mdns`.
///
/// However, the key difference between the two is that the `rs-matter` built-in mDNS
/// implementation - _for now_ -_only_ responds to queries which concern the Matter
/// protocol itself and also does not expose a query interface. This makes it unsuitable
/// for use in cases where the same host that operates an `rs-matter` stack needs to -
/// for whatever reasons - to host additional service types different than the ones
/// concerning Matter, and/or issue ad-hoc mDNS queries outside the queries necessary
/// for operating the Matter stack.
///
/// Using `edge-mdns` solves this problem by providing a general-purpose mDNS which can be
/// shared between the `rs-matter` stack and other - user-specific use cases.
#[cfg(feature = "edge-mdns")]
pub mod edge_mdns {
    use rs_matter::error::Error;
    use rs_matter::Matter;

    /// An adaptor from `rs-matter` buffers to `edge-mdns` buffers.
    pub struct MatterBuffer<B>(B);

    impl<B> MatterBuffer<B> {
        /// Create a new instance of `MatterBuffer`
        pub const fn new(buffer: B) -> Self {
            Self(buffer)
        }
    }

    impl<B, T> edge_mdns::buf::BufferAccess<T> for MatterBuffer<B>
    where
        B: rs_matter::utils::storage::pooled::BufferAccess<T>,
        T: ?Sized,
    {
        type Buffer<'a>
            = B::Buffer<'a>
        where
            Self: 'a;

        async fn get(&self) -> Option<Self::Buffer<'_>> {
            self.0.get().await
        }
    }

    /// Visit all mDNS services registered by `rs-matter` as `edge_mdns::host::Service` instances.
    pub fn emdns_services<T>(matter: &Matter<'_>, mut visitor: T) -> Result<(), Error>
    where
        T: FnMut(&edge_mdns::host::Service) -> Result<(), Error>,
    {
        matter.mdns_services(|matter_service| {
            rs_matter::transport::network::mdns::Service::call_with(
                &matter_service,
                matter.dev_det(),
                matter.port(),
                |service| {
                    let service = edge_mdns::host::Service {
                        name: service.name,
                        service: service.service,
                        protocol: service.protocol,
                        port: service.port,
                        service_subtypes: service.service_subtypes,
                        txt_kvs: service.txt_kvs,
                        priority: 0,
                        weight: 0,
                    };

                    visitor(&service)
                },
            )
        })
    }
}

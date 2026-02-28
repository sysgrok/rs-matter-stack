/// Re-export the `edge-nal` crate
pub use edge_nal::*;

/// Re-export the `edge-nal-std` crate
#[cfg(feature = "std")]
pub mod std {
    pub use edge_nal_std::*;

    impl super::NetStack for Stack {
        type UdpBind<'a>
            = &'a Stack
        where
            Self: 'a;
        type UdpConnect<'a>
            = &'a Stack
        where
            Self: 'a;

        type TcpBind<'a>
            = &'a Stack
        where
            Self: 'a;
        type TcpConnect<'a>
            = &'a Stack
        where
            Self: 'a;

        type Dns<'a>
            = &'a Stack
        where
            Self: 'a;

        fn udp_bind(&self) -> Option<Self::UdpBind<'_>> {
            Some(self)
        }

        fn udp_connect(&self) -> Option<Self::UdpConnect<'_>> {
            Some(self)
        }

        fn tcp_bind(&self) -> Option<Self::TcpBind<'_>> {
            Some(self)
        }

        fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>> {
            Some(self)
        }

        fn dns(&self) -> Option<Self::Dns<'_>> {
            Some(self)
        }
    }
}

/// Noop implementations for the `edge-nal` traits.
/// All of these panic when called
// TODO: Move to `edge-nal`
pub mod noop {
    use core::convert::Infallible;
    use core::net::{IpAddr, Ipv4Addr, SocketAddr};

    use edge_nal::io::{ErrorType, Read, Write};
    use edge_nal::{
        AddrType, Close, Dns, MulticastV4, MulticastV6, Readable, TcpAccept, TcpBind, TcpConnect,
        TcpShutdown, TcpSplit, UdpBind, UdpConnect, UdpReceive, UdpSend, UdpSplit,
    };

    /// A type that implements all `edge-nal` traits but does not support any operation
    /// and panics when any method is called.
    pub struct NoopNet;

    impl UdpBind for NoopNet {
        type Error = Infallible;

        type Socket<'a>
            = NoopNet
        where
            Self: 'a;

        async fn bind(&self, _local: SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
            panic!("UDP bind not supported")
        }
    }

    impl UdpConnect for NoopNet {
        type Error = Infallible;

        type Socket<'a>
            = NoopNet
        where
            Self: 'a;

        async fn connect(
            &self,
            _local: SocketAddr,
            _remote: SocketAddr,
        ) -> Result<Self::Socket<'_>, Self::Error> {
            panic!("UDP connect not supported")
        }
    }

    impl TcpBind for NoopNet {
        type Error = Infallible;

        type Accept<'a>
            = NoopNet
        where
            Self: 'a;

        async fn bind(&self, _local: SocketAddr) -> Result<Self::Accept<'_>, Self::Error> {
            panic!("TCP bind not supported")
        }
    }

    impl TcpAccept for NoopNet {
        type Error = Infallible;

        type Socket<'a>
            = NoopNet
        where
            Self: 'a;

        async fn accept(&self) -> Result<(SocketAddr, Self::Socket<'_>), Self::Error> {
            panic!("TCP accept not supported")
        }
    }

    impl TcpConnect for NoopNet {
        type Error = Infallible;

        type Socket<'a>
            = NoopNet
        where
            Self: 'a;

        async fn connect(&self, _remote: SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
            panic!("TCP connect not supported")
        }
    }

    impl Dns for NoopNet {
        type Error = Infallible;

        async fn get_host_by_name(
            &self,
            _host: &str,
            _addr_type: AddrType,
        ) -> Result<IpAddr, Self::Error> {
            panic!("DNS get_host_by_name not supported")
        }

        async fn get_host_by_address(
            &self,
            _addr: IpAddr,
            _result: &mut [u8],
        ) -> Result<usize, Self::Error> {
            panic!("DNS get_host_by_address not supported")
        }
    }

    impl ErrorType for NoopNet {
        type Error = Infallible;
    }

    impl Readable for NoopNet {
        async fn readable(&mut self) -> Result<(), Self::Error> {
            panic!("Readable not supported")
        }
    }

    impl UdpSend for NoopNet {
        async fn send(&mut self, _remote: SocketAddr, _data: &[u8]) -> Result<(), Self::Error> {
            panic!("UDP send not supported")
        }
    }

    impl UdpReceive for NoopNet {
        async fn receive(
            &mut self,
            _buffer: &mut [u8],
        ) -> Result<(usize, SocketAddr), Self::Error> {
            panic!("UDP receive not supported")
        }
    }

    impl UdpSplit for NoopNet {
        type Receive<'a> = Self;
        type Send<'a> = Self;

        fn split(&mut self) -> (Self::Send<'_>, Self::Receive<'_>) {
            panic!("UDP split not supported")
        }
    }

    impl MulticastV4 for NoopNet {
        async fn join_v4(
            &mut self,
            _multicast_addr: Ipv4Addr,
            _interface: Ipv4Addr,
        ) -> Result<(), Self::Error> {
            panic!("Multicast join not supported")
        }

        async fn leave_v4(
            &mut self,
            _multicast_addr: Ipv4Addr,
            _interface: Ipv4Addr,
        ) -> Result<(), Self::Error> {
            panic!("Multicast leave not supported")
        }
    }

    impl MulticastV6 for NoopNet {
        async fn join_v6(
            &mut self,
            _multicast_addr: core::net::Ipv6Addr,
            _interface: u32,
        ) -> Result<(), Self::Error> {
            panic!("Multicast join not supported")
        }

        async fn leave_v6(
            &mut self,
            _multicast_addr: core::net::Ipv6Addr,
            _interface: u32,
        ) -> Result<(), Self::Error> {
            panic!("Multicast leave not supported")
        }
    }

    impl Read for NoopNet {
        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
            panic!("Read not supported")
        }
    }

    impl Write for NoopNet {
        async fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
            panic!("Write not supported")
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            panic!("Flush not supported")
        }
    }

    impl TcpSplit for NoopNet {
        type Read<'a> = Self;
        type Write<'a> = Self;

        fn split(&mut self) -> (Self::Write<'_>, Self::Read<'_>) {
            panic!("TCP split not supported")
        }
    }

    impl TcpShutdown for NoopNet {
        async fn close(&mut self, _what: Close) -> Result<(), Self::Error> {
            panic!("TCP shutdown not supported")
        }

        async fn abort(&mut self) -> Result<(), Self::Error> {
            panic!("TCP abort not supported")
        }
    }
}

/// The network stack used by `rs-matter-stack`
///
/// Depending on the platform and features enabled, the stack
/// might or might not support certain protocols.
pub trait NetStack {
    type UdpBind<'a>: UdpBind
    where
        Self: 'a;
    type UdpConnect<'a>: UdpConnect
    where
        Self: 'a;

    type TcpBind<'a>: TcpBind
    where
        Self: 'a;
    type TcpConnect<'a>: TcpConnect
    where
        Self: 'a;

    type Dns<'a>: Dns
    where
        Self: 'a;

    /// Return a UDP Bind implementation, if supported by the network stack
    fn udp_bind(&self) -> Option<Self::UdpBind<'_>>;

    /// Return a UDP Connect implementation, if supported by the network stack
    fn udp_connect(&self) -> Option<Self::UdpConnect<'_>>;

    /// Return a TCP Bind implementation, if supported by the network stack
    fn tcp_bind(&self) -> Option<Self::TcpBind<'_>>;

    /// Return a TCP Connect implementation, if supported by the network stack
    fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>>;

    /// Return a DNS implementation, if supported by the network stack
    fn dns(&self) -> Option<Self::Dns<'_>>;
}

impl<T> NetStack for &T
where
    T: NetStack,
{
    type UdpBind<'a>
        = T::UdpBind<'a>
    where
        Self: 'a;
    type UdpConnect<'a>
        = T::UdpConnect<'a>
    where
        Self: 'a;

    type TcpBind<'a>
        = T::TcpBind<'a>
    where
        Self: 'a;
    type TcpConnect<'a>
        = T::TcpConnect<'a>
    where
        Self: 'a;

    type Dns<'a>
        = T::Dns<'a>
    where
        Self: 'a;

    fn udp_bind(&self) -> Option<Self::UdpBind<'_>> {
        (*self).udp_bind()
    }

    fn udp_connect(&self) -> Option<Self::UdpConnect<'_>> {
        (*self).udp_connect()
    }

    fn tcp_bind(&self) -> Option<Self::TcpBind<'_>> {
        (*self).tcp_bind()
    }

    fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>> {
        (*self).tcp_connect()
    }

    fn dns(&self) -> Option<Self::Dns<'_>> {
        (*self).dns()
    }
}

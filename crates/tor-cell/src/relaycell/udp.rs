//! Encoding and decoding for relay messages
//!
//! Relay messages are sent along circuits, inside RELAY or RELAY_EARLY
//! cells.

use super::msg;
use crate::chancell::CELL_DATA_LEN;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use tor_bytes::{Error, Result};
use tor_bytes::{Readable, Reader, Writeable, Writer};

/// Address contained in a ConnectUdp and ConnectedUdp cell which can
/// represent a hostname, IPv4 or IPv6.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Address {
    /// Hostname
    Hostname(Vec<u8>),
    /// IP version 4 address
    Ipv4(Ipv4Addr),
    /// IP version 6 address
    Ipv6(Ipv6Addr),
}

/// Indicates the payload is a hostname.
const T_HOSTNAME: u8 = 0x01;
/// Indicates the payload is an IPv4.
const T_IPV4: u8 = 0x04;
/// Indicates the payload is an IPv6.
const T_IPV6: u8 = 0x06;

/// Maximum length of an Address::Hostname. It must fit in a u8 minus the nul term byte.
const MAX_HOSTNAME_LEN: usize = (u8::MAX - 1) as usize;

impl Address {
    /// Return true iff this is a Hostname.
    pub fn is_hostname(&self) -> bool {
        matches!(self, Address::Hostname(_))
    }

    /// Return the cell wire format address type value.
    fn wire_addr_type(&self) -> u8 {
        match self {
            Address::Hostname(_) => T_HOSTNAME,
            Address::Ipv4(_) => T_IPV4,
            Address::Ipv6(_) => T_IPV6,
        }
    }

    /// Return the cell wire format address length. Note that the Hostname has an extra byte added
    /// to its length due to the nulterm character needed for encoding.
    fn wire_addr_len(&self) -> u8 {
        match self {
            // Add nulterm byte to length. Length can't be above MAX_HOSTNAME_LEN.
            Address::Hostname(h) => (h.len() + 1).try_into().expect("Address hostname too long"),
            Address::Ipv4(_) => 4,
            Address::Ipv6(_) => 16,
        }
    }
}

impl Readable for Address {
    fn take_from(r: &mut Reader<'_>) -> Result<Self> {
        let addr_type = r.take_u8()?;
        let addr_len = r.take_u8()? as usize;

        Ok(match addr_type {
            T_HOSTNAME => {
                let h = r.take_until(0)?;
                if h.len() != (addr_len - 1) {
                    return Err(Error::BadMessage(
                        "Address length doesn't match nulterm hostname",
                    ));
                }
                Self::Hostname(h.into())
            }
            T_IPV4 => Self::Ipv4(r.extract()?),
            T_IPV6 => Self::Ipv6(r.extract()?),
            _ => return Err(Error::BadMessage("Unknown address type")),
        })
    }
}

impl Writeable for Address {
    fn write_onto<B: Writer + ?Sized>(&self, w: &mut B) {
        // Address type.
        w.write_u8(self.wire_addr_type());
        // Address length.
        w.write_u8(self.wire_addr_len());

        match self {
            Address::Hostname(h) => {
                w.write_all(&h[..]);
                w.write_zeros(1); // Nul terminating byte.
            }
            Address::Ipv4(ip) => w.write(ip),
            Address::Ipv6(ip) => w.write(ip),
        }
    }
}

impl FromStr for Address {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(Error::BadMessage("Empty address"));
        }
        if !s.is_ascii() {
            return Err(Error::BadMessage("Non-ascii address"));
        }

        if let Ok(ipv4) = Ipv4Addr::from_str(s) {
            Ok(Self::Ipv4(ipv4))
        } else if let Ok(ipv6) = Ipv6Addr::from_str(s) {
            Ok(Self::Ipv6(ipv6))
        } else {
            if s.len() > MAX_HOSTNAME_LEN {
                return Err(Error::BadMessage("Hostname too long"));
            }
            let mut addr = s.to_string();
            addr.make_ascii_lowercase();
            Ok(Self::Hostname(addr.into_bytes()))
        }
    }
}

impl From<IpAddr> for Address {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(ip) => Address::Ipv4(ip),
            IpAddr::V6(ip) => Address::Ipv6(ip),
        }
    }
}

/// A ConnectUdp message creates a new UDP data stream.
///
/// Upon receiving a ConnectUdp message, a relay tries to connect to the given address with the UDP
/// procotol if the xit policy permits.
///
/// If the exit decides to reject the message, or if the UDP connection fails, the exit should send
/// an End message.
///
/// Clients should reject these messages.
#[derive(Debug, Clone)]
pub struct ConnectUdp {
    /// Same as Begin flags.
    flags: msg::BeginFlags,
    /// Address to connect to. Can be Hostname, IPv4 or IPv6.
    addr: Address,
    /// Target port
    port: u16,
}

impl ConnectUdp {
    /// Construct a new ConnectUdp cell
    pub fn new<F>(addr: &str, port: u16, flags: F) -> crate::Result<Self>
    where
        F: Into<msg::BeginFlags>,
    {
        Ok(Self {
            addr: Address::from_str(addr)?,
            port,
            flags: flags.into(),
        })
    }
}

impl msg::Body for ConnectUdp {
    fn into_message(self) -> msg::RelayMsg {
        msg::RelayMsg::ConnectUdp(self)
    }

    fn decode_from_reader(r: &mut Reader<'_>) -> Result<Self> {
        let flags = r.take_u32()?;
        let addr = r.extract()?;
        let port = r.take_u16()?;

        Ok(Self {
            flags: flags.into(),
            addr,
            port,
        })
    }

    fn encode_onto(self, w: &mut Vec<u8>) {
        w.write_u32(self.flags.bits());
        w.write(&self.addr);
        w.write_u16(self.port);
    }
}

/// A ConnectedUdp cell sent in response to a ConnectUdp.
#[derive(Debug, Clone)]
pub struct ConnectedUdp {
    /// The address that the relay has bound locally of a ConnectUdp. Note
    /// that this might not be the relay address from the descriptor.
    our_address: Address,
    /// The address that the stream is connected to.
    their_address: Address,
}

impl ConnectedUdp {
    /// Construct a new ConnectedUdp cell.
    pub fn new(our: IpAddr, their: IpAddr) -> Result<Self> {
        Ok(Self {
            our_address: our.into(),
            their_address: their.into(),
        })
    }
}

impl msg::Body for ConnectedUdp {
    fn into_message(self) -> msg::RelayMsg {
        msg::RelayMsg::ConnectedUdp(self)
    }

    fn decode_from_reader(r: &mut Reader<'_>) -> Result<Self> {
        let our_address: Address = r.extract()?;
        if our_address.is_hostname() {
            return Err(Error::BadMessage("Our address is a Hostname"));
        }
        let their_address: Address = r.extract()?;
        if their_address.is_hostname() {
            return Err(Error::BadMessage("Their address is a Hostname"));
        }

        Ok(Self {
            our_address,
            their_address,
        })
    }

    fn encode_onto(self, w: &mut Vec<u8>) {
        w.write(&self.our_address);
        w.write(&self.their_address);
    }
}

/// A Datagram message represents data sent along a UDP stream.
///
/// Upon receiving a Datagram message for a live stream, the client or
/// exit sends that data onto the associated UDP connection.
///
/// These messages hold between 1 and [Datagram::MAXLEN] bytes of data each.
#[derive(Debug, Clone)]
pub struct Datagram {
    /// Contents of the cell, to be sent on a specific stream
    body: Vec<u8>,
}

impl Datagram {
    /// NOTE: Proposal 340, fragmented relay message, might change this value reality.
    /// The longest allowable body length for a single data cell.
    pub const MAXLEN: usize = CELL_DATA_LEN - 11;

    /// Construct a new data cell.
    ///
    /// Returns an error if `inp` is longer than [`Data::MAXLEN`] bytes.
    pub fn new(inp: &[u8]) -> crate::Result<Self> {
        if inp.len() > msg::Data::MAXLEN {
            return Err(crate::Error::CantEncode);
        }
        Ok(Self::new_unchecked(inp.into()))
    }

    /// Construct a new cell from a provided vector of bytes.
    ///
    /// The vector _must_ have fewer than [`Data::MAXLEN`] bytes.
    fn new_unchecked(body: Vec<u8>) -> Self {
        Self { body }
    }
}

impl From<Datagram> for Vec<u8> {
    fn from(data: Datagram) -> Vec<u8> {
        data.body
    }
}

impl AsRef<[u8]> for Datagram {
    fn as_ref(&self) -> &[u8] {
        &self.body[..]
    }
}

impl msg::Body for Datagram {
    fn into_message(self) -> msg::RelayMsg {
        msg::RelayMsg::Datagram(self)
    }

    fn decode_from_reader(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Datagram {
            body: r.take(r.remaining())?.into(),
        })
    }

    fn encode_onto(mut self, w: &mut Vec<u8>) {
        w.append(&mut self.body);
    }
}

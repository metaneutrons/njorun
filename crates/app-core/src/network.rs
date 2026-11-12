//! Network settings per interface (plan 006, decisions N3 and N4).
//!
//! The same shape is set on the tablet during setup and sent by Home Assistant. Validation is
//! identical on both sides and pinned by `protocol/fixtures/network-cases.json`.

use std::fmt;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

/// The interfaces a board can have. The tablet reports which ones it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interface {
    Wifi,
    Ethernet,
}

/// How an interface gets its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Dhcp,
    Static,
}

/// The IP side of an interface. DHCP carries nothing else; a static setup needs an address, a
/// prefix length and a router, and may have up to two DNS servers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpConfig {
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<Ipv4Addr>,
}

impl IpConfig {
    /// The default: the address comes from the router.
    #[must_use]
    pub const fn dhcp() -> Self {
        Self {
            mode: Mode::Dhcp,
            address: None,
            prefix: None,
            gateway: None,
            dns: Vec::new(),
        }
    }
}

impl IpConfig {
    /// Checks the values of this setup, on their own.
    ///
    /// # Errors
    /// The first [`Problem`] found.
    pub fn validate(&self) -> Result<(), Problem> {
        validate_ip(self)
    }

    /// Builds a static setup from what a person typed, one text per field.
    ///
    /// `dns` holds none, one or two addresses separated by spaces or commas. The problem names
    /// the field to correct: [`Problem::Address`], [`Problem::Prefix`], [`Problem::Gateway`] or
    /// [`Problem::Dns`].
    ///
    /// # Errors
    /// The first [`Problem`] found, in the order the fields are asked.
    pub fn from_text(
        address: &str,
        prefix: &str,
        gateway: &str,
        dns: &str,
    ) -> Result<Self, Problem> {
        let address = address.trim().parse().map_err(|_| Problem::Address)?;
        let prefix = prefix.trim().parse::<u8>().map_err(|_| Problem::Prefix)?;
        let gateway = gateway.trim().parse().map_err(|_| Problem::Gateway)?;
        let dns = dns
            .split([' ', ','])
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().map_err(|_| Problem::Dns))
            .collect::<Result<Vec<Ipv4Addr>, _>>()?;
        let config = Self {
            mode: Mode::Static,
            address: Some(address),
            prefix: Some(prefix),
            gateway: Some(gateway),
            dns,
        };
        config.validate()?;
        Ok(config)
    }
}

impl Default for IpConfig {
    fn default() -> Self {
        Self::dhcp()
    }
}

/// A Wi-Fi network and how to address the tablet on it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiConfig {
    pub ssid: String,
    /// Empty or absent for an open network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub ip: IpConfig,
}

impl fmt::Debug for WifiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WifiConfig")
            .field("ssid", &self.ssid)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("ip", &self.ip)
            .finish()
    }
}

/// A wired interface has no credentials.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EthernetConfig {
    #[serde(default)]
    pub ip: IpConfig,
}

/// What Home Assistant (or the setup) wants for the network. An interface that is absent is left
/// as it is on the tablet (N4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi: Option<WifiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ethernet: Option<EthernetConfig>,
}

/// Why a configuration is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Nothing in it.
    Empty,
    /// The board has no such interface.
    NoSuchInterface(Interface),
    /// The network name is empty, longer than 32 bytes or has a control character.
    Ssid,
    /// A password that is neither 8 to 63 characters nor 64 hexadecimal digits.
    Password,
    /// DHCP with static values, or a static setup that lacks its address, prefix or router.
    Shape,
    /// The address is not a usable host address.
    Address,
    /// The prefix length is not 1 to 30.
    Prefix,
    /// The router is not a host address inside the subnet of the address.
    Gateway,
    /// More than two DNS servers, or one that is no unicast address.
    Dns,
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "the network settings are empty",
            Self::NoSuchInterface(Interface::Wifi) => "this board has no Wi-Fi",
            Self::NoSuchInterface(Interface::Ethernet) => "this board has no Ethernet",
            Self::Ssid => "the Wi-Fi name must be 1 to 32 bytes without control characters",
            Self::Password => "the Wi-Fi password must be 8 to 63 characters or 64 hex digits",
            Self::Shape => "DHCP takes no address; a static setup needs address, prefix and router",
            Self::Address => "the address is not a usable host address",
            Self::Prefix => "the prefix length must be 1 to 30",
            Self::Gateway => "the router must be another host address in the same subnet",
            Self::Dns => "at most two DNS servers, each a unicast address",
        })
    }
}

impl std::error::Error for Problem {}

impl NetworkConfig {
    /// Checks the configuration against the interfaces the board has.
    ///
    /// # Errors
    /// The first [`Problem`] found.
    pub fn validate(&self, interfaces: &[Interface]) -> Result<(), Problem> {
        if self.wifi.is_none() && self.ethernet.is_none() {
            return Err(Problem::Empty);
        }
        if let Some(wifi) = &self.wifi {
            if !interfaces.contains(&Interface::Wifi) {
                return Err(Problem::NoSuchInterface(Interface::Wifi));
            }
            validate_ssid(&wifi.ssid)?;
            if let Some(password) = wifi.password.as_deref() {
                validate_password(password)?;
            }
            validate_ip(&wifi.ip)?;
        }
        if let Some(ethernet) = &self.ethernet {
            if !interfaces.contains(&Interface::Ethernet) {
                return Err(Problem::NoSuchInterface(Interface::Ethernet));
            }
            validate_ip(&ethernet.ip)?;
        }
        Ok(())
    }
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn validate_ssid(ssid: &str) -> Result<(), Problem> {
    if ssid.is_empty() || ssid.len() > 32 || has_control(ssid) {
        return Err(Problem::Ssid);
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), Problem> {
    // An empty password is an open network. A passphrase is 8 to 63 printable characters, a
    // pre-shared key is exactly 64 hexadecimal digits.
    if password.is_empty() {
        return Ok(());
    }
    let passphrase = (8..=63).contains(&password.len()) && !has_control(password);
    let key = password.len() == 64 && password.bytes().all(|byte| byte.is_ascii_hexdigit());
    if passphrase || key {
        Ok(())
    } else {
        Err(Problem::Password)
    }
}

const fn unicast_host(address: Ipv4Addr) -> bool {
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_link_local()
        || address.octets()[0] >= 240)
}

fn mask(prefix: u8) -> u32 {
    u32::MAX << (32 - u32::from(prefix))
}

fn validate_ip(ip: &IpConfig) -> Result<(), Problem> {
    if ip.mode == Mode::Dhcp {
        let carries_values = ip.address.is_some()
            || ip.prefix.is_some()
            || ip.gateway.is_some()
            || !ip.dns.is_empty();
        return if carries_values {
            Err(Problem::Shape)
        } else {
            Ok(())
        };
    }
    let (Some(address), Some(prefix), Some(gateway)) = (ip.address, ip.prefix, ip.gateway) else {
        return Err(Problem::Shape);
    };
    if !(1..=30).contains(&prefix) {
        return Err(Problem::Prefix);
    }
    let mask = mask(prefix);
    let network = u32::from(address) & mask;
    let broadcast = network | !mask;
    if !unicast_host(address) || u32::from(address) == network || u32::from(address) == broadcast {
        return Err(Problem::Address);
    }
    if !unicast_host(gateway)
        || gateway == address
        || u32::from(gateway) & mask != network
        || u32::from(gateway) == network
        || u32::from(gateway) == broadcast
    {
        return Err(Problem::Gateway);
    }
    if ip.dns.len() > 2 || !ip.dns.iter().all(|server| unicast_host(*server)) {
        return Err(Problem::Dns);
    }
    Ok(())
}

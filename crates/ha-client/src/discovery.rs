//! Bounded host-side discovery of Home Assistant's documented DNS-SD service.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};

use crate::{ClientError, websocket_url};

const SERVICE_TYPE: &str = "_home-assistant._tcp.local.";
const MAX_INSTANCES: usize = 16;

/// One Home Assistant installation found on the local network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredHomeAssistant {
    pub name: String,
    pub endpoint: String,
}

/// Browse Home Assistant's documented service type for a bounded duration.
///
/// # Errors
/// Returns `Connection` when the host mDNS adapter cannot start or browse.
pub fn discover(timeout: Duration) -> Result<Vec<DiscoveredHomeAssistant>, ClientError> {
    let daemon = ServiceDaemon::new().map_err(|_| ClientError::Connection)?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .map_err(|_| ClientError::Connection)?;
    let deadline = Instant::now() + timeout;
    let mut found = BTreeMap::new();
    while found.len() < MAX_INSTANCES {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let Ok(event) = receiver.recv_timeout(remaining) else {
            break;
        };
        if let ServiceEvent::ServiceResolved(service) = event
            && let Some(instance) = resolved_instance(&service)
        {
            found.insert(instance.endpoint.clone(), instance);
        }
    }
    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}

fn resolved_instance(service: &ResolvedService) -> Option<DiscoveredHomeAssistant> {
    let endpoint = ["internal_url", "base_url", "external_url"]
        .into_iter()
        .filter_map(|key| service.get_property_val_str(key))
        .find(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| fallback_endpoint(service))?;
    websocket_url(&endpoint, true).ok()?;
    let name = service
        .get_property_val_str("location_name")
        .filter(|value| !value.is_empty() && value.chars().count() <= 128)
        .map_or_else(
            || {
                service
                    .get_fullname()
                    .strip_suffix(SERVICE_TYPE)
                    .unwrap_or_else(|| service.get_fullname())
                    .trim_end_matches('.')
                    .to_owned()
            },
            str::to_owned,
        );
    if name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    Some(DiscoveredHomeAssistant { name, endpoint })
}

fn fallback_endpoint(service: &ResolvedService) -> Option<String> {
    let address = service
        .get_addresses()
        .iter()
        .map(mdns_sd::ScopedIp::to_ip_addr)
        .find(|address| !address.is_unspecified() && !address.is_multicast())?;
    Some(match address {
        IpAddr::V4(address) => format!("http://{address}:{}", service.get_port()),
        IpAddr::V6(address) => format!("http://[{address}]:{}", service.get_port()),
    })
}

#[cfg(test)]
mod tests {
    use super::{MAX_INSTANCES, discover, resolved_instance};
    use mdns_sd::ServiceInfo;
    use std::time::Duration;

    fn service<Ip>(name: &str, addresses: Ip, properties: &[(&str, &str)]) -> ResolvedService
    where
        Ip: mdns_sd::AsIpAddrs,
    {
        ServiceInfo::new(
            "_home-assistant._tcp.local.",
            name,
            "homeassistant.local.",
            addresses,
            8123,
            properties,
        )
        .expect("valid test service")
        .as_resolved_service()
    }

    use mdns_sd::ResolvedService;

    #[test]
    fn txt_endpoint_and_location_name_take_precedence() {
        let resolved = service(
            "Fallback name",
            "192.0.2.10",
            &[
                ("internal_url", "https://ha.example.test:8443"),
                ("base_url", "http://192.0.2.10:8123"),
                ("location_name", "Home"),
            ],
        );

        assert_eq!(
            resolved_instance(&resolved),
            Some(super::DiscoveredHomeAssistant {
                name: "Home".into(),
                endpoint: "https://ha.example.test:8443".into(),
            })
        );
    }

    #[test]
    fn address_fallback_supports_ipv4_and_ipv6() {
        let ipv4 = service("IPv4 Home", "192.0.2.11", &[] as &[(&str, &str)]);
        let ipv6 = service("IPv6 Home", "2001:db8::11", &[] as &[(&str, &str)]);

        assert_eq!(
            resolved_instance(&ipv4).expect("IPv4 instance").endpoint,
            "http://192.0.2.11:8123"
        );
        assert_eq!(
            resolved_instance(&ipv6).expect("IPv6 instance").endpoint,
            "http://[2001:db8::11]:8123"
        );
    }

    #[test]
    fn invalid_metadata_is_rejected_or_falls_back_safely() {
        let invalid_url = service(
            "Invalid URL",
            "0.0.0.0",
            &[("internal_url", "file:///not-home-assistant")],
        );
        assert_eq!(resolved_instance(&invalid_url), None);

        let control_name = service("bad\nname", "192.0.2.12", &[] as &[(&str, &str)]);
        assert_eq!(resolved_instance(&control_name), None);

        let long_location = "x".repeat(129);
        let fallback = service(
            "Safe fallback",
            "192.0.2.13",
            &[("location_name", long_location.as_str())],
        );
        assert_eq!(
            resolved_instance(&fallback)
                .expect("fallback instance")
                .name,
            "Safe fallback"
        );
    }

    #[test]
    fn zero_duration_browse_is_bounded() {
        let found = discover(Duration::ZERO).expect("mDNS adapter starts");
        assert!(found.len() <= MAX_INSTANCES);
    }
}

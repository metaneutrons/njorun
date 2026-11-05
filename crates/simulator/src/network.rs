//! Desktop status of the default network interface. Unknown transports stay hidden.
use std::process::Command;

fn output(program: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(program).args(args).output().ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).into_owned())
}

const fn wifi(dbm: Option<i32>) -> &'static str {
    match dbm {
        Some(-54..=-1) => "wifi4",
        Some(-66..=-55) => "wifi3",
        Some(-76..=-67) => "wifi2",
        Some(-120..=-77) => "wifi1",
        _ => "wifi",
    }
}

#[cfg(target_os = "macos")]
pub fn sample() -> Option<&'static str> {
    let route = output("/sbin/route", &["-n", "get", "default"])
        .or_else(|| output("/sbin/route", &["-n", "get", "-inet6", "default"]))?;
    let iface = route
        .lines()
        .find_map(|line| line.trim().strip_prefix("interface: "))?;
    let ports = output("/usr/sbin/networksetup", &["-listallhardwareports"])?;
    let port = ports.split("\n\n").find(|port| {
        port.lines()
            .any(|line| line.strip_prefix("Device: ") == Some(iface))
    })?;
    let kind = port
        .lines()
        .find_map(|line| line.strip_prefix("Hardware Port: "))?;
    if kind == "Wi-Fi" {
        let rssi = output("/usr/bin/osascript", &["-l", "JavaScript", "-e",
            "ObjC.import('CoreWLAN'); const i = $.CWWiFiClient.sharedWiFiClient.interfaceWithName($.NSProcessInfo.processInfo.arguments.lastObject); i.rssiValue", iface])
            .and_then(|value| value.trim().parse().ok());
        return Some(wifi(rssi));
    }
    kind.to_ascii_lowercase()
        .contains("ethernet")
        .then_some("ethernet")
}

#[cfg(target_os = "linux")]
pub fn sample() -> Option<&'static str> {
    use std::{fs, path::Path};
    let routes = output("ip", &["-o", "route", "show", "default"])
        .filter(|table| !table.trim().is_empty())
        .or_else(|| output("ip", &["-o", "-6", "route", "show", "default"]))?;
    let iface = routes.lines().find_map(|route| {
        let mut words = route.split_whitespace();
        words.find(|word| *word == "dev")?;
        words.next()
    })?;
    let device = Path::new("/sys/class/net").join(iface);
    if device.join("wireless").is_dir() || device.join("phy80211").exists() {
        let rssi = fs::read_to_string("/proc/net/wireless")
            .ok()
            .and_then(|table| {
                table.lines().find_map(|line| {
                    let (name, values) = line.split_once(':')?;
                    if name.trim() != iface {
                        return None;
                    }
                    values
                        .split_whitespace()
                        .nth(2)?
                        .trim_end_matches('.')
                        .parse()
                        .ok()
                })
            });
        return Some(wifi(rssi));
    }
    (device.join("device").exists() && fs::read_to_string(device.join("type")).ok()?.trim() == "1")
        .then_some("ethernet")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const fn sample() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::{output, wifi};

    #[test]
    fn command_output_and_wifi_buckets_are_bounded() {
        assert_eq!(
            output("/usr/bin/printf", &["connected"]),
            Some("connected".into())
        );
        assert_eq!(output("/njorun/missing-command", &[]), None);
        assert_eq!(wifi(Some(-54)), "wifi4");
        assert_eq!(wifi(Some(-55)), "wifi3");
        assert_eq!(wifi(Some(-67)), "wifi2");
        assert_eq!(wifi(Some(-77)), "wifi1");
        assert_eq!(wifi(Some(-121)), "wifi");
        assert_eq!(wifi(None), "wifi");
    }
}

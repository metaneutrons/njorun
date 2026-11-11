//! The Wi-Fi list of the commissioning screen, kept steady across repeated scans.
//!
//! A single scan is a noisy snapshot: access points are missed now and then, a mesh shows the
//! same name several times and every reading of the signal jumps by a few dB. Shown as it is,
//! the list would flicker and reorder itself on every scan. This module merges the scans into one
//! model and hands out a list that only changes when something real changed:
//!
//! - Entries with the same name are one network, the strongest access point counts.
//! - The signal is smoothed over the last scans, and the bar count changes only after the signal
//!   has moved a little past the boundary (hysteresis).
//! - A network disappears after it was missing from [`MISSES_BEFORE_REMOVAL`] scans in a row, a new
//!   one shows up at once.
//! - The order is by bars, then by name, so a network never moves because of a dB of noise.
//!
//! The frontend decides when the shown list may change, for example not while a finger is on the
//! display. It calls [`WifiList::merge`] for every scan and [`WifiList::visible`] when it may
//! redraw.

/// Scans a network may be missing from in a row before it leaves the list.
pub const MISSES_BEFORE_REMOVAL: u8 = 2;

/// Signal margin in dB that a network has to pass before its bar count changes.
const BAR_HYSTERESIS_DB: i16 = 2;

/// One network as a scan reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub rssi: i8,
    pub password_required: bool,
}

/// One row of the list as the screen shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub ssid: String,
    /// Smoothed signal in dBm.
    pub rssi: i8,
    /// Bar count from 1 to 5. It follows the signal with hysteresis, so use this value and do
    /// not derive the bars from `rssi` again.
    pub bars: i32,
    pub password_required: bool,
}

const fn bars(rssi: i16) -> i32 {
    match rssi {
        -50..=i16::MAX => 5,
        -60..=-51 => 4,
        -70..=-61 => 3,
        -80..=-71 => 2,
        _ => 1,
    }
}

#[derive(Clone, Debug)]
struct Entry {
    ssid: String,
    /// Smoothed signal in tenths of a dB, so that the average does not round away.
    rssi_tenths: i32,
    password_required: bool,
    bars: i32,
    missed: u8,
}

impl Entry {
    fn rssi(&self) -> i16 {
        i16::try_from((self.rssi_tenths + 5).div_euclid(10)).unwrap_or(i16::MIN)
    }

    fn settle_bars(&mut self) {
        let rssi = self.rssi();
        let up = bars(rssi - BAR_HYSTERESIS_DB);
        let down = bars(rssi + BAR_HYSTERESIS_DB);
        if up > self.bars {
            self.bars = up;
        } else if down < self.bars {
            self.bars = down;
        }
    }
}

/// The merged scans. Not thread-safe by itself, the frontend owns it.
#[derive(Clone, Debug, Default)]
pub struct WifiList {
    entries: Vec<Entry>,
}

impl WifiList {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A list that starts from one scan, for example the first one of the commissioning screen.
    #[must_use]
    pub fn from_scan(scan: &[Network]) -> Self {
        let mut list = Self::new();
        list.merge(scan);
        list
    }

    /// Merges one complete scan into the list.
    pub fn merge(&mut self, scan: &[Network]) {
        let mut seen: Vec<Network> = Vec::new();
        for network in scan {
            if network.ssid.is_empty() {
                continue;
            }
            match seen.iter_mut().find(|known| known.ssid == network.ssid) {
                Some(known) if network.rssi > known.rssi => *known = network.clone(),
                Some(_) => {}
                None => seen.push(network.clone()),
            }
        }

        for entry in &mut self.entries {
            match seen.iter().find(|network| network.ssid == entry.ssid) {
                Some(network) => {
                    // Two thirds of the old value and one third of the new one.
                    entry.rssi_tenths = (entry.rssi_tenths * 2 + i32::from(network.rssi) * 10) / 3;
                    entry.password_required = network.password_required;
                    entry.missed = 0;
                    entry.settle_bars();
                }
                None => entry.missed = entry.missed.saturating_add(1),
            }
        }
        self.entries
            .retain(|entry| entry.missed < MISSES_BEFORE_REMOVAL);

        for network in seen {
            if !self.entries.iter().any(|entry| entry.ssid == network.ssid) {
                self.entries.push(Entry {
                    bars: bars(i16::from(network.rssi)),
                    rssi_tenths: i32::from(network.rssi) * 10,
                    ssid: network.ssid,
                    password_required: network.password_required,
                    missed: 0,
                });
            }
        }
    }

    /// The list as the screen shows it: most bars first, then by name.
    #[must_use]
    pub fn visible(&self) -> Vec<Listed> {
        let mut entries: Vec<&Entry> = self.entries.iter().collect();
        entries.sort_by(|left, right| {
            right
                .bars
                .cmp(&left.bars)
                .then_with(|| left.ssid.to_lowercase().cmp(&right.ssid.to_lowercase()))
                .then_with(|| left.ssid.cmp(&right.ssid))
        });
        entries
            .into_iter()
            .map(|entry| Listed {
                ssid: entry.ssid.clone(),
                rssi: i8::try_from(entry.rssi()).unwrap_or(i8::MIN),
                bars: entry.bars,
                password_required: entry.password_required,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(ssid: &str, rssi: i8) -> Network {
        Network {
            ssid: ssid.to_owned(),
            rssi,
            password_required: true,
        }
    }

    fn names(list: &WifiList) -> Vec<String> {
        list.visible()
            .into_iter()
            .map(|network| network.ssid)
            .collect()
    }

    #[test]
    fn same_name_is_one_network_with_the_strongest_signal() {
        let list = WifiList::from_scan(&[
            network("Home", -70),
            network("Home", -48),
            network("Home", -80),
        ]);
        let shown = list.visible();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].rssi, -48);
        assert_eq!(shown[0].bars, 5);
    }

    #[test]
    fn hidden_networks_are_left_out() {
        let list = WifiList::from_scan(&[network("", -40), network("Home", -60)]);
        assert_eq!(names(&list), ["Home"]);
    }

    #[test]
    fn order_is_bars_then_name_not_the_raw_signal() {
        // Same bar count: the name decides, whatever the dB values are.
        let list = WifiList::from_scan(&[
            network("beta", -52),
            network("Alpha", -59),
            network("gamma", -45),
        ]);
        assert_eq!(names(&list), ["gamma", "Alpha", "beta"]);
        let louder = WifiList::from_scan(&[network("zeta", -40), network("alpha", -75)]);
        assert_eq!(names(&louder), ["zeta", "alpha"]);
    }

    #[test]
    fn a_new_network_shows_up_at_once() {
        let mut list = WifiList::from_scan(&[network("Home", -60)]);
        list.merge(&[network("Home", -60), network("Guest", -55)]);
        assert_eq!(names(&list), ["Guest", "Home"]);
    }

    #[test]
    fn a_network_leaves_only_after_repeated_misses() {
        let mut list = WifiList::from_scan(&[network("Home", -60), network("Cafe", -62)]);
        list.merge(&[network("Home", -60)]);
        assert_eq!(names(&list), ["Home", "Cafe"], "one missed scan keeps it");
        list.merge(&[network("Home", -60)]);
        assert_eq!(names(&list), ["Home"], "the second one removes it");
    }

    #[test]
    fn a_sighting_in_between_resets_the_miss_count() {
        let mut list = WifiList::from_scan(&[network("Home", -60), network("Cafe", -62)]);
        list.merge(&[network("Home", -60)]);
        list.merge(&[network("Home", -60), network("Cafe", -62)]);
        list.merge(&[network("Home", -60)]);
        assert_eq!(names(&list), ["Home", "Cafe"]);
    }

    #[test]
    fn signal_noise_does_not_reorder_neighbours() {
        let mut list = WifiList::from_scan(&[network("aaa", -61), network("bbb", -60)]);
        let before = names(&list);
        for step in 0..20 {
            let wobble = if step % 2 == 0 { 2 } else { -2 };
            list.merge(&[network("aaa", -61 + wobble), network("bbb", -60 - wobble)]);
            assert_eq!(names(&list), before, "scan {step}");
        }
    }

    #[test]
    fn bars_change_only_after_a_real_move_past_the_boundary() {
        // -60 is the top of four bars, -61 the bottom of three. A reading just across the
        // boundary does not flip the bars, a clear move does.
        let mut list = WifiList::from_scan(&[network("Home", -58)]);
        assert_eq!(list.visible()[0].bars, 4);
        for _ in 0..6 {
            list.merge(&[network("Home", -62)]);
        }
        assert_eq!(
            list.visible()[0].bars,
            4,
            "-62 is only just across the boundary"
        );
        for _ in 0..8 {
            list.merge(&[network("Home", -70)]);
        }
        assert_eq!(list.visible()[0].bars, 3);
    }

    #[test]
    fn a_sudden_single_reading_is_smoothed() {
        let mut list = WifiList::from_scan(&[network("Home", -50)]);
        list.merge(&[network("Home", -90)]);
        assert_eq!(
            list.visible()[0].bars,
            3,
            "one bad scan does not drop it to one bar"
        );
    }

    #[test]
    fn the_password_flag_follows_the_latest_scan() {
        let mut list = WifiList::from_scan(&[Network {
            ssid: "Home".into(),
            rssi: -60,
            password_required: false,
        }]);
        list.merge(&[network("Home", -60)]);
        assert!(list.visible()[0].password_required);
    }

    #[test]
    fn an_empty_scan_empties_the_list_only_after_the_grace_scans() {
        let mut list = WifiList::from_scan(&[network("Home", -60)]);
        list.merge(&[]);
        assert_eq!(names(&list), ["Home"]);
        list.merge(&[]);
        assert_eq!(names(&list), Vec::<String>::new());
    }
}

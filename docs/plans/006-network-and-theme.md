# Network settings from the tablet and from Home Assistant, and the first theme choice

Two things move into the setup and into Home Assistant: the theme is chosen once on
the first start, and the network (Wi-Fi credentials, static addresses, wired
networking) can be set on the tablet and later changed from Home Assistant without
losing the tablet when a value is wrong. This builds on the management link
([003](003-d1001-ha-management.md)), the pairing
([004](004-tablet-first-pairing.md)) and the first setup over USB
([005](005-usb-provisioning.md)).

## Baseline (2026-11-12)

Read in the repository: the theme default is `dark` on both sides, the theme can be
changed on the tablet only in the dashboard settings and only until the tablet is
managed, and the first setup has no theme page. The tablet joins Wi-Fi with an SSID
and a password kept in plain NVS, always with DHCP. There is no Ethernet code, no
static address, no DNS setting and no interface priority. Home Assistant pushes
settings in a `configuration` message that the tablet acknowledges with `applied`
after it has shown them. The tablet has no way to report that it refused or could not
apply a configuration; a refusal is only logged and Home Assistant sees a missing
`applied`. The D1001 has Wi-Fi only (plan 005), so wired networking cannot be tried
on the hardware we have.

## Outcome

- On the first start the user picks light, dark or automatic, with dark selected. After
  the tablet is connected to Home Assistant the theme comes from Home Assistant, as
  today.
- A board with Ethernet uses the cable as soon as it works and keeps Wi-Fi, when it is
  set up, as the fallback.
- Wi-Fi credentials and, for each interface, the IP mode can be set on the tablet
  during setup and from Home Assistant afterwards. DHCP is the default. A static setup
  has an address, a prefix length, a router and DNS servers.
- A change from Home Assistant is tried by the tablet first. If the tablet cannot
  reach Home Assistant with the new values, it returns to the previous values and tells
  Home Assistant what failed.

## Decisions for review

- **N1 The first theme choice is a setup page.** A page between the language and the
  Wi-Fi page with three choices, dark selected, applied at once so the user sees the
  result. The choice is stored in the existing `theme` key. When Home Assistant sends
  the first configuration, its value wins, because a new tablet inherits its profile
  (plan 003 D2). So that the user's choice is not thrown away, the announcement
  carries it and Home Assistant offers it as the starting value of a new tablet's
  theme at approval, where the administrator can change it (see open point O1).
- **N2 Network settings are a message of their own.** They do not go into `settings`.
  They contain secrets, they must not appear in entities, diagnostics or logs, and
  they follow a different rule: they are acknowledged only after the tablet has shown
  that it works with them. The message is `network` with its own `operation_id` and
  revision, in the same encrypted session. `settings` and its 3072-byte limit stay as
  they are.
- **N3 One shape per interface.** The tablet reports which interfaces it has
  (`wifi`, `ethernet`) in its capabilities, as plan 005 already foresaw, and Home
  Assistant only offers what the tablet has. Each interface has an `ip` object with `mode`
  (`dhcp` or `static`) and, for `static`, `address`, `prefix`, `gateway` and up to two
  `dns` servers (none is allowed). DHCP carries none of them. Wi-Fi also has `ssid` and
  `password` next to `ip`; an empty or absent password is an open network. IPv4 only in this plan (see
  open point O3). Validation is the same on the tablet, in the integration and in the
  setup page, and a shared fixture pins it, as for the settings: the address is a
  unicast host address, the prefix is 1 to 30, the gateway lies in the subnet and is
  not the address itself, and at most two DNS servers.
- **N4 Unset means leave as it is.** What Home Assistant has not configured stays as
  set on the tablet. Setting a value in Home Assistant replaces the tablet's local
  value. Removing it in Home Assistant does not clear the tablet.
- **N5 Try, prove, commit.** The tablet keeps the previous values until the new ones
  are proven. It applies the candidate, and the candidate is proven when the tablet has
  an address and has opened a management session with Home Assistant over it, not
  merely joined the network: a wrong router or DNS server still joins. The proof has
  a timeout (45 seconds as a starting value). On success the tablet writes the
  candidate as the current values, drops the previous ones and sends `network_result`
  with `applied`. On failure, a timeout or a power loss before the commit, the tablet
  returns to the previous values, which remain the stored values until the commit, so
  a reset during the trial also ends on the previous values. It then reconnects and
  sends `network_result` with `reverted` and a reason (`no_association`, `no_address`,
  `no_home_assistant`, `rejected_values`). The password is never in a result.
- **N6 Home Assistant does not retry a reverted revision.** A `reverted` result marks
  that revision as failed. The integration shows the reason in a sensor of the tablet
  and in a repair issue, and sends nothing again until the administrator changes the
  values. Without this the reconnect logic of plan 003 would push the same bad values
  after every reconnect.
- **N7 Only the interface in use can prove itself fully.** A candidate for the
  interface that currently carries the session is proven as in N5. A candidate for the
  other interface (a new Wi-Fi password while a cable is the primary link) is proven
  as far as that interface joins, gets an address and opens a TCP connection to Home
  Assistant through it. That is a weaker proof, and the result says so
  (`proof: "link"` or `proof: "session"`) so that Home Assistant can show it.
- **N8 The wired link has priority.** Ethernet is preferred as soon as it has link and
  an address; Wi-Fi stays joined as the fallback when it is set up. If the wired link
  loses its address, traffic uses Wi-Fi. A wired link that is up but without a path to
  Home Assistant (no session for 60 seconds while Wi-Fi is joined) also hands over to
  Wi-Fi, and the tablet tries the wired link again every few minutes. Which interface
  carries the session is reported to Home Assistant (`network-link`, which the UI
  already shows).
- **N9 Where Home Assistant keeps it.** Wi-Fi credentials are set in a profile or per
  tablet. A static address is set per tablet only, because a profile would give the
  same address to every tablet in it, and the integration refuses it there. The
  password is stored in the integration's storage like other integrations store
  credentials, is shown only as "set" in forms, is excluded from diagnostics and from
  entity attributes, and travels only inside the Noise session.
- **N10 Setup on the tablet.** After the Wi-Fi password the setup offers
  "Advanced" with DHCP or static values and the same validation. The default path stays
  DHCP and needs no extra tap. A board with Ethernet shows the wired
  page first when a cable is present. The Improv extension for the USB tool (a command
  of our own, as plan 005 foresaw) follows in a later slice.

## Messages

`network` (Home Assistant to tablet): `type`, `operation_id`, `revision`,
`expires_at` and per interface the object of N3. The tablet checks it as it checks a
configuration (type, expiry, revision not older, strict parse, validation) and answers
only through `network_result`.

`network_result` (tablet to Home Assistant): `operation_id`, `revision`, `result`
(`applied` or `reverted`), `reason` when reverted, `proof` (`session` or `link`) when
applied, and the non-secret state that is now in force: per interface the mode, the
address, the SSID and whether it carries the session. Unlike a configuration, a
refused `network` message (malformed, expired, stale) is answered with `reverted` and
`rejected_values`, so that Home Assistant is never left guessing. The same reply
channel is the first step towards the same for `configuration` (see O4).

## Slices

1. **Theme page.** The setup page, the simulator twin, the announcement field and the
   approval default. Small, verifiable on the tablet.
2. **Network model.** `NetworkConfig` with validation and fixtures in `app-core`, the
   Python twin in the integration, the interface selection rule of N8 as a pure state
   machine with tests, and the trial and revert logic of N5 as a state machine with
   tests (no hardware needed).
3. **Static address on Wi-Fi.** The C glue stops DHCP and sets address, router and DNS
   on the station interface. In the setup a switch under the network list ("Fixed IP
   address") makes the tablet ask, after the network and its password, for the address,
   the prefix length, the router and the DNS servers, one page each, and check them as
   in N3. The setup is stored with the network (`wifi_ip`) and used when the saved
   network is restored. The previous values and the revert belong to slice 4, which
   brings the messages that need them.
4. **From Home Assistant.** In two parts.
   - **4a, the messages and the trial.** The `network` and `network_result` messages
     ([protocol](../protocols/002-ha-management.md#network-settings)), the gate on the
     tablet, the trial with its revert, the stored result that the next session reports,
     the storage in Home Assistant, the failed-revision rule and the repair issue. The
     runtime has the calls that the forms will use. Nothing in Home Assistant's user
     interface sets a value yet.
   - **4b, the forms and the sensor.** Two entries in the options flow: the Wi-Fi a
     profile gives its tablets, and the Wi-Fi override and the IP setup of one tablet
     (DHCP or static, per interface the tablet reports). A password is never shown,
     only whether one is set; an empty field keeps it while the name stays the same.
     An interface whose DHCP was never set stays unset (N4). A diagnostic sensor,
     `Network settings`, shows not set, waiting, being tried, applied or reverted, with
     the reason or proof and the state the tablet reported. The form code itself has no
     test, because it needs Home Assistant; its helpers and the runtime calls do.
5. **Ethernet.** The driver, the second interface and the priority of N8 on a board
   that has a wired port. It cannot be run on the D1001, so it is written against the
   interface abstraction and the state machines of slice 2, and stays unqualified
   until a board runs it. Built as the Kconfig option `NJORUN_ETHERNET` (off by default).
6. **Refusal answer for `configuration`.** The same reply channel for the settings
   message (O4), with a sensor or repair entry that shows the reason.

## Decided while building slice 4a

- **N11 A trial's result is reported by the next session.** A Wi-Fi trial breaks the
  session, so the tablet never sends the result at the moment it decides it. It writes
  the result to storage and the next session that opens reports it. The record is written
  as `reverted` before the trial starts, so a reset or a power loss ends with a report.
  Home Assistant therefore matches a result by tablet and revision, not by session or
  operation.
- **N12 A repeated message is harmless.** Values that equal the ones in use are answered
  `applied` at once without a trial. The same revision with other values is refused. The
  tablet gate lets the same revision pass for this reason (a message with an older one is
  stale).
- **N13 The revision follows the values.** Home Assistant raises a tablet's revision
  whenever the values it would send differ from the ones it saw last, found when it is
  about to send. Profile edits, reassignment and a new capability count as well, without
  a hook at each place.
- **N14 One message at a time.** Home Assistant sends the next revision only after the
  answer to the last one, or after 180 seconds. The tablet refuses a second message
  during a trial, and without this rule that refusal would mark a good revision as failed.
- **N15 The capability is `network`.** A list of the interfaces the tablet has, negotiated
  in the subscribe result like `ota`. An integration older than this change refuses a
  capability it does not know, so the integration has to be updated before a tablet with
  this firmware connects.

## Decided while building slice 5

- **N16 The wired port is a build option.** `CONFIG_NJORUN_ETHERNET` in the component's
  Kconfig (default off, PHY address, reset, MDC and MDIO pins; the RMII pins and the
  external 50 MHz clock are the ESP32-P4 defaults of ESP-IDF, generic IEEE 802.3 PHY
  driver). Without it the C functions answer "not supported" and `interfaces()` lists
  Wi-Fi only, so the D1001 behaves as before. With it, the capability `network` lists
  `ethernet` as well.
- **N17 The wired link starts at boot and is looked at on every pass.** The driver starts
  with the stored setup (`eth_ip`, DHCP when absent) and returns at once; the selector of
  N8 sees link and address on every pass of the session loop and the default route follows
  it (esp-netif prefers Wi-Fi by its own priority, so the route is set explicitly).
- **N18 A candidate is proven by the session only when the session runs over it.** A
  session that falls back to Wi-Fi while a cable setup is tried says nothing about the
  cable. The trial therefore commits on a session only when the interface that carries it
  is one of the changed ones. For a changed interface that does not carry the session the
  proof is `link` (N7): a TCP connection to Home Assistant, bound to that interface with
  `SO_BINDTODEVICE`.

## Verified and not verified

Read in the code and the plans by an inventory of this repository: the points of the
baseline and the file locations behind them (theme in `model.py`, `settings.rs`,
`store.rs`, `commissioning.rs`; Wi-Fi in `njorun_d1001.c`; the configuration cycle in
`runtime.py` and `management.rs`).

Read in the ESP-IDF sources of the build (`esp_netif_lwip.c`, `esp_netif_handlers.c`):
`esp_netif_set_ip_info` needs the DHCP client stopped and clears the DNS servers, so the
DNS servers are set after it; with the client stopped and a valid address the connected
action posts `IP_EVENT_STA_GOT_IP` itself, so the existing handler needs no change.

Slice 4a is covered by unit tests on both sides against shared fixtures: the gate, the
planning and the stored record in `app-core`, the storage, the sending and the result
handling in Home Assistant. The firmware builds. Not run on a tablet: the trial itself
(the join with new values, the session proof, the revert, the report after a reset). Also
not verified: how long the join to the stored network takes after a failed trial, that a
blocking DNS lookup while Home Assistant is unreachable can delay the revert past the
45 seconds (the lookup is not bounded, see the host connection lifecycle), and that
Home Assistant's 180 seconds cover a slow reconnect.

Slice 5 is written and builds, and the pure parts are tested (the selector of N8, the
endpoint of the address a link proof connects to). It has not run on a board: the driver,
the link and address handling, the explicit default route, the bound TCP connection and
the fallback to Wi-Fi are all unverified. I read in the ESP-IDF sources of the build that
`esp_eth_phy_new_generic`, the P4 EMAC defaults, `esp_netif_set_default_netif` and
`SO_BINDTODEVICE` (lwIP `netif_find` by name) exist; that they behave as assumed is
not checked.

Not verified: that the netif which esp_hosted and esp_wifi_remote create for the D1001
is the standard station netif with its DHCP client flag, so that the above holds on the
tablet (its sources are not in this checkout); that the
station can stay associated to a second network while the new one is tried (it
cannot with one radio, so a Wi-Fi trial always drops the current Wi-Fi link, which
is why the trial of the interface in use must end with the session proof); that
lwIP's two DNS slots suffice; how Ethernet and Wi-Fi netif priorities behave on the
ESP-IDF version in use; whether the reconnect after the revert is quick enough for
Home Assistant's 60-second operation lifetime.

## Risks

- A Wi-Fi trial that cannot reach Home Assistant takes the tablet offline for the
  length of the proof timeout. The revert is local and needs nothing from the network.
- The password is in clear text in the tablet's NVS, as it is today, and now also in
  Home Assistant's storage. A removed tablet's stored credentials stay in the profile
  until the administrator clears them.
- A static address that collides with another host is not detected by the proof if
  the other host is quiet. The tablet should run address conflict detection when it
  applies a static address (to be checked in lwIP's settings).
- Wired priority on a board we cannot test can be written and unit-tested but not
  qualified; the slice stays marked as unverified until a board runs it.

## Answered by the maintainer (2026-11-12)

- **O1** The first theme choice is the starting value of the tablet's theme in Home
  Assistant (N1 as written).
- **O2** Slice 5 is built now, without a board. It is written and unit-tested but
  stays marked unverified until a board with a wired port runs it.
- **O5** Wi-Fi credentials live in the profile and can be overridden per tablet (N9 as
  written).
- **O4** `configuration` gets the same answer for a refusal as `network` (`applied` or
  `rejected` with a reason), and Home Assistant shows the reason.

## Open points

- **O3** IPv4 only is assumed in N3, without having been asked. Say so if IPv6
  addresses are needed.
- **O6** An address change alone cannot be sent while Home Assistant has no credentials
  for the tablet. The message names the network (N3), so a tablet whose Wi-Fi was set up
  on the tablet needs the name and password in Home Assistant before its static address
  can be set there. Say so if a message without credentials should keep the tablet's own.

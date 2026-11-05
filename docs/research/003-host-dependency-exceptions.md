# Host simulator dependency exceptions

Reviewed 2026-11-05. Scope: development desktop simulator only.

The crates.io API reports Slint and i-slint-backend-winit **1.18.1** as their
latest stable releases on this date. The downloaded backend's Cargo manifest
requires zbus `5.14.0` (resolved 5.19.0) on non-Apple, non-Windows desktop
platforms. This dependency is unconditional for its Linux backend, including
X11-only builds. It reads XDG portal desktop settings; it is not the HA transport
or the embedded firmware adapter.

| Exception | Locked crate | Upstream disposition |
| --- | --- | --- |
| [RUSTSEC-2026-0370](https://rustsec.org/advisories/RUSTSEC-2026-0370.html) | zvariant 5.15.1 | Standalone crate unmaintained; merged into zbus 6 wire module |
| [RUSTSEC-2026-0371](https://rustsec.org/advisories/RUSTSEC-2026-0371.html) | zbus_names 4.3.5 | Standalone crate unmaintained; merged into zbus 6 names module |
| [RUSTSEC-2026-0372](https://rustsec.org/advisories/RUSTSEC-2026-0372.html) | zvariant_derive 5.15.1 | Standalone crate unmaintained; macros moved into zbus 6 |

These advisories are informational maintenance notices, with no patched
versions listed. A lockfile-only update cannot satisfy Slint's zbus 5 requirement
with zbus 6. A local upstream fork solely to change this dependency would add a
maintenance burden to this prototype. We temporarily accept these three specific
notices while retaining the current stable native backend. This is not a claim
that unmaintained dependencies are safe indefinitely.

`deny.toml` names each advisory and its removal condition. Remove the exceptions
when Slint migrates to zbus 6, when the dependency is removed, or when a supported
backend avoids it. Recheck at every Slint update and before publishing a desktop
binary; review again by 2026-12-05 if still present. New security advisories,
yanked versions and license failures remain blocking.

Slint's open-source GPL-3.0-only license option is accepted for the simulator's
combined binary. Original Njörun source remains GPL-3.0-or-later; this does not
relicense third-party code or imply access to Slint's proprietary license.
The additional BSD-2-Clause, BSL-1.0, Zlib and CDLA-Permissive-2.0 entries in the
license allowlist cover the native renderer's dependency tree and trust data.

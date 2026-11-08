# D1001 setup persistence

The D1001 firmware keeps its setup across restarts, so a power cycle returns to
the dashboard without the commissioning screens.

## What is stored

Everything lives in the `njorun` namespace of the 24 KB `nvs` partition
(`partitions.d1001.csv`, offset `0x9000`). It is **plaintext**.

| Key | Content |
| --- | --- |
| `wifi_ssid`, `wifi_pass` | The network that last connected successfully. Written only after a successful connect, so a wrong password is never saved. |
| `ha_url` | Home Assistant base URL used for sign-in. |
| `ha_client` | The OAuth `client_id` of that sign-in (`http://<tablet-ip>:<port>/client`). Home Assistant binds a refresh token to it, so it must be reused unchanged. |
| `ha_refresh` | The long-lived refresh token. Access tokens are never stored. |
| `lang` | `en` or `de`. |
| `tablet_id` | The tablet's management ID, `njorun-` followed by 32 random hex digits. Not a secret; Home Assistant uses it to recognize the tablet. |
| `tablet_key` | The tablet's static X25519 private key for the management link to the Njörun integration, as 64 hex digits. It never leaves the tablet. |
| `mgmt_binding`, `mgmt_ha_key` | The binding ID the Njörun integration issued when an administrator approved this tablet, and the integration's public key for that binding, which the tablet pins. Cleared when HA no longer knows the binding; the tablet then waits for approval again. |

| `settings` | The settings Home Assistant last sent and the tablet applied, as JSON with their revision (at most about 3 KB). Written before the tablet acknowledges them; if it cannot be written, the settings are not acknowledged. |
| `orient`, `theme`, `daylight` | The orientation and theme in effect, and the sun state Home Assistant last reported. Once the tablet is paired, `lang`, `orient` and `theme` are rewritten from the applied settings, so they hold at boot before Home Assistant answers. |

Applied settings: language, theme, orientation (portrait or landscape, keeping the
tablet's flip) and brightness take effect. Idle timeout and brightness, screensaver,
home view, favorites, room and entity order, exclusions and the update settings are
stored and acknowledged but not enacted by the firmware yet.

### Tablet identity

The firmware creates `tablet_id` and `tablet_key` on the first start, from the
ESP32-P4 hardware random number generator. ESP-IDF keeps the SAR ADC entropy
source enabled after startup on this chip, so the bytes are true random
numbers. The pair is written, read back and compared before it is used; if
storage cannot be read or written, the tablet runs without an identity rather
than with one that would change on the next start. A damaged or half-written
pair is replaced by a new identity, which has to be paired with Home Assistant
again. Updates keep the identity.

The module is `experiments/p4-build/src/store.rs`, backed by
`njorun_d1001_store_*` in the D1001 component.

## Boot behavior

1. If both Wi-Fi and Home Assistant credentials are saved, the tablet shows
   "Connecting to Wi-Fi", joins the saved network, exchanges the refresh token
   for a fresh access token at `/auth/token`, loads the areas over the
   WebSocket and opens the dashboard.
2. Otherwise, or on failure, it falls back to setup:

| Failure | Result | Saved data |
| --- | --- | --- |
| Saved Wi-Fi unreachable | Wi-Fi list with an explanation | kept |
| Home Assistant unreachable or HTTP 5xx | Home Assistant discovery with an explanation | kept |
| Home Assistant answers 400, 401 or 403 (token revoked or client unknown) | Home Assistant discovery with an explanation | Home Assistant session erased |

A successful new sign-in overwrites the saved session.

## Resetting

Tap the Home Assistant icon in the dashboard header, then **Reset setup** and
confirm with a second tap. This erases the whole namespace, including the
tablet identity, and restarts the device. The tablet then starts with a new
identity and has to be paired with Home Assistant again. Erasing the flash with `espflash erase-flash` does the same.

## Security limits

Anyone with physical access to the board can read the Wi-Fi password and the
Home Assistant refresh token from flash. The refresh token grants the access of
the user who signed in. Revoke it in Home Assistant (profile, security,
refresh tokens) if a device is lost or sold. The same applies to
`tablet_key`: whoever reads it can act as this tablet towards the Njörun
integration, so remove the tablet in the integration as well.

This firmware does **not** enable secure boot or flash encryption, and it does
not depend on either. Application-level encryption was deliberately left out:
without a key that sits in protected hardware, a key stored in the same flash
would only obscure the data.

### Irreversible steps (not taken)

These burn one-time-programmable eFuses on the ESP32-P4. They cannot be undone
and can make the board unrecoverable if done wrongly:

- **Secure boot v2:** burns the signing-key digest. Only images signed with
  that key will ever boot, and a lost key means no more firmware updates.
- **Flash encryption (release mode):** burns the encryption key and disables
  plain UART/USB flashing for good. The development mode still allows a limited
  number of re-flashes.
- **NVS encryption with the HMAC scheme:** burns an HMAC key into an eFuse
  block; the key purpose cannot be changed afterwards.
- **Disabling USB download mode or JTAG:** burns eFuse bits that remove those
  recovery paths.

Reversible alternatives within this design: erasing the `nvs` partition,
revoking the refresh token in Home Assistant, and re-running setup.

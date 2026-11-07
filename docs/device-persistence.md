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
confirm with a second tap. This erases the whole namespace and restarts the
device. Erasing the flash with `espflash erase-flash` does the same.

## Security limits

Anyone with physical access to the board can read the Wi-Fi password and the
Home Assistant refresh token from flash. The refresh token grants the access of
the user who signed in. Revoke it in Home Assistant (profile, security,
refresh tokens) if a device is lost or sold.

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

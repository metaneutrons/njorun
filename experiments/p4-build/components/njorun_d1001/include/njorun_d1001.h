// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

/* Causes of a failed connection, returned by the HTTP and Home Assistant session calls. */
#define NJORUN_D1001_ERR_DNS          0x6E01 /* host name did not resolve */
#define NJORUN_D1001_ERR_TIMEOUT      0x6E02 /* no answer: often a firewall or routing rule */
#define NJORUN_D1001_ERR_REFUSED      0x6E03 /* host answered, nothing listens on the port */
#define NJORUN_D1001_ERR_UNREACHABLE  0x6E04 /* no route to the host or network */
#define NJORUN_D1001_ERR_CERTIFICATE  0x6E05 /* TLS handshake or certificate check failed */

#ifdef __cplusplus
extern "C" {
#endif

#define NJORUN_D1001_WIDTH 800U
#define NJORUN_D1001_HEIGHT 1280U
#define NJORUN_D1001_MAX_WIFI_NETWORKS 16U
#define NJORUN_D1001_WIFI_SSID_CAPACITY 33U
#define NJORUN_D1001_MAX_HA_INSTANCES 8U
#define NJORUN_D1001_HA_NAME_CAPACITY 129U
#define NJORUN_D1001_HA_ENDPOINT_CAPACITY 512U

typedef struct {
    char ssid[NJORUN_D1001_WIFI_SSID_CAPACITY];
    int8_t rssi;
    bool password_required;
} njorun_d1001_wifi_network_t;

typedef struct {
    uint16_t x;
    uint16_t y;
    bool pressed;
} njorun_d1001_touch_point_t;

typedef struct {
    char name[NJORUN_D1001_HA_NAME_CAPACITY];
    char endpoint[NJORUN_D1001_HA_ENDPOINT_CAPACITY];
} njorun_d1001_ha_instance_t;

/**
 * Initialize the minimum D1001 power rails and native portrait DSI display.
 *
 * The backlight remains off so the caller can render a complete first frame.
 * The returned RGB565 framebuffer is owned by ESP-IDF and remains valid for
 * the lifetime of the application.
 */
esp_err_t njorun_d1001_display_init(uint16_t **framebuffer, size_t *pixel_count);

/** Flush CPU writes to the continuously scanned DSI framebuffer. */
esp_err_t njorun_d1001_display_sync(void);

/** Set backlight brightness from 0 through 100 percent. */
esp_err_t njorun_d1001_backlight_set(uint8_t percent);

/** Initialize the GSL3670 touch controller in native portrait orientation. */
esp_err_t njorun_d1001_touch_init(void);

/** Poll the primary touch point. A released screen returns pressed=false. */
esp_err_t njorun_d1001_touch_read(njorun_d1001_touch_point_t *point);

/** Initialize the ESP32-C6 remote Wi-Fi station through ESP-Hosted SDIO. */
esp_err_t njorun_d1001_wifi_init(void);

/** Perform a bounded blocking scan, strongest access point first. */
esp_err_t njorun_d1001_wifi_scan(
    njorun_d1001_wifi_network_t *networks,
    size_t capacity,
    size_t *network_count
);

/** IPv4 setup of the Wi-Fi station: the router's DHCP, or a fixed address (plan 006). */
typedef struct {
    bool use_static;        /**< false: DHCP, the other fields are ignored. */
    uint8_t address[4];     /**< Network order. */
    uint8_t gateway[4];
    uint8_t prefix;         /**< Subnet prefix length, 1 to 30. */
    uint8_t dns_count;      /**< 0 to 2 servers; with 0 the station has none. */
    uint8_t dns[2][4];
    char hostname[32];      /**< Sent in DHCP (option 12); empty keeps the default. */
} njorun_d1001_ip_config_t;

/**
 * Connect and wait for an IPv4 address, or return a bounded failure. `ip` NULL means DHCP.
 * A fixed setup stops the DHCP client before the station joins, so the address is in use
 * as soon as the link is up; a later call with DHCP starts the client again.
 */
esp_err_t njorun_d1001_wifi_connect(
    const char *ssid,
    const char *password,
    const njorun_d1001_ip_config_t *ip
);

/**
 * Why the last njorun_d1001_wifi_connect() failed: 0 it did not, 1 the network could not be
 * joined (name, password or reach), 2 it was joined but no address arrived in time or the
 * fixed one could not be used, 3 anything else.
 */
int njorun_d1001_wifi_last_failure(void);

/** Read the current station IPv4 address as four network-order octets. */
esp_err_t njorun_d1001_wifi_ipv4(uint8_t address[4]);

/* Interface numbers for njorun_d1001_prefer_interface() and njorun_d1001_tcp_probe(). */
#define NJORUN_D1001_INTERFACE_WIFI 0
#define NJORUN_D1001_INTERFACE_ETHERNET 1

/** Whether this build has a wired interface (CONFIG_NJORUN_ETHERNET). The D1001 has none. */
bool njorun_d1001_ethernet_present(void);

/**
 * Starts the wired interface with `ip` (NULL means DHCP), or applies new values to it when it
 * runs. Returns at once: the link and the address come later, see njorun_d1001_ethernet_wait().
 * ESP_ERR_NOT_SUPPORTED without CONFIG_NJORUN_ETHERNET. Unverified on hardware.
 */
esp_err_t njorun_d1001_ethernet_start(const njorun_d1001_ip_config_t *ip);

/** Waits for the link and an address. ESP_ERR_TIMEOUT afterwards, see the failure number. */
esp_err_t njorun_d1001_ethernet_wait(uint32_t timeout_ms);

/** Why the last Ethernet start or wait failed: 0 it did not, 1 no link, 2 no address, 3 other. */
int njorun_d1001_ethernet_last_failure(void);

/** The wired IPv4 address as four network-order octets; ESP_ERR_INVALID_STATE without link or address. */
esp_err_t njorun_d1001_ethernet_ipv4(uint8_t address[4]);

/** Makes the given interface carry the default route (the wired link has priority, N8). */
esp_err_t njorun_d1001_prefer_interface(int interface);

/**
 * Opens a TCP connection to host:port over one interface only, and closes it again. This is the
 * proof of a link that does not carry the session (decision N7).
 */
esp_err_t njorun_d1001_tcp_probe(int interface, const char *host, uint16_t port, uint32_t timeout_ms);

/* Signal of the current access point in dBm; ESP_ERR_INVALID_STATE without a link. */
esp_err_t njorun_d1001_wifi_rssi(int8_t *rssi);

/** Discover Home Assistant's documented DNS-SD service on the current LAN. */
esp_err_t njorun_d1001_ha_discover(
    njorun_d1001_ha_instance_t *instances,
    size_t capacity,
    size_t *instance_count
);

/** Fill a caller-owned buffer from ESP-IDF's hardware-backed random source. */
esp_err_t njorun_d1001_random(void *buffer, size_t length);

/** POST an application/x-www-form-urlencoded body and return a bounded response. */
esp_err_t njorun_d1001_http_post_form(
    const char *url,
    const char *body,
    char *response,
    size_t response_capacity,
    size_t *response_length
);

/**
 * The USB-C serial port (plan 005): the first setup talks Improv frames over it. Reading and
 * writing use the USB-Serial-JTAG driver, not the console, which would turn a line feed in a
 * binary frame into a carriage return and a line feed. `serial_read` waits up to `timeout_ms`
 * and returns with a length of 0 when nothing came.
 */
esp_err_t njorun_d1001_serial_read(
    uint8_t *buffer,
    size_t capacity,
    uint32_t timeout_ms,
    size_t *length
);
esp_err_t njorun_d1001_serial_write(const uint8_t *data, size_t length);

/** Set the system clock to UTC Unix seconds (a profile sent over USB carries the host's time). */
esp_err_t njorun_d1001_set_utc(int64_t seconds);

/**
 * POST an application/json body and return the HTTP status and a bounded response. Any HTTP
 * status is a success of the call (plan 004: the announcement endpoint answers 404, 429 and
 * 503 on purpose); only a failed connection, TLS or timeout is an error.
 */
esp_err_t njorun_d1001_http_post_json(
    const char *url,
    const char *body,
    char *response,
    size_t response_capacity,
    size_t *response_length,
    int *status
);

/**
 * GET with a bearer token and return a bounded binary body (for example a camera picture).
 * HTTP 401 and 403 are reported as ESP_ERR_NOT_ALLOWED, other failures as ESP_ERR_INVALID_RESPONSE.
 */
esp_err_t njorun_d1001_http_get_bearer(
    const char *url,
    const char *token,
    char *response,
    size_t response_capacity,
    size_t *response_length
);

/** Authenticate one HA WebSocket and return the area-registry result message. */
esp_err_t njorun_d1001_ha_fetch_areas(
    const char *websocket_url,
    const char *auth_message,
    char *response,
    size_t response_capacity,
    size_t *response_length
);

/**
 * Persistent settings in the "njorun" NVS namespace. Values are stored as
 * plaintext: flash encryption and secure boot are not enabled on this device.
 * Returns ESP_ERR_NOT_FOUND when the key is absent.
 */
esp_err_t njorun_d1001_store_get(
    const char *key,
    char *value,
    size_t capacity,
    size_t *length
);
esp_err_t njorun_d1001_store_set(const char *key, const char *value);
esp_err_t njorun_d1001_store_erase_all(void);

/**
 * One persistent, authenticated Home Assistant WebSocket. Requests are serialized and each
 * returns the next non-event message; state_changed events are queued for poll_event.
 */
esp_err_t njorun_d1001_ha_session_open(const char *websocket_url, const char *auth_message);
esp_err_t njorun_d1001_ha_session_request(
    const char *message,
    char *response,
    size_t response_capacity,
    size_t *response_length,
    uint32_t timeout_ms
);
/**
 * Send without waiting. Replies arrive through poll_event, like events, because only a
 * request armed by ha_session_request owns the response buffer.
 */
esp_err_t njorun_d1001_ha_session_send(const char *message);
esp_err_t njorun_d1001_ha_session_poll_event(
    char *buffer,
    size_t capacity,
    size_t *length,
    uint32_t timeout_ms
);
bool njorun_d1001_ha_session_take_events_lost(void);
bool njorun_d1001_ha_session_alive(void);
void njorun_d1001_ha_session_close(void);

/** Keeps a crash message across the next software reset; read it once with crash_take. */
void njorun_d1001_crash_save(const char *message);
size_t njorun_d1001_crash_take(char *buffer, size_t capacity);
/** Why the chip last reset, as a short English phrase. */
const char *njorun_d1001_reset_reason(void);

/**
 * Logging. log_init captures ESP-IDF's console output, keeps a crash ring in RTC memory
 * (survives a software reset) and a queue for the log sink. log_append adds the app's own
 * lines. log_previous returns the crash ring of the boot before this one, log_drain takes
 * whole queued lines and reports how many were dropped because the queue was full.
 */
esp_err_t njorun_d1001_log_init(void);
void njorun_d1001_log_append(const char *line, size_t length);
uint32_t njorun_d1001_log_boot(void);
size_t njorun_d1001_log_previous(char *buffer, size_t capacity, uint32_t *boot);
size_t njorun_d1001_log_drain(char *buffer, size_t capacity, uint32_t *dropped);
/** POST a body with a content type; any 2xx answer is success. */
esp_err_t njorun_d1001_http_post(
    const char *url,
    const char *content_type,
    const char *body,
    size_t body_length,
    uint32_t timeout_ms
);

/**
 * Firmware install (decision D7). ota_partitions counts the OTA app slots of the partition
 * table that is flashed and gives the size of the inactive one. The install needs two.
 * ota_verdict_pending is true while the bootloader waits for this image to be marked valid;
 * ota_mark_valid confirms it, ota_rollback returns to the previous image and does not return.
 */
esp_err_t njorun_d1001_ota_partitions(uint32_t *ota_count, uint32_t *slot_bytes);
bool njorun_d1001_ota_verdict_pending(void);
esp_err_t njorun_d1001_ota_mark_valid(void);
void njorun_d1001_ota_rollback(void);

/**
 * One streaming GET with a bearer token and no redirects. HTTP 401 and 403 are reported as
 * ESP_ERR_NOT_ALLOWED. http_read returns length 0 at the end of the body and an error when
 * the body was cut off.
 */
esp_err_t njorun_d1001_ota_http_open(const char *url, const char *token, uint32_t *content_length);
esp_err_t njorun_d1001_ota_http_read(void *buffer, size_t capacity, size_t *length);
void njorun_d1001_ota_http_close(void);

/**
 * The inactive OTA slot. begin erases for an image of `size` bytes, end validates the image
 * header and checksum, activate makes the slot the next boot slot, abort drops everything
 * (also the HTTP stream).
 */
esp_err_t njorun_d1001_ota_begin(uint32_t size);
esp_err_t njorun_d1001_ota_write(const void *data, size_t length);
esp_err_t njorun_d1001_ota_end(void);
void njorun_d1001_ota_abort(void);
esp_err_t njorun_d1001_ota_activate(void);

#ifdef __cplusplus
}
#endif

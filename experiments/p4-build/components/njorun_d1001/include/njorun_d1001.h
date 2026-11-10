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

/** Connect and wait for an IPv4 address, or return a bounded failure. */
esp_err_t njorun_d1001_wifi_connect(const char *ssid, const char *password);

/** Read the current station IPv4 address as four network-order octets. */
esp_err_t njorun_d1001_wifi_ipv4(uint8_t address[4]);

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

#ifdef __cplusplus
}
#endif

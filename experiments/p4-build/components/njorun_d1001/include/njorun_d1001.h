// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define NJORUN_D1001_WIDTH 800U
#define NJORUN_D1001_HEIGHT 1280U
#define NJORUN_D1001_MAX_WIFI_NETWORKS 16U
#define NJORUN_D1001_WIFI_SSID_CAPACITY 33U

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

/** Begin connecting to one access point. Completion is reported by ESP-IDF events. */
esp_err_t njorun_d1001_wifi_connect(const char *ssid, const char *password);

#ifdef __cplusplus
}
#endif

// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#pragma once

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define NJORUN_D1001_WIDTH 800U
#define NJORUN_D1001_HEIGHT 1280U

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

#ifdef __cplusplus
}
#endif

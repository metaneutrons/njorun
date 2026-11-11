# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later

configure_file(
    "${CMAKE_CURRENT_LIST_DIR}/../../partitions.d1001.csv"
    "${PROJECT_DIR}/partitions.d1001.csv"
    COPYONLY
)

idf_component_register(
    SRCS "njorun_d1001.c"
    INCLUDE_DIRS "include"
    REQUIRES
        app_update
        driver
        esp_driver_gpio
        esp_driver_i2c
        esp_driver_ledc
        esp_hw_support
        esp_http_client
        esp_lcd_touch_gsl3670
        esp_lcd
        esp_mm
        esp_netif
        esp_partition
        esp_wifi
        espressif__esp_websocket_client
        espressif__esp_io_expander
        espressif__mdns
        mbedtls
        nvs_flash
        waveshare__esp_lcd_jd9365_8
)

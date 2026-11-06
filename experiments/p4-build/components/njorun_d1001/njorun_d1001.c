// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#include "njorun_d1001.h"

#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "arpa/inet.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "esp_cache.h"
#include "esp_check.h"
#include "esp_crt_bundle.h"
#include "esp_http_client.h"
#include "esp_io_expander.h"
#include "esp_lcd_jd9365_8.h"
#include "esp_lcd_mipi_dsi.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_touch.h"
#include "esp_lcd_touch_gsl3670.h"
#include "esp_ldo_regulator.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_netif_sntp.h"
#include "esp_system.h"
#include "esp_websocket_client.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"
#include "mdns.h"
#include "nvs_flash.h"
#include "soc/pmu_reg.h"
#include "soc/soc.h"

static const char *TAG = "njorun_d1001";

enum {
    I2C0_SDA = 37,
    I2C0_SCL = 38,
    I2C1_SDA = 20,
    I2C1_SCL = 21,
    LCD_BACKLIGHT = 14,
    PCA9535_ADDRESS = 0x20,
    PCA9535_INPUT_0 = 0x00,
    PCA9535_OUTPUT_0 = 0x02,
    PCA9535_CONFIG_0 = 0x06,
    EXP_LCD_POWER = 1U << 0,
    EXP_LCD_RESET = 1U << 2,
    EXP_LCD_BACKLIGHT_ENABLE = 1U << 7,
    EXP_POWER_HOLD = 1U << 8,
};

static i2c_master_bus_handle_t s_touch_i2c_bus;
static i2c_master_bus_handle_t s_i2c_bus;
static i2c_master_dev_handle_t s_expander;
static esp_ldo_channel_handle_t s_dsi_ldo;
static esp_lcd_dsi_bus_handle_t s_dsi_bus;
static esp_lcd_panel_io_handle_t s_panel_io;
static esp_lcd_panel_handle_t s_panel;
static esp_lcd_panel_io_handle_t s_touch_io;
static esp_lcd_touch_handle_t s_touch;
static uint16_t *s_framebuffer;
static uint16_t s_expander_outputs;
static uint16_t s_expander_directions;
static uint16_t s_touch_x;
static uint16_t s_touch_y;
static bool s_touch_pressed;
static bool s_backlight_initialized;
static bool s_wifi_initialized;
static EventGroupHandle_t s_wifi_events;
static bool s_sntp_initialized;

enum {
    WIFI_CONNECTED_BIT = BIT0,
    WIFI_FAILED_BIT = BIT1,
    WEBSOCKET_CONNECTED_BIT = BIT0,
    WEBSOCKET_DATA_BIT = BIT1,
    WEBSOCKET_FAILED_BIT = BIT2,
};

typedef struct {
    char *buffer;
    size_t capacity;
    size_t length;
    bool overflow;
} http_response_t;

typedef struct {
    EventGroupHandle_t events;
    char *buffer;
    size_t capacity;
    size_t length;
    bool overflow;
} websocket_response_t;

static esp_err_t expander_write_pair(uint8_t first_register, uint16_t value)
{
    const uint8_t bytes[] = {
        first_register,
        (uint8_t)(value & 0xffU),
        (uint8_t)(value >> 8),
    };
    return i2c_master_transmit(s_expander, bytes, sizeof(bytes), 100);
}

static esp_err_t expander_read_pair(uint8_t first_register, uint16_t *value)
{
    if (value == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    uint8_t bytes[2] = {0};
    ESP_RETURN_ON_ERROR(
        i2c_master_transmit_receive(s_expander, &first_register, 1, bytes, sizeof(bytes), 100),
        TAG,
        "PCA9535 read failed"
    );
    *value = (uint16_t)bytes[0] | ((uint16_t)bytes[1] << 8);
    return ESP_OK;
}

static esp_err_t expander_commit(void)
{
    return expander_write_pair(PCA9535_OUTPUT_0, s_expander_outputs);
}

static esp_err_t adapter_read_input(esp_io_expander_handle_t handle, uint32_t *value)
{
    (void)handle;
    if (value == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    uint16_t inputs = 0;
    ESP_RETURN_ON_ERROR(expander_read_pair(PCA9535_INPUT_0, &inputs), TAG, "PCA9535 input failed");
    *value = inputs;
    return ESP_OK;
}

static esp_err_t adapter_write_output(esp_io_expander_handle_t handle, uint32_t value)
{
    (void)handle;
    s_expander_outputs = (uint16_t)value;
    return expander_commit();
}

static esp_err_t adapter_read_output(esp_io_expander_handle_t handle, uint32_t *value)
{
    (void)handle;
    if (value == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    *value = s_expander_outputs;
    return ESP_OK;
}

static esp_err_t adapter_write_direction(esp_io_expander_handle_t handle, uint32_t value)
{
    (void)handle;
    s_expander_directions = (uint16_t)value;
    return expander_write_pair(PCA9535_CONFIG_0, s_expander_directions);
}

static esp_err_t adapter_read_direction(esp_io_expander_handle_t handle, uint32_t *value)
{
    (void)handle;
    if (value == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    *value = s_expander_directions;
    return ESP_OK;
}

static esp_err_t adapter_reset(esp_io_expander_handle_t handle)
{
    (void)handle;
    /* Resetting this shared expander would drop the D1001 power-hold rail. */
    return ESP_ERR_NOT_SUPPORTED;
}

static esp_err_t adapter_delete(esp_io_expander_handle_t handle)
{
    (void)handle;
    return ESP_OK;
}

static esp_io_expander_t s_expander_adapter = {
    .read_input_reg = adapter_read_input,
    .write_output_reg = adapter_write_output,
    .read_output_reg = adapter_read_output,
    .write_direction_reg = adapter_write_direction,
    .read_direction_reg = adapter_read_direction,
    .reset = adapter_reset,
    .del = adapter_delete,
    .config = {
        .io_count = 16,
        .flags = {
            .dir_out_bit_zero = 1,
        },
    },
};

/* Seeed's GSL3670 driver requires this board-level symbol. It points at the
 * safe adapter above rather than Seeed's PCA9535 reset implementation. */
esp_io_expander_handle_t io_expander = &s_expander_adapter;

static esp_err_t safe_touch_read_data(esp_lcd_touch_handle_t touch)
{
    if (touch == NULL || touch->io == NULL) {
        return ESP_ERR_INVALID_ARG;
    }

    /* The upstream driver requests 44 bytes into a 24-byte local array. Only
     * the status header and primary point are required for this single-touch
     * UI, so read exactly the eight bytes that are parsed below. */
    uint8_t data[8] = {0};
    ESP_RETURN_ON_ERROR(
        esp_lcd_panel_io_rx_param(touch->io, 0x80, data, sizeof(data)),
        TAG,
        "GSL3670 coordinate read failed"
    );
    s_touch_pressed = data[0] > 0;
    if (s_touch_pressed) {
        s_touch_x = (uint16_t)(((uint16_t)(data[7] & 0x0fU) << 8) | data[6]);
        s_touch_y = (uint16_t)(((uint16_t)data[5] << 8) | data[4]);
    }
    return ESP_OK;
}

static bool safe_touch_get_xy(
    esp_lcd_touch_handle_t touch,
    uint16_t *x,
    uint16_t *y,
    uint16_t *strength,
    uint8_t *point_count,
    uint8_t max_point_count
)
{
    (void)touch;
    if (x == NULL || y == NULL || point_count == NULL || max_point_count == 0) {
        return false;
    }
    if (!s_touch_pressed) {
        *point_count = 0;
        return false;
    }
    x[0] = s_touch_x;
    y[0] = s_touch_y;
    if (strength != NULL) {
        strength[0] = 1;
    }
    *point_count = 1;
    return true;
}

static esp_err_t power_init(void)
{
    SET_PERI_REG_BITS(
        PMU_HP_ACTIVE_BIAS_REG,
        PMU_HP_ACTIVE_DCM_VSET,
        26,
        PMU_HP_ACTIVE_DCM_VSET_S
    );
    vTaskDelay(pdMS_TO_TICKS(1000));

    const i2c_master_bus_config_t bus_config = {
        .i2c_port = 1,
        .sda_io_num = I2C1_SDA,
        .scl_io_num = I2C1_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    ESP_RETURN_ON_ERROR(i2c_new_master_bus(&bus_config, &s_i2c_bus), TAG, "I2C1 init failed");

    const i2c_device_config_t expander_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = PCA9535_ADDRESS,
        .scl_speed_hz = 400000,
    };
    ESP_RETURN_ON_ERROR(
        i2c_master_bus_add_device(s_i2c_bus, &expander_config, &s_expander),
        TAG,
        "PCA9535 init failed"
    );

    /* Program safe output latches before changing the pins from inputs. Unused
     * camera, modem and amplifier rails stay low throughout initialization. */
    s_expander_outputs = EXP_POWER_HOLD;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "PCA9535 output latch failed");
    s_expander_directions = 0;
    ESP_RETURN_ON_ERROR(
        expander_write_pair(PCA9535_CONFIG_0, s_expander_directions),
        TAG,
        "PCA9535 direction failed"
    );
    vTaskDelay(pdMS_TO_TICKS(50));

    s_expander_outputs |= EXP_LCD_POWER | EXP_LCD_BACKLIGHT_ENABLE;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "LCD rail enable failed");
    vTaskDelay(pdMS_TO_TICKS(50));

    s_expander_outputs |= EXP_LCD_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "LCD reset release failed");
    return ESP_OK;
}

static esp_err_t panel_hardware_reset(void)
{
    s_expander_outputs |= EXP_LCD_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "LCD reset high failed");
    vTaskDelay(pdMS_TO_TICKS(5));
    s_expander_outputs &= (uint16_t)~EXP_LCD_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "LCD reset low failed");
    vTaskDelay(pdMS_TO_TICKS(10));
    s_expander_outputs |= EXP_LCD_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "LCD reset release failed");
    vTaskDelay(pdMS_TO_TICKS(120));
    return ESP_OK;
}

static esp_err_t panel_init(void)
{
    const esp_ldo_channel_config_t ldo_config = {
        .chan_id = 3,
        .voltage_mv = 2500,
    };
    ESP_RETURN_ON_ERROR(
        esp_ldo_acquire_channel(&ldo_config, &s_dsi_ldo),
        TAG,
        "DSI PHY LDO failed"
    );

    const esp_lcd_dsi_bus_config_t dsi_config = {
        .bus_id = 0,
        .num_data_lanes = 2,
        .phy_clk_src = MIPI_DSI_PHY_CLK_SRC_DEFAULT,
        .lane_bit_rate_mbps = 1000,
    };
    ESP_RETURN_ON_ERROR(esp_lcd_new_dsi_bus(&dsi_config, &s_dsi_bus), TAG, "DSI bus failed");

    const esp_lcd_dbi_io_config_t dbi_config = {
        .virtual_channel = 0,
        .lcd_cmd_bits = 8,
        .lcd_param_bits = 8,
    };
    ESP_RETURN_ON_ERROR(
        esp_lcd_new_panel_io_dbi(s_dsi_bus, &dbi_config, &s_panel_io),
        TAG,
        "DBI panel IO failed"
    );

    esp_lcd_dpi_panel_config_t dpi_config = {
        .virtual_channel = 0,
        .dpi_clk_src = MIPI_DSI_DPI_CLK_SRC_DEFAULT,
        .dpi_clock_freq_mhz = 40,
        .in_color_format = LCD_COLOR_FMT_RGB565,
        .out_color_format = LCD_COLOR_FMT_RGB565,
        .num_fbs = 1,
        .video_timing = {
            .h_size = NJORUN_D1001_WIDTH,
            .v_size = NJORUN_D1001_HEIGHT,
            .hsync_back_porch = 20,
            .hsync_front_porch = 40,
            .hsync_pulse_width = 20,
            .vsync_back_porch = 30,
            .vsync_front_porch = 30,
            .vsync_pulse_width = 4,
        },
    };
    const jd9365_8_vendor_config_t vendor_config = {
        .mipi_config = {
            .dsi_bus = s_dsi_bus,
            .dpi_config = &dpi_config,
            .lane_num = 2,
        },
    };
    const esp_lcd_panel_dev_config_t panel_config = {
        .reset_gpio_num = GPIO_NUM_NC,
        .rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB,
        .bits_per_pixel = 16,
        .vendor_config = (void *)&vendor_config,
    };
    ESP_RETURN_ON_ERROR(
        esp_lcd_new_panel_jd9365_8(s_panel_io, &panel_config, &s_panel),
        TAG,
        "JD9365 create failed"
    );
    ESP_RETURN_ON_ERROR(panel_hardware_reset(), TAG, "JD9365 hardware reset failed");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_init(s_panel), TAG, "JD9365 init failed");
    ESP_RETURN_ON_ERROR(esp_lcd_panel_disp_on_off(s_panel, true), TAG, "JD9365 display on failed");
    ESP_RETURN_ON_ERROR(
        esp_lcd_dpi_panel_get_frame_buffer(s_panel, 1, (void **)&s_framebuffer),
        TAG,
        "JD9365 framebuffer unavailable"
    );
    return ESP_OK;
}

esp_err_t njorun_d1001_display_init(uint16_t **framebuffer, size_t *pixel_count)
{
    if (framebuffer == NULL || pixel_count == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (s_framebuffer == NULL) {
        ESP_RETURN_ON_ERROR(power_init(), TAG, "D1001 power init failed");
        ESP_RETURN_ON_ERROR(panel_init(), TAG, "D1001 panel init failed");
    }
    *framebuffer = s_framebuffer;
    *pixel_count = (size_t)NJORUN_D1001_WIDTH * NJORUN_D1001_HEIGHT;
    ESP_LOGI(TAG, "D1001 native framebuffer ready: %ux%u RGB565", NJORUN_D1001_WIDTH, NJORUN_D1001_HEIGHT);
    return ESP_OK;
}

esp_err_t njorun_d1001_display_sync(void)
{
    if (s_framebuffer == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    return esp_cache_msync(
        s_framebuffer,
        (size_t)NJORUN_D1001_WIDTH * NJORUN_D1001_HEIGHT * sizeof(uint16_t),
        ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED
    );
}

esp_err_t njorun_d1001_backlight_set(uint8_t percent)
{
    if (!s_backlight_initialized) {
        const ledc_timer_config_t timer = {
            .speed_mode = LEDC_LOW_SPEED_MODE,
            .duty_resolution = LEDC_TIMER_10_BIT,
            .timer_num = LEDC_TIMER_0,
            .freq_hz = 5000,
            .clk_cfg = LEDC_AUTO_CLK,
        };
        ESP_RETURN_ON_ERROR(ledc_timer_config(&timer), TAG, "Backlight timer failed");
        const ledc_channel_config_t channel = {
            .gpio_num = LCD_BACKLIGHT,
            .speed_mode = LEDC_LOW_SPEED_MODE,
            .channel = LEDC_CHANNEL_0,
            .timer_sel = LEDC_TIMER_0,
            .duty = 0,
            .hpoint = 0,
        };
        ESP_RETURN_ON_ERROR(ledc_channel_config(&channel), TAG, "Backlight channel failed");
        s_backlight_initialized = true;
    }
    if (percent > 100U) {
        percent = 100U;
    }
    const uint32_t duty = 1023U * percent / 100U;
    ESP_RETURN_ON_ERROR(
        ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0, duty),
        TAG,
        "Backlight duty failed"
    );
    return ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_0);
}

esp_err_t njorun_d1001_touch_init(void)
{
    if (s_touch != NULL) {
        return ESP_OK;
    }
    if (s_framebuffer == NULL || s_expander == NULL) {
        return ESP_ERR_INVALID_STATE;
    }

    const i2c_master_bus_config_t bus_config = {
        .i2c_port = 0,
        .sda_io_num = I2C0_SDA,
        .scl_io_num = I2C0_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    ESP_RETURN_ON_ERROR(
        i2c_new_master_bus(&bus_config, &s_touch_i2c_bus),
        TAG,
        "touch I2C init failed"
    );

    esp_lcd_panel_io_i2c_config_t io_config = ESP_LCD_TOUCH_IO_I2C_GSL3670_CONFIG();
    io_config.scl_speed_hz = 400000;
    ESP_RETURN_ON_ERROR(
        esp_lcd_new_panel_io_i2c(s_touch_i2c_bus, &io_config, &s_touch_io),
        TAG,
        "GSL3670 panel IO failed"
    );

    const esp_lcd_touch_config_t touch_config = {
        .x_max = NJORUN_D1001_WIDTH,
        .y_max = NJORUN_D1001_HEIGHT,
        .rst_gpio_num = 12,
        .int_gpio_num = GPIO_NUM_NC,
        .levels = {
            .reset = 0,
            .interrupt = 0,
        },
        .flags = {
            .swap_xy = 0,
            .mirror_x = 1,
            .mirror_y = 1,
        },
    };
    ESP_RETURN_ON_ERROR(
        esp_lcd_touch_new_i2c_gsl3670(s_touch_io, &touch_config, &s_touch),
        TAG,
        "GSL3670 init failed"
    );
    s_touch->read_data = safe_touch_read_data;
    s_touch->get_xy = safe_touch_get_xy;
    ESP_LOGI(TAG, "D1001 touch ready: GSL3670 on I2C0");
    return ESP_OK;
}

esp_err_t njorun_d1001_touch_read(njorun_d1001_touch_point_t *point)
{
    if (point == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (s_touch == NULL) {
        return ESP_ERR_INVALID_STATE;
    }

    ESP_RETURN_ON_ERROR(esp_lcd_touch_read_data(s_touch), TAG, "GSL3670 read failed");
    esp_lcd_touch_point_data_t data = {0};
    uint8_t point_count = 0;
    ESP_RETURN_ON_ERROR(
        esp_lcd_touch_get_data(s_touch, &data, &point_count, 1),
        TAG,
        "GSL3670 coordinate conversion failed"
    );
    point->pressed = point_count > 0;
    point->x = point->pressed
        ? (data.x < NJORUN_D1001_WIDTH ? data.x : NJORUN_D1001_WIDTH - 1)
        : 0;
    point->y = point->pressed
        ? (data.y < NJORUN_D1001_HEIGHT ? data.y : NJORUN_D1001_HEIGHT - 1)
        : 0;
    return ESP_OK;
}

static void wifi_event_handler(
    void *argument,
    esp_event_base_t event_base,
    int32_t event_id,
    void *event_data
)
{
    (void)argument;
    (void)event_data;
    if (s_wifi_events == NULL) {
        return;
    }
    if (event_base == IP_EVENT && event_id == IP_EVENT_STA_GOT_IP) {
        xEventGroupClearBits(s_wifi_events, WIFI_FAILED_BIT);
        xEventGroupSetBits(s_wifi_events, WIFI_CONNECTED_BIT);
    } else if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_STA_DISCONNECTED) {
        xEventGroupClearBits(s_wifi_events, WIFI_CONNECTED_BIT);
        xEventGroupSetBits(s_wifi_events, WIFI_FAILED_BIT);
    }
}

static void start_time_sync(void)
{
    if (s_sntp_initialized) {
        return;
    }
    esp_sntp_config_t config = ESP_NETIF_SNTP_DEFAULT_CONFIG("pool.ntp.org");
    config.server_from_dhcp = true;
    const esp_err_t result = esp_netif_sntp_init(&config);
    if (result == ESP_OK || result == ESP_ERR_INVALID_STATE) {
        s_sntp_initialized = true;
    } else {
        ESP_LOGW(TAG, "NTP initialization failed: %s", esp_err_to_name(result));
    }
}

static esp_err_t require_valid_clock_for_tls(const char *url)
{
    if (url == NULL || (strncmp(url, "https://", 8) != 0 && strncmp(url, "wss://", 6) != 0)) {
        return ESP_OK;
    }
    start_time_sync();
    const time_t minimum_valid_time = (time_t)1704067200; /* 2024-01-01 UTC */
    for (unsigned attempt = 0; attempt < 10 && time(NULL) < minimum_valid_time; ++attempt) {
        esp_netif_sntp_sync_wait(pdMS_TO_TICKS(1000));
    }
    return time(NULL) >= minimum_valid_time ? ESP_OK : ESP_ERR_INVALID_STATE;
}

esp_err_t njorun_d1001_wifi_init(void)
{
    if (s_wifi_initialized) {
        return ESP_OK;
    }
    esp_err_t result = nvs_flash_init();
    if (result == ESP_ERR_NVS_NO_FREE_PAGES || result == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_RETURN_ON_ERROR(nvs_flash_erase(), TAG, "incompatible NVS erase failed");
        result = nvs_flash_init();
    }
    ESP_RETURN_ON_ERROR(result, TAG, "NVS init failed");
    ESP_RETURN_ON_ERROR(esp_netif_init(), TAG, "network interface init failed");
    result = esp_event_loop_create_default();
    if (result != ESP_OK && result != ESP_ERR_INVALID_STATE) {
        return result;
    }
    if (esp_netif_create_default_wifi_sta() == NULL) {
        return ESP_ERR_NO_MEM;
    }
    const wifi_init_config_t wifi_config = WIFI_INIT_CONFIG_DEFAULT();
    ESP_RETURN_ON_ERROR(esp_wifi_init(&wifi_config), TAG, "remote Wi-Fi init failed");
    s_wifi_events = xEventGroupCreate();
    if (s_wifi_events == NULL) {
        return ESP_ERR_NO_MEM;
    }
    ESP_RETURN_ON_ERROR(
        esp_event_handler_register(
            WIFI_EVENT,
            WIFI_EVENT_STA_DISCONNECTED,
            wifi_event_handler,
            NULL
        ),
        TAG,
        "Wi-Fi disconnect handler failed"
    );
    ESP_RETURN_ON_ERROR(
        esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event_handler, NULL),
        TAG,
        "Wi-Fi IP handler failed"
    );
    ESP_RETURN_ON_ERROR(esp_wifi_set_storage(WIFI_STORAGE_RAM), TAG, "Wi-Fi storage failed");
    ESP_RETURN_ON_ERROR(esp_wifi_set_mode(WIFI_MODE_STA), TAG, "station mode failed");
    ESP_RETURN_ON_ERROR(esp_wifi_start(), TAG, "remote Wi-Fi start failed");
    s_wifi_initialized = true;
    return ESP_OK;
}

esp_err_t njorun_d1001_wifi_scan(
    njorun_d1001_wifi_network_t *networks,
    size_t capacity,
    size_t *network_count
)
{
    if (networks == NULL || network_count == NULL || capacity == 0) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(njorun_d1001_wifi_init(), TAG, "Wi-Fi unavailable");

    const wifi_scan_config_t scan_config = {
        .show_hidden = false,
        .scan_type = WIFI_SCAN_TYPE_ACTIVE,
    };
    ESP_RETURN_ON_ERROR(esp_wifi_scan_start(&scan_config, true), TAG, "Wi-Fi scan failed");

    uint16_t requested = (uint16_t)(capacity < NJORUN_D1001_MAX_WIFI_NETWORKS
        ? capacity
        : NJORUN_D1001_MAX_WIFI_NETWORKS);
    wifi_ap_record_t records[NJORUN_D1001_MAX_WIFI_NETWORKS] = {0};
    ESP_RETURN_ON_ERROR(
        esp_wifi_scan_get_ap_records(&requested, records),
        TAG,
        "Wi-Fi scan result failed"
    );
    for (uint16_t index = 0; index < requested; ++index) {
        memcpy(networks[index].ssid, records[index].ssid, sizeof(records[index].ssid));
        networks[index].ssid[sizeof(records[index].ssid)] = '\0';
        networks[index].rssi = records[index].rssi;
        networks[index].password_required = records[index].authmode != WIFI_AUTH_OPEN
            && records[index].authmode != WIFI_AUTH_OWE;
    }
    *network_count = requested;
    return ESP_OK;
}

esp_err_t njorun_d1001_wifi_connect(const char *ssid, const char *password)
{
    if (ssid == NULL || password == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    const size_t ssid_length = strnlen(ssid, sizeof(((wifi_sta_config_t *)0)->ssid));
    const size_t password_length = strnlen(password, sizeof(((wifi_sta_config_t *)0)->password));
    if (ssid_length == 0 || ssid_length >= sizeof(((wifi_sta_config_t *)0)->ssid)
        || password_length >= sizeof(((wifi_sta_config_t *)0)->password)) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(njorun_d1001_wifi_init(), TAG, "Wi-Fi unavailable");

    wifi_config_t config = {0};
    memcpy(config.sta.ssid, ssid, ssid_length);
    memcpy(config.sta.password, password, password_length);
    config.sta.threshold.authmode = password_length == 0 ? WIFI_AUTH_OPEN : WIFI_AUTH_WPA2_PSK;
    config.sta.scan_method = WIFI_ALL_CHANNEL_SCAN;
    config.sta.sort_method = WIFI_CONNECT_AP_BY_SECURITY;
    config.sta.pmf_cfg.capable = true;
    config.sta.pmf_cfg.required = false;
    config.sta.sae_pwe_h2e = WPA3_SAE_PWE_BOTH;
    config.sta.sae_pk_mode = WPA3_SAE_PK_MODE_AUTOMATIC;
    esp_err_t result = esp_wifi_disconnect();
    if (result != ESP_OK && result != ESP_ERR_WIFI_NOT_CONNECT) {
        return result;
    }
    if (result == ESP_OK) {
        vTaskDelay(pdMS_TO_TICKS(50));
    }
    xEventGroupClearBits(s_wifi_events, WIFI_CONNECTED_BIT | WIFI_FAILED_BIT);
    ESP_RETURN_ON_ERROR(esp_wifi_set_config(WIFI_IF_STA, &config), TAG, "Wi-Fi config failed");
    ESP_RETURN_ON_ERROR(esp_wifi_connect(), TAG, "Wi-Fi connection start failed");
    const EventBits_t bits = xEventGroupWaitBits(
        s_wifi_events,
        WIFI_CONNECTED_BIT | WIFI_FAILED_BIT,
        pdFALSE,
        pdFALSE,
        pdMS_TO_TICKS(20000)
    );
    if ((bits & WIFI_CONNECTED_BIT) != 0) {
        start_time_sync();
        return ESP_OK;
    }
    return (bits & WIFI_FAILED_BIT) != 0 ? ESP_ERR_WIFI_CONN : ESP_ERR_TIMEOUT;
}

static const mdns_txt_item_t *mdns_find_txt(const mdns_result_t *result, const char *key)
{
    if (result == NULL || key == NULL) {
        return NULL;
    }
    for (size_t index = 0; index < result->txt_count; ++index) {
        if (result->txt[index].key != NULL && strcmp(result->txt[index].key, key) == 0) {
            return &result->txt[index];
        }
    }
    return NULL;
}

static bool copy_mdns_txt(
    const mdns_result_t *result,
    const char *key,
    char *destination,
    size_t capacity
)
{
    const mdns_txt_item_t *item = mdns_find_txt(result, key);
    if (item == NULL || item->value == NULL || capacity == 0) {
        return false;
    }
    const size_t index = (size_t)(item - result->txt);
    const size_t length = result->txt_value_len != NULL
        ? result->txt_value_len[index]
        : strlen(item->value);
    if (length == 0 || length >= capacity) {
        return false;
    }
    memcpy(destination, item->value, length);
    destination[length] = '\0';
    return true;
}

static bool valid_ha_endpoint(const char *endpoint)
{
    return endpoint != NULL
        && (strncmp(endpoint, "http://", 7) == 0 || strncmp(endpoint, "https://", 8) == 0);
}

static bool endpoint_already_added(
    const njorun_d1001_ha_instance_t *instances,
    size_t count,
    const char *endpoint
)
{
    for (size_t index = 0; index < count; ++index) {
        if (strcmp(instances[index].endpoint, endpoint) == 0) {
            return true;
        }
    }
    return false;
}

esp_err_t njorun_d1001_ha_discover(
    njorun_d1001_ha_instance_t *instances,
    size_t capacity,
    size_t *instance_count
)
{
    if (instances == NULL || instance_count == NULL || capacity == 0) {
        return ESP_ERR_INVALID_ARG;
    }
    *instance_count = 0;
    ESP_RETURN_ON_ERROR(njorun_d1001_wifi_init(), TAG, "Wi-Fi unavailable");
    ESP_RETURN_ON_ERROR(mdns_init(), TAG, "mDNS init failed");

    mdns_result_t *results = NULL;
    const size_t bounded_capacity = capacity < NJORUN_D1001_MAX_HA_INSTANCES
        ? capacity
        : NJORUN_D1001_MAX_HA_INSTANCES;
    esp_err_t result = mdns_query_ptr(
        "_home-assistant",
        "_tcp",
        3000,
        bounded_capacity,
        &results
    );
    if (result != ESP_OK && result != ESP_ERR_NOT_FOUND) {
        mdns_free();
        return result;
    }

    for (mdns_result_t *current = results;
         current != NULL && *instance_count < bounded_capacity;
         current = current->next) {
        char endpoint[NJORUN_D1001_HA_ENDPOINT_CAPACITY] = {0};
        const char *endpoint_keys[] = {"internal_url", "base_url", "external_url"};
        for (size_t key = 0; key < sizeof(endpoint_keys) / sizeof(endpoint_keys[0]); ++key) {
            if (copy_mdns_txt(current, endpoint_keys[key], endpoint, sizeof(endpoint))
                && valid_ha_endpoint(endpoint)) {
                break;
            }
            endpoint[0] = '\0';
        }
        if (endpoint[0] == '\0') {
            for (mdns_ip_addr_t *address = current->addr; address != NULL; address = address->next) {
                if (address->addr.type == ESP_IPADDR_TYPE_V4) {
                    snprintf(
                        endpoint,
                        sizeof(endpoint),
                        "http://" IPSTR ":%u",
                        IP2STR(&address->addr.u_addr.ip4),
                        current->port
                    );
                    break;
                }
            }
        }
        if (!valid_ha_endpoint(endpoint)
            || endpoint_already_added(instances, *instance_count, endpoint)) {
            continue;
        }

        njorun_d1001_ha_instance_t *instance = &instances[*instance_count];
        const bool has_location = copy_mdns_txt(
            current,
            "location_name",
            instance->name,
            sizeof(instance->name)
        );
        if (!has_location && current->instance_name != NULL) {
            strlcpy(instance->name, current->instance_name, sizeof(instance->name));
        }
        if (instance->name[0] == '\0') {
            strlcpy(instance->name, "Home Assistant", sizeof(instance->name));
        }
        strlcpy(instance->endpoint, endpoint, sizeof(instance->endpoint));
        ++*instance_count;
    }
    mdns_query_results_free(results);
    mdns_free();
    return ESP_OK;
}

esp_err_t njorun_d1001_random(void *buffer, size_t length)
{
    if (buffer == NULL || length == 0) {
        return ESP_ERR_INVALID_ARG;
    }
    esp_fill_random(buffer, length);
    return ESP_OK;
}

static esp_err_t http_event_handler(esp_http_client_event_t *event)
{
    if (event == NULL || event->user_data == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    http_response_t *response = event->user_data;
    if (event->event_id != HTTP_EVENT_ON_DATA || event->data_len <= 0 || response->overflow) {
        return ESP_OK;
    }
    const size_t data_length = (size_t)event->data_len;
    if (data_length > response->capacity - 1 - response->length) {
        response->overflow = true;
        return ESP_OK;
    }
    memcpy(response->buffer + response->length, event->data, data_length);
    response->length += data_length;
    response->buffer[response->length] = '\0';
    return ESP_OK;
}

esp_err_t njorun_d1001_http_post_form(
    const char *url,
    const char *body,
    char *response,
    size_t response_capacity,
    size_t *response_length
)
{
    if (url == NULL || body == NULL || response == NULL || response_length == NULL
        || response_capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    const size_t body_length = strlen(body);
    if (body_length > INT_MAX) {
        return ESP_ERR_INVALID_SIZE;
    }
    ESP_RETURN_ON_ERROR(require_valid_clock_for_tls(url), TAG, "TLS requires synchronized time");
    http_response_t context = {
        .buffer = response,
        .capacity = response_capacity,
    };
    response[0] = '\0';
    const esp_http_client_config_t config = {
        .url = url,
        .event_handler = http_event_handler,
        .user_data = &context,
        .timeout_ms = 15000,
        .disable_auto_redirect = true,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (client == NULL) {
        return ESP_ERR_NO_MEM;
    }
    esp_err_t result = esp_http_client_set_method(client, HTTP_METHOD_POST);
    if (result == ESP_OK) {
        result = esp_http_client_set_header(
            client,
            "Content-Type",
            "application/x-www-form-urlencoded"
        );
    }
    if (result == ESP_OK) {
        result = esp_http_client_set_post_field(client, body, (int)body_length);
    }
    if (result == ESP_OK) {
        result = esp_http_client_perform(client);
    }
    if (result == ESP_OK && esp_http_client_get_status_code(client) != 200) {
        result = ESP_ERR_INVALID_RESPONSE;
    }
    if (result == ESP_OK && context.overflow) {
        result = ESP_ERR_NO_MEM;
    }
    *response_length = context.length;
    esp_http_client_cleanup(client);
    return result;
}

static void websocket_event_handler(
    void *argument,
    esp_event_base_t event_base,
    int32_t event_id,
    void *event_data
)
{
    (void)event_base;
    websocket_response_t *response = argument;
    if (response == NULL || response->events == NULL) {
        return;
    }
    if (event_id == WEBSOCKET_EVENT_CONNECTED) {
        xEventGroupSetBits(response->events, WEBSOCKET_CONNECTED_BIT);
        return;
    }
    if (event_id == WEBSOCKET_EVENT_ERROR || event_id == WEBSOCKET_EVENT_DISCONNECTED) {
        xEventGroupSetBits(response->events, WEBSOCKET_FAILED_BIT);
        return;
    }
    if (event_id != WEBSOCKET_EVENT_DATA || event_data == NULL) {
        return;
    }
    const esp_websocket_event_data_t *data = event_data;
    if (data->op_code != 0x0 && data->op_code != 0x1) {
        return;
    }
    if (data->payload_offset == 0) {
        response->length = 0;
        response->overflow = false;
    }
    if (data->payload_offset < 0 || (size_t)data->payload_offset != response->length
        || data->data_len < 0) {
        response->overflow = true;
    } else if (!response->overflow) {
        const size_t data_length = (size_t)data->data_len;
        if (data_length > response->capacity - 1 - response->length) {
            response->overflow = true;
        } else {
            memcpy(response->buffer + response->length, data->data_ptr, data_length);
            response->length += data_length;
            response->buffer[response->length] = '\0';
        }
    }
    if (data->fin && data->payload_len >= 0
        && (size_t)data->payload_len == response->length) {
        xEventGroupSetBits(response->events, WEBSOCKET_DATA_BIT);
    } else if (data->fin && response->overflow) {
        xEventGroupSetBits(response->events, WEBSOCKET_FAILED_BIT);
    }
}

static esp_err_t websocket_wait(websocket_response_t *response, EventBits_t bits)
{
    const EventBits_t received = xEventGroupWaitBits(
        response->events,
        bits | WEBSOCKET_FAILED_BIT,
        pdTRUE,
        pdFALSE,
        pdMS_TO_TICKS(15000)
    );
    if ((received & WEBSOCKET_FAILED_BIT) != 0) {
        return ESP_ERR_INVALID_RESPONSE;
    }
    return (received & bits) != 0 ? ESP_OK : ESP_ERR_TIMEOUT;
}

static esp_err_t websocket_send(
    esp_websocket_client_handle_t client,
    websocket_response_t *response,
    const char *message
)
{
    xEventGroupClearBits(response->events, WEBSOCKET_DATA_BIT | WEBSOCKET_FAILED_BIT);
    response->length = 0;
    response->overflow = false;
    response->buffer[0] = '\0';
    const int length = (int)strlen(message);
    const int sent = esp_websocket_client_send_text(
        client,
        message,
        length,
        pdMS_TO_TICKS(15000)
    );
    if (sent != length) {
        return ESP_FAIL;
    }
    return websocket_wait(response, WEBSOCKET_DATA_BIT);
}

esp_err_t njorun_d1001_ha_fetch_areas(
    const char *websocket_url,
    const char *auth_message,
    char *response,
    size_t response_capacity,
    size_t *response_length
)
{
    if (websocket_url == NULL || auth_message == NULL || response == NULL
        || response_length == NULL || response_capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(
        require_valid_clock_for_tls(websocket_url),
        TAG,
        "TLS requires synchronized time"
    );
    websocket_response_t context = {
        .events = xEventGroupCreate(),
        .buffer = response,
        .capacity = response_capacity,
    };
    if (context.events == NULL) {
        return ESP_ERR_NO_MEM;
    }
    response[0] = '\0';
    const esp_websocket_client_config_t config = {
        .uri = websocket_url,
        .disable_auto_reconnect = true,
        .user_context = &context,
        .buffer_size = 4096,
        .network_timeout_ms = 15000,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_websocket_client_handle_t client = esp_websocket_client_init(&config);
    if (client == NULL) {
        vEventGroupDelete(context.events);
        return ESP_ERR_NO_MEM;
    }
    bool started = false;
    esp_err_t result = esp_websocket_register_events(
        client,
        WEBSOCKET_EVENT_ANY,
        websocket_event_handler,
        &context
    );
    if (result == ESP_OK) {
        result = esp_websocket_client_start(client);
        started = result == ESP_OK;
    }
    if (result == ESP_OK) {
        result = websocket_wait(&context, WEBSOCKET_CONNECTED_BIT);
    }
    if (result == ESP_OK) {
        result = websocket_wait(&context, WEBSOCKET_DATA_BIT);
    }
    if (result == ESP_OK && strstr(response, "\"auth_required\"") == NULL) {
        result = ESP_ERR_INVALID_RESPONSE;
    }
    if (result == ESP_OK) {
        result = websocket_send(client, &context, auth_message);
    }
    if (result == ESP_OK && strstr(response, "\"auth_ok\"") == NULL) {
        result = ESP_ERR_INVALID_RESPONSE;
    }
    if (result == ESP_OK) {
        result = websocket_send(client, &context, "{\"id\":1,\"type\":\"config/area_registry/list\"}");
    }
    if (result == ESP_OK && context.overflow) {
        result = ESP_ERR_NO_MEM;
    }
    *response_length = context.length;
    if (started) {
        esp_websocket_client_stop(client);
    }
    esp_websocket_client_destroy(client);
    vEventGroupDelete(context.events);
    return result;
}

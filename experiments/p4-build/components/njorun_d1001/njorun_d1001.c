// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#include "njorun_d1001.h"

#include <stdbool.h>
#include <string.h>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "esp_cache.h"
#include "esp_check.h"
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
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
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

esp_err_t njorun_d1001_wifi_init(void)
{
    if (s_wifi_initialized) {
        return ESP_OK;
    }
    ESP_RETURN_ON_ERROR(nvs_flash_init(), TAG, "NVS init failed");
    ESP_RETURN_ON_ERROR(esp_netif_init(), TAG, "network interface init failed");
    esp_err_t result = esp_event_loop_create_default();
    if (result != ESP_OK && result != ESP_ERR_INVALID_STATE) {
        return result;
    }
    if (esp_netif_create_default_wifi_sta() == NULL) {
        return ESP_ERR_NO_MEM;
    }
    const wifi_init_config_t wifi_config = WIFI_INIT_CONFIG_DEFAULT();
    ESP_RETURN_ON_ERROR(esp_wifi_init(&wifi_config), TAG, "remote Wi-Fi init failed");
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
    config.sta.threshold.authmode = WIFI_AUTH_OPEN;
    ESP_RETURN_ON_ERROR(esp_wifi_set_config(WIFI_IF_STA, &config), TAG, "Wi-Fi config failed");
    return esp_wifi_connect();
}

// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#include "njorun_d1001.h"

#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stdbool.h>
#include <stdio.h>
#include "esp_heap_caps.h"
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "arpa/inet.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "driver/usb_serial_jtag.h"
#include <sys/time.h>
#include "esp_attr.h"
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
#include "esp_timer.h"
#include "esp_video_device.h"
#include "esp_video_init.h"
#include "esp_video_ioctl.h"
#include "lwip/netdb.h"
#include "lwip/sockets.h"
#include <sys/mman.h>
#include <unistd.h>
#include "esp_ota_ops.h"
#include "esp_partition.h"
#include "esp_system.h"
#include "esp_websocket_client.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include <errno.h>
#include <stdarg.h>

#include "esp_tls_errors.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
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
    EXP_CAMERA_POWER = 1U << 1,
    EXP_LCD_RESET = 1U << 2,
    EXP_CAMERA_PWDN = 1U << 3,
    EXP_LCD_BACKLIGHT_ENABLE = 1U << 7,
    EXP_POWER_HOLD = 1U << 8,
    EXP_CAMERA_RESET = 1U << 9,
    EXP_TOUCH_RESET = 1U << 12,
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
static esp_netif_t *s_wifi_netif;
static esp_netif_t *s_eth_netif; /* NULL unless CONFIG_NJORUN_ETHERNET is built and started */
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

static uint16_t touch_map_axis(
    uint16_t raw,
    uint16_t raw_min,
    uint16_t raw_max,
    uint16_t extent,
    bool mirror
)
{
    const uint16_t bounded = raw < raw_min ? raw_min : (raw > raw_max ? raw_max : raw);
    uint32_t mapped =
        ((uint32_t)(bounded - raw_min) * (uint32_t)(extent - 1U)) / (raw_max - raw_min);
    if (mirror) {
        mapped = (uint32_t)(extent - 1U) - mapped;
    }
    return (uint16_t)mapped;
}

static esp_err_t safe_touch_read_data(esp_lcd_touch_handle_t touch)
{
    if (touch == NULL || touch->io == NULL) {
        return ESP_ERR_INVALID_ARG;
    }

    /* Read one coherent controller frame. AROS-NX verified this 44-byte
     * format, its raw bounds and the auxiliary-record marker on this exact
     * D1001. Seeed's driver requests the same 44 bytes into a 24-byte array. */
    uint8_t data[44] = {0};
    ESP_RETURN_ON_ERROR(
        esp_lcd_panel_io_rx_param(touch->io, 0x80, data, sizeof(data)),
        TAG,
        "GSL3670 coordinate read failed"
    );

    const uint8_t raw_count = data[0] <= 10U ? data[0] : 0U;
    bool found = false;
    uint32_t best_distance = UINT_MAX;
    uint16_t best_x = 0;
    uint16_t best_y = 0;
    for (uint8_t index = 0; index < raw_count; ++index) {
        const size_t offset = (size_t)(index + 1) * 4U;
        if ((data[offset + 1U] >> 4) != 0U) {
            continue;
        }
        const uint16_t raw_y =
            (uint16_t)data[offset] | ((uint16_t)(data[offset + 1U] & 0x0fU) << 8);
        const uint16_t raw_x = (uint16_t)data[offset + 2U]
            | ((uint16_t)(data[offset + 3U] & 0x0fU) << 8);

        /* AROS-NX measured the D1001 in its clockwise landscape view as
         * direct raw X and mirrored raw Y. Njörun draws in the panel's native
         * portrait orientation, so the inverse rotation swaps the axes and
         * mirrors both resulting axes. */
        const uint16_t x = touch_map_axis(raw_y, 15U, 874U, NJORUN_D1001_WIDTH, true);
        const uint16_t y = touch_map_axis(raw_x, 16U, 1638U, NJORUN_D1001_HEIGHT, true);
        const int32_t dx = (int32_t)x - (int32_t)s_touch_x;
        const int32_t dy = (int32_t)y - (int32_t)s_touch_y;
        const uint32_t distance = (uint32_t)(dx * dx + dy * dy);
        if (!found || !s_touch_pressed || distance < best_distance) {
            found = true;
            best_distance = distance;
            best_x = x;
            best_y = y;
        }
    }

    s_touch_pressed = found;
    if (found) {
        s_touch_x = best_x;
        s_touch_y = best_y;
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

    /* The PCA9535 latches power up high on the reference firmware. Because
     * Njörun deliberately programs safe low latches first, release the
     * GSL3670's active-low reset explicitly before touch initialization. */
    s_expander_outputs |= EXP_TOUCH_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "touch reset release failed");
    vTaskDelay(pdMS_TO_TICKS(50));
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
    /* AROS-NX proved this controller at 10 kHz. ESP-IDF's fixed divider is
     * reliable at 100 kHz while leaving ample time for a 44-byte report. */
    io_config.scl_speed_hz = 100000;
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
            .mirror_x = 0,
            .mirror_y = 0,
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

static bool i2c0_address_present(uint8_t address)
{
    return s_touch_i2c_bus != NULL
        && i2c_master_probe(s_touch_i2c_bus, address, 100) == ESP_OK;
}

static esp_err_t camera_power_on(void)
{
    s_expander_outputs |= EXP_CAMERA_POWER;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "camera rail enable failed");
    vTaskDelay(pdMS_TO_TICKS(50));

    /* Match Seeed's D1001 sequence: PWDN high, then a high-low-high reset pulse. */
    s_expander_outputs |= EXP_CAMERA_PWDN | EXP_CAMERA_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "camera enable failed");
    vTaskDelay(pdMS_TO_TICKS(10));
    s_expander_outputs &= (uint16_t)~EXP_CAMERA_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "camera reset low failed");
    vTaskDelay(pdMS_TO_TICKS(10));
    s_expander_outputs |= EXP_CAMERA_RESET;
    ESP_RETURN_ON_ERROR(expander_commit(), TAG, "camera reset release failed");
    vTaskDelay(pdMS_TO_TICKS(50));
    return ESP_OK;
}

static esp_err_t camera_power_off(void)
{
    s_expander_outputs &= (uint16_t)~(
        EXP_CAMERA_RESET | EXP_CAMERA_PWDN | EXP_CAMERA_POWER
    );
    return expander_commit();
}

enum {
    CAMERA_MONITOR_BUFFER_COUNT = 2,
    CAMERA_MONITOR_GRID_WIDTH = 80,
    CAMERA_MONITOR_GRID_HEIGHT = 45,
    CAMERA_MONITOR_GRID_SIZE = CAMERA_MONITOR_GRID_WIDTH * CAMERA_MONITOR_GRID_HEIGHT,
    CAMERA_MONITOR_SETTLE_FRAMES = 30,
    CAMERA_MONITOR_SAMPLE_FRAMES = 15,
    CAMERA_MONITOR_STABLE_SAMPLES = 3,
};

typedef struct {
    int fd;
    void *mappings[CAMERA_MONITOR_BUFFER_COUNT];
    size_t mapping_lengths[CAMERA_MONITOR_BUFFER_COUNT];
    uint32_t width;
    uint32_t height;
    uint32_t frame_number;
    uint8_t previous_mean;
    uint8_t sensitivity;
    bool powered;
    bool video_initialized;
    bool streaming;
    bool previous_valid;
    bool previous_candidate;
    bool motion_ready;
    uint8_t stable_samples;
} camera_monitor_t;

static camera_monitor_t s_camera_monitor = {
    .fd = -1,
    .mappings = {MAP_FAILED, MAP_FAILED},
};

/* Motion history is cold data and does not need scarce internal SRAM. */
EXT_RAM_BSS_ATTR static uint8_t s_camera_previous[CAMERA_MONITOR_GRID_SIZE];
EXT_RAM_BSS_ATTR static uint8_t s_camera_current[CAMERA_MONITOR_GRID_SIZE];

static esp_err_t camera_monitor_cleanup(void)
{
    esp_err_t result = ESP_OK;
    const int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    if (s_camera_monitor.streaming) {
        if (ioctl(s_camera_monitor.fd, VIDIOC_STREAMOFF, &type) != 0) {
            ESP_LOGW(TAG, "camera monitor stream stop failed: errno %d", errno);
            result = ESP_FAIL;
        }
        s_camera_monitor.streaming = false;
    }
    for (size_t index = 0; index < CAMERA_MONITOR_BUFFER_COUNT; index++) {
        if (s_camera_monitor.mappings[index] != MAP_FAILED) {
            munmap(s_camera_monitor.mappings[index], s_camera_monitor.mapping_lengths[index]);
            s_camera_monitor.mappings[index] = MAP_FAILED;
            s_camera_monitor.mapping_lengths[index] = 0;
        }
    }
    if (s_camera_monitor.fd >= 0) {
        close(s_camera_monitor.fd);
        s_camera_monitor.fd = -1;
    }
    if (s_camera_monitor.video_initialized) {
        const esp_err_t deinit = esp_video_deinit_with_flags(ESP_VIDEO_INIT_FLAGS_MIPI_CSI);
        if (result == ESP_OK && deinit != ESP_OK) {
            result = deinit;
        }
        s_camera_monitor.video_initialized = false;
    }
    if (s_camera_monitor.powered) {
        const esp_err_t power_off = camera_power_off();
        if (result == ESP_OK && power_off != ESP_OK) {
            result = power_off;
        }
        s_camera_monitor.powered = false;
    }
    s_camera_monitor.previous_valid = false;
    s_camera_monitor.previous_candidate = false;
    s_camera_monitor.motion_ready = false;
    s_camera_monitor.stable_samples = 0;
    return result;
}

static esp_err_t camera_set_fixed_gain(int fd, uint32_t target_gain_x1000)
{
    struct v4l2_query_ext_ctrl query = {.id = V4L2_CID_GAIN};
    if (ioctl(fd, VIDIOC_QUERY_EXT_CTRL, &query) != 0) {
        return ESP_FAIL;
    }

    uint32_t best_index = (uint32_t)query.minimum;
    uint32_t best_gain = 0;
    uint32_t best_distance = UINT32_MAX;
    for (uint32_t index = (uint32_t)query.minimum; index <= (uint32_t)query.maximum; index++) {
        struct v4l2_querymenu menu = {.id = V4L2_CID_GAIN, .index = index};
        if (ioctl(fd, VIDIOC_QUERYMENU, &menu) != 0) {
            return ESP_FAIL;
        }
        const uint32_t gain = (uint32_t)menu.value;
        const uint32_t distance = gain > target_gain_x1000
            ? gain - target_gain_x1000
            : target_gain_x1000 - gain;
        if (distance < best_distance) {
            best_distance = distance;
            best_index = index;
            best_gain = gain;
        }
        if (distance == 0) {
            break;
        }
    }

    struct v4l2_ext_control control = {
        .id = V4L2_CID_GAIN,
        .value = (int32_t)best_index,
    };
    struct v4l2_ext_controls controls = {
        .ctrl_class = V4L2_CID_USER_CLASS,
        .count = 1,
        .controls = &control,
    };
    if (ioctl(fd, VIDIOC_S_EXT_CTRLS, &controls) != 0) {
        return ESP_FAIL;
    }
    ESP_LOGI(TAG, "camera fixed gain: %" PRIu32 "/1000 (index %" PRIu32 ")", best_gain, best_index);
    return ESP_OK;
}

esp_err_t njorun_d1001_camera_monitor_start(uint8_t sensitivity)
{
    if (sensitivity < 1 || sensitivity > 100) {
        return ESP_ERR_INVALID_ARG;
    }
    if (s_camera_monitor.streaming) {
        return ESP_ERR_INVALID_STATE;
    }
    if (s_touch_i2c_bus == NULL || s_expander == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    memset(&s_camera_monitor, 0, sizeof(s_camera_monitor));
    s_camera_monitor.fd = -1;
    s_camera_monitor.sensitivity = sensitivity;
    for (size_t index = 0; index < CAMERA_MONITOR_BUFFER_COUNT; index++) {
        s_camera_monitor.mappings[index] = MAP_FAILED;
    }

    s_camera_monitor.powered = true;
    esp_err_t result = camera_power_on();
    if (result != ESP_OK) {
        goto fail;
    }

    const esp_video_init_csi_config_t csi_config = {
        .sccb_config = {
            .init_sccb = false,
            .i2c_handle = s_touch_i2c_bus,
            .freq = 100000,
        },
        .reset_pin = GPIO_NUM_NC,
        .pwdn_pin = GPIO_NUM_NC,
        .dont_init_ldo = false,
    };
    const esp_video_init_config_t video_config = {.csi = &csi_config};
    result = esp_video_init_with_flags(&video_config, ESP_VIDEO_INIT_FLAGS_MIPI_CSI);
    if (result != ESP_OK) {
        goto fail;
    }
    s_camera_monitor.video_initialized = true;

    s_camera_monitor.fd = open(ESP_VIDEO_MIPI_CSI_DEVICE_NAME, O_RDONLY);
    if (s_camera_monitor.fd < 0) {
        result = ESP_FAIL;
        goto fail;
    }

    struct v4l2_format format = {
        .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
        .fmt.pix = {
            .width = 1280,
            .height = 720,
            .pixelformat = V4L2_PIX_FMT_SBGGR8,
        },
    };
    if (ioctl(s_camera_monitor.fd, VIDIOC_S_FMT, &format) != 0) {
        result = ESP_FAIL;
        goto fail;
    }
    s_camera_monitor.width = format.fmt.pix.width;
    s_camera_monitor.height = format.fmt.pix.height;
    result = camera_set_fixed_gain(s_camera_monitor.fd, 16000);
    if (result != ESP_OK) {
        ESP_LOGE(TAG, "camera fixed gain configuration failed: errno %d", errno);
        goto fail;
    }

    struct timeval timeout = {.tv_sec = 2, .tv_usec = 0};
    if (ioctl(s_camera_monitor.fd, VIDIOC_S_DQBUF_TIMEOUT, &timeout) != 0) {
        ESP_LOGW(TAG, "camera monitor dequeue timeout could not be set: errno %d", errno);
    }

    struct v4l2_requestbuffers request = {
        .count = CAMERA_MONITOR_BUFFER_COUNT,
        .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
        .memory = V4L2_MEMORY_MMAP,
    };
    if (ioctl(s_camera_monitor.fd, VIDIOC_REQBUFS, &request) != 0
        || request.count < CAMERA_MONITOR_BUFFER_COUNT) {
        result = ESP_ERR_NO_MEM;
        goto fail;
    }
    for (uint32_t index = 0; index < CAMERA_MONITOR_BUFFER_COUNT; index++) {
        struct v4l2_buffer buffer = {
            .index = index,
            .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
            .memory = V4L2_MEMORY_MMAP,
        };
        if (ioctl(s_camera_monitor.fd, VIDIOC_QUERYBUF, &buffer) != 0) {
            result = ESP_FAIL;
            goto fail;
        }
        s_camera_monitor.mapping_lengths[index] = buffer.length;
        s_camera_monitor.mappings[index] = mmap(
            NULL,
            buffer.length,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            s_camera_monitor.fd,
            buffer.m.offset
        );
        if (s_camera_monitor.mappings[index] == MAP_FAILED
            || ioctl(s_camera_monitor.fd, VIDIOC_QBUF, &buffer) != 0) {
            result = ESP_FAIL;
            goto fail;
        }
    }

    const int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    if (ioctl(s_camera_monitor.fd, VIDIOC_STREAMON, &type) != 0) {
        result = ESP_FAIL;
        goto fail;
    }
    s_camera_monitor.streaming = true;
    ESP_LOGI(
        TAG,
        "camera motion monitor started: %" PRIu32 "x%" PRIu32 " RAW8, sensitivity %u",
        s_camera_monitor.width,
        s_camera_monitor.height,
        s_camera_monitor.sensitivity
    );
    return ESP_OK;

fail:
    camera_monitor_cleanup();
    return result;
}

static uint16_t camera_sensitivity_threshold(
    uint16_t least_sensitive,
    uint16_t most_sensitive
)
{
    const uint32_t step = s_camera_monitor.sensitivity - 1U;
    return (uint16_t)(
        least_sensitive - (((least_sensitive - most_sensitive) * step + 49U) / 99U)
    );
}

static void camera_monitor_analyze(
    const uint8_t *frame,
    size_t frame_bytes,
    njorun_d1001_camera_motion_sample_t *sample
)
{
    const uint32_t width = s_camera_monitor.width;
    const uint32_t height = s_camera_monitor.height;
    if (frame == NULL || width == 0 || height == 0 || frame_bytes < (size_t)width * height) {
        return;
    }

    uint64_t sum = 0;
    uint8_t minimum = UINT8_MAX;
    uint8_t maximum = 0;
    size_t grid_index = 0;
    for (uint32_t grid_y = 0; grid_y < CAMERA_MONITOR_GRID_HEIGHT; grid_y++) {
        const uint32_t y = ((2 * grid_y + 1) * height)
            / (2 * CAMERA_MONITOR_GRID_HEIGHT);
        for (uint32_t grid_x = 0; grid_x < CAMERA_MONITOR_GRID_WIDTH; grid_x++) {
            const uint32_t x = ((2 * grid_x + 1) * width)
                / (2 * CAMERA_MONITOR_GRID_WIDTH);
            const uint8_t value = frame[(size_t)y * width + x];
            s_camera_current[grid_index++] = value;
            minimum = value < minimum ? value : minimum;
            maximum = value > maximum ? value : maximum;
            sum += value;
        }
    }
    const uint8_t mean = (uint8_t)(sum / CAMERA_MONITOR_GRID_SIZE);
    sample->valid = true;
    sample->raw_min = minimum;
    sample->raw_mean = mean;
    sample->raw_max = maximum;

    if (s_camera_monitor.previous_valid) {
        const int mean_delta = (int)mean - (int)s_camera_monitor.previous_mean;
        /* At 16x gain the qualified sensor's dark-scene temporal noise averages
         * about three RAW8 levels. Keep the low-light pixel decision above it. */
        const int threshold = mean < 32 ? 8 : mean < 96 ? 5 : 4;
        uint32_t changed = 0;
        uint32_t residual_sum = 0;
        for (size_t index = 0; index < CAMERA_MONITOR_GRID_SIZE; index++) {
            int residual = (int)s_camera_current[index]
                - (int)s_camera_previous[index]
                - mean_delta;
            residual = residual < 0 ? -residual : residual;
            residual_sum += (uint32_t)residual;
            changed += residual >= threshold;
        }
        sample->changed_per_mille = (uint16_t)(
            (changed * 1000U) / CAMERA_MONITOR_GRID_SIZE
        );
        sample->residual_mean_x100 = (uint16_t)(
            (residual_sum * 100U) / CAMERA_MONITOR_GRID_SIZE
        );
        const bool low_light = mean < 32;
        const uint16_t broad_changed_threshold = low_light
            ? camera_sensitivity_threshold(280U, 90U)
            : camera_sensitivity_threshold(180U, 50U);
        const uint16_t broad_residual_threshold = low_light
            ? camera_sensitivity_threshold(900U, 375U)
            : camera_sensitivity_threshold(350U, 150U);
        const uint16_t moderate_changed_threshold = low_light
            ? camera_sensitivity_threshold(180U, 60U)
            : camera_sensitivity_threshold(100U, 30U);
        const uint16_t moderate_residual_threshold = low_light
            ? camera_sensitivity_threshold(650U, 325U)
            : camera_sensitivity_threshold(250U, 100U);
        const bool broad_change = sample->changed_per_mille >= broad_changed_threshold
            && sample->residual_mean_x100 >= broad_residual_threshold;
        const bool moderate_change = sample->changed_per_mille >= moderate_changed_threshold
            && sample->residual_mean_x100 >= moderate_residual_threshold;
        if (!s_camera_monitor.motion_ready) {
            /*
             * The first frames after STREAMON can differ sharply even with a fixed
             * gain. Do not turn that sensor settling into a presence event. Arm
             * motion only after several quiet comparisons in succession.
             */
            if (!moderate_change) {
                s_camera_monitor.stable_samples++;
                if (s_camera_monitor.stable_samples >= CAMERA_MONITOR_STABLE_SAMPLES) {
                    s_camera_monitor.motion_ready = true;
                    ESP_LOGI(TAG, "camera motion baseline stable; detection ready");
                }
            } else {
                s_camera_monitor.stable_samples = 0;
            }
            sample->motion = false;
            s_camera_monitor.previous_candidate = false;
        } else {
            sample->motion = broad_change
                || (moderate_change && s_camera_monitor.previous_candidate);
            s_camera_monitor.previous_candidate = moderate_change;
        }
    }
    memcpy(s_camera_previous, s_camera_current, CAMERA_MONITOR_GRID_SIZE);
    s_camera_monitor.previous_mean = mean;
    s_camera_monitor.previous_valid = true;
}

esp_err_t njorun_d1001_camera_monitor_next(njorun_d1001_camera_motion_sample_t *sample)
{
    if (sample == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    memset(sample, 0, sizeof(*sample));
    if (!s_camera_monitor.streaming) {
        return ESP_ERR_INVALID_STATE;
    }

    struct v4l2_buffer buffer = {
        .type = V4L2_BUF_TYPE_VIDEO_CAPTURE,
        .memory = V4L2_MEMORY_MMAP,
    };
    if (ioctl(s_camera_monitor.fd, VIDIOC_DQBUF, &buffer) != 0) {
        return ESP_FAIL;
    }
    if (buffer.index >= CAMERA_MONITOR_BUFFER_COUNT
        || s_camera_monitor.mappings[buffer.index] == MAP_FAILED) {
        return ESP_ERR_INVALID_RESPONSE;
    }

    s_camera_monitor.frame_number++;
    if (s_camera_monitor.frame_number >= CAMERA_MONITOR_SETTLE_FRAMES
        && s_camera_monitor.frame_number % CAMERA_MONITOR_SAMPLE_FRAMES == 0) {
        camera_monitor_analyze(
            s_camera_monitor.mappings[buffer.index],
            buffer.bytesused,
            sample
        );
    }
    if (ioctl(s_camera_monitor.fd, VIDIOC_QBUF, &buffer) != 0) {
        return ESP_FAIL;
    }
    if (sample->motion) {
        ESP_LOGI(
            TAG,
            "camera motion: changed=%u.%u%% residual=%u.%02u mean=%u",
            sample->changed_per_mille / 10,
            sample->changed_per_mille % 10,
            sample->residual_mean_x100 / 100,
            sample->residual_mean_x100 % 100,
            sample->raw_mean
        );
    }
    return ESP_OK;
}

esp_err_t njorun_d1001_camera_monitor_stop(void)
{
    const bool was_active = s_camera_monitor.powered;
    const esp_err_t result = camera_monitor_cleanup();
    if (was_active) {
        ESP_LOGI(
            TAG,
            "camera motion monitor stopped; task stack high-water mark %u",
            (unsigned)uxTaskGetStackHighWaterMark(NULL)
        );
    }
    return result;
}

static void raw8_statistics(
    const uint8_t *frame,
    size_t frame_bytes,
    uint32_t width,
    uint32_t height,
    njorun_d1001_camera_probe_t *probe
)
{
    if (frame == NULL || probe == NULL || width == 0 || height == 0) {
        return;
    }
    const size_t expected = (size_t)width * height;
    if (frame_bytes < expected) {
        return;
    }

    uint64_t sum = 0;
    uint32_t samples = 0;
    uint8_t minimum = UINT8_MAX;
    uint8_t maximum = 0;
    /* The native 1280x720 mode becomes an 80x45 grid without another frame buffer. */
    for (uint32_t y = 8; y < height; y += 16) {
        for (uint32_t x = 8; x < width; x += 16) {
            const uint8_t value = frame[(size_t)y * width + x];
            minimum = value < minimum ? value : minimum;
            maximum = value > maximum ? value : maximum;
            sum += value;
            samples++;
        }
    }
    if (samples > 0) {
        probe->raw_min = minimum;
        probe->raw_mean = (uint8_t)(sum / samples);
        probe->raw_max = maximum;
    }
}

static esp_err_t camera_capture_probe(njorun_d1001_camera_probe_t *probe)
{
    enum { CAMERA_BUFFER_COUNT = 2 };
    const int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    int fd = -1;
    void *mappings[CAMERA_BUFFER_COUNT] = {MAP_FAILED, MAP_FAILED};
    size_t mapping_lengths[CAMERA_BUFFER_COUNT] = {0};
    bool streaming = false;
    esp_err_t result = ESP_FAIL;

    const esp_video_init_csi_config_t csi_config = {
        .sccb_config = {
            .init_sccb = false,
            .i2c_handle = s_touch_i2c_bus,
            .freq = 100000,
        },
        .reset_pin = GPIO_NUM_NC,
        .pwdn_pin = GPIO_NUM_NC,
        .dont_init_ldo = false,
    };
    const esp_video_init_config_t video_config = {
        .csi = &csi_config,
    };
    ESP_RETURN_ON_ERROR(
        esp_video_init_with_flags(&video_config, ESP_VIDEO_INIT_FLAGS_MIPI_CSI),
        TAG,
        "camera video initialization failed"
    );

    fd = open(ESP_VIDEO_MIPI_CSI_DEVICE_NAME, O_RDONLY);
    if (fd < 0) {
        ESP_LOGE(TAG, "camera device open failed: errno %d", errno);
        goto cleanup;
    }

    esp_cam_sensor_id_t sensor_id = {0};
    struct v4l2_ext_control control = {
        .id = ESP_CAM_SENSOR_IOC_G_CHIP_ID,
        .size = sizeof(sensor_id),
        .p_u8 = (uint8_t *)&sensor_id,
    };
    struct v4l2_ext_controls controls = {
        .ctrl_class = V4L2_CTRL_CLASS_ESP_CAM_IOCTL,
        .count = 1,
        .controls = &control,
    };
    if (ioctl(fd, VIDIOC_G_EXT_CTRLS, &controls) == 0) {
        probe->sensor_pid = sensor_id.pid;
    }

    struct v4l2_format format = {
        .type = type,
        .fmt.pix = {
            .width = 1280,
            .height = 720,
            .pixelformat = V4L2_PIX_FMT_SBGGR8,
        },
    };
    if (ioctl(fd, VIDIOC_S_FMT, &format) != 0) {
        ESP_LOGE(TAG, "camera RAW8 format failed: errno %d", errno);
        goto cleanup;
    }

    struct timeval timeout = {
        .tv_sec = 2,
        .tv_usec = 0,
    };
    if (ioctl(fd, VIDIOC_S_DQBUF_TIMEOUT, &timeout) != 0) {
        ESP_LOGW(TAG, "camera dequeue timeout could not be set: errno %d", errno);
    }

    struct v4l2_requestbuffers request = {
        /* esp-video's default no-backup-buffer mode requires at least two. */
        .count = CAMERA_BUFFER_COUNT,
        .type = type,
        .memory = V4L2_MEMORY_MMAP,
    };
    if (ioctl(fd, VIDIOC_REQBUFS, &request) != 0 || request.count < CAMERA_BUFFER_COUNT) {
        ESP_LOGE(TAG, "camera buffer request failed: errno %d", errno);
        goto cleanup;
    }

    struct v4l2_buffer buffer = {
        .type = type,
        .memory = V4L2_MEMORY_MMAP,
    };
    for (uint32_t index = 0; index < CAMERA_BUFFER_COUNT; index++) {
        buffer.index = index;
        if (ioctl(fd, VIDIOC_QUERYBUF, &buffer) != 0) {
            ESP_LOGE(TAG, "camera buffer %" PRIu32 " query failed: errno %d", index, errno);
            goto cleanup;
        }
        mapping_lengths[index] = buffer.length;
        mappings[index] = mmap(
            NULL,
            buffer.length,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fd,
            buffer.m.offset
        );
        if (mappings[index] == MAP_FAILED) {
            ESP_LOGE(TAG, "camera buffer %" PRIu32 " mapping failed: errno %d", index, errno);
            goto cleanup;
        }
        if (ioctl(fd, VIDIOC_QBUF, &buffer) != 0) {
            ESP_LOGE(TAG, "camera buffer %" PRIu32 " queue failed: errno %d", index, errno);
            goto cleanup;
        }
    }

    const int64_t started_us = esp_timer_get_time();
    if (ioctl(fd, VIDIOC_STREAMON, &type) != 0) {
        ESP_LOGE(TAG, "camera stream start failed: errno %d", errno);
        goto cleanup;
    }
    streaming = true;
    if (ioctl(fd, VIDIOC_DQBUF, &buffer) != 0) {
        ESP_LOGE(TAG, "camera frame receive failed: errno %d", errno);
        goto cleanup;
    }
    if (buffer.index >= CAMERA_BUFFER_COUNT || mappings[buffer.index] == MAP_FAILED) {
        ESP_LOGE(TAG, "camera returned invalid buffer index %" PRIu32, buffer.index);
        goto cleanup;
    }

    probe->width = format.fmt.pix.width;
    probe->height = format.fmt.pix.height;
    probe->frame_bytes = buffer.bytesused;
    probe->capture_ms = (uint32_t)((esp_timer_get_time() - started_us) / 1000);
    raw8_statistics(
        mappings[buffer.index],
        buffer.bytesused,
        probe->width,
        probe->height,
        probe
    );
    result = ESP_OK;

cleanup:
    if (streaming && ioctl(fd, VIDIOC_STREAMOFF, &type) != 0) {
        ESP_LOGW(TAG, "camera stream stop failed: errno %d", errno);
    }
    for (size_t index = 0; index < CAMERA_BUFFER_COUNT; index++) {
        if (mappings[index] != MAP_FAILED) {
            munmap(mappings[index], mapping_lengths[index]);
        }
    }
    if (fd >= 0) {
        close(fd);
    }
    const esp_err_t deinit = esp_video_deinit_with_flags(ESP_VIDEO_INIT_FLAGS_MIPI_CSI);
    if (result == ESP_OK && deinit != ESP_OK) {
        result = deinit;
    }
    return result;
}

esp_err_t njorun_d1001_camera_probe(njorun_d1001_camera_probe_t *probe)
{
    if (probe == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    memset(probe, 0, sizeof(*probe));
    if (s_touch_i2c_bus == NULL || s_expander == NULL) {
        return ESP_ERR_INVALID_STATE;
    }

    probe->light_sensor_off = i2c0_address_present(0x29);
    esp_err_t result = camera_power_on();
    if (result == ESP_OK) {
        probe->light_sensor_on = i2c0_address_present(0x29);
        probe->camera_address_36 = i2c0_address_present(0x36);
        probe->camera_address_10 = i2c0_address_present(0x10);
        result = camera_capture_probe(probe);
    }
    const esp_err_t power_off = camera_power_off();
    if (result == ESP_OK && power_off != ESP_OK) {
        result = power_off;
    }
    ESP_LOGI(
        TAG,
        "camera probe: light 0x29 off=%d on=%d, camera 0x36=%d 0x10=%d, "
        "PID=0x%04x, frame=%" PRIu32 "x%" PRIu32 " (%" PRIu32 " bytes) in %" PRIu32
        " ms, RAW8 min/mean/max=%u/%u/%u",
        probe->light_sensor_off,
        probe->light_sensor_on,
        probe->camera_address_36,
        probe->camera_address_10,
        probe->sensor_pid,
        probe->width,
        probe->height,
        probe->frame_bytes,
        probe->capture_ms,
        probe->raw_min,
        probe->raw_mean,
        probe->raw_max
    );
    return result;
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
    /* DHCP-provided NTP needs CONFIG_LWIP_DHCP_GET_NTP_SRV (off); lwIP is limited to one server. */
    esp_sntp_config_t config = ESP_NETIF_SNTP_DEFAULT_CONFIG("pool.ntp.org");
    const esp_err_t result = esp_netif_sntp_init(&config);
    if (result == ESP_OK || result == ESP_ERR_INVALID_STATE) {
        s_sntp_initialized = true;
    } else {
        ESP_LOGW(TAG, "NTP initialization failed: %s", esp_err_to_name(result));
    }
}

static bool clock_is_valid(void)
{
    return time(NULL) >= (time_t)1704067200; /* 2024-01-01 UTC */
}

static esp_err_t require_valid_clock_for_tls(const char *url)
{
    if (url == NULL || (strncmp(url, "https://", 8) != 0 && strncmp(url, "wss://", 6) != 0)) {
        return ESP_OK;
    }
    start_time_sync();
    for (unsigned attempt = 0; attempt < 15 && !clock_is_valid(); ++attempt) {
        esp_netif_sntp_sync_wait(pdMS_TO_TICKS(1000));
    }
    if (!clock_is_valid()) {
        ESP_LOGE(TAG, "clock still invalid after NTP wait");
        return ESP_ERR_INVALID_STATE;
    }
    return ESP_OK;
}

/* What both interfaces need before they exist: storage, the netif layer and the event loop.
 * Safe to call again. */
static esp_err_t net_base_init(void)
{
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
    return ESP_OK;
}

esp_err_t njorun_d1001_wifi_init(void)
{
    if (s_wifi_initialized) {
        return ESP_OK;
    }
    ESP_RETURN_ON_ERROR(net_base_init(), TAG, "network base init failed");
    s_wifi_netif = esp_netif_create_default_wifi_sta();
    if (s_wifi_netif == NULL) {
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

/* Puts the station netif on DHCP or on the fixed setup. Called while the station is not joined.
 * esp_netif_set_ip_info() needs the DHCP client stopped and clears the DNS servers, so the DNS
 * servers are set after it. With the client stopped, the connected action of esp-netif posts
 * IP_EVENT_STA_GOT_IP itself once the link is up, so the event handler needs no change. */
static esp_err_t apply_ip_config(
    esp_netif_t *netif,
    const njorun_d1001_ip_config_t *ip,
    bool clear_first
)
{
    /* The host name goes out in DHCP option 12. Without one the router lists the tablet as
     * "espressif". It is set while the station is not joined, before the client starts. */
    if (ip != NULL && ip->hostname[0] != '\0') {
        char hostname[sizeof(ip->hostname)] = {0};
        memcpy(hostname, ip->hostname, sizeof(hostname) - 1);
        const esp_err_t named = esp_netif_set_hostname(netif, hostname);
        if (named != ESP_OK) {
            ESP_LOGW(TAG, "host name not set: %s", esp_err_to_name(named));
        }
    }
    if (ip == NULL || !ip->use_static) {
        if (clear_first) {
            /* The interface may hold a fixed address from before. Without clearing it, that
             * address would look like a lease to whoever waits for one. */
            const esp_err_t stopped = esp_netif_dhcpc_stop(netif);
            if (stopped != ESP_OK && stopped != ESP_ERR_ESP_NETIF_DHCP_ALREADY_STOPPED) {
                return stopped;
            }
            esp_netif_ip_info_t none = {0};
            ESP_RETURN_ON_ERROR(esp_netif_set_ip_info(netif, &none), TAG, "address reset failed");
        }
        const esp_err_t result = esp_netif_dhcpc_start(netif);
        return result == ESP_ERR_ESP_NETIF_DHCP_ALREADY_STARTED ? ESP_OK : result;
    }
    if (ip->prefix < 1 || ip->prefix > 30 || ip->dns_count > 2) {
        return ESP_ERR_INVALID_ARG;
    }
    esp_err_t result = esp_netif_dhcpc_stop(netif);
    if (result != ESP_OK && result != ESP_ERR_ESP_NETIF_DHCP_ALREADY_STOPPED) {
        return result;
    }
    esp_netif_ip_info_t info = {0};
    esp_netif_set_ip4_addr(&info.ip, ip->address[0], ip->address[1], ip->address[2], ip->address[3]);
    esp_netif_set_ip4_addr(&info.gw, ip->gateway[0], ip->gateway[1], ip->gateway[2], ip->gateway[3]);
    const uint32_t mask = UINT32_MAX << (32 - ip->prefix);
    esp_netif_set_ip4_addr(
        &info.netmask,
        (mask >> 24) & 0xff,
        (mask >> 16) & 0xff,
        (mask >> 8) & 0xff,
        mask & 0xff
    );
    ESP_RETURN_ON_ERROR(esp_netif_set_ip_info(netif, &info), TAG, "static address failed");
    static const esp_netif_dns_type_t kinds[2] = {ESP_NETIF_DNS_MAIN, ESP_NETIF_DNS_BACKUP};
    for (uint8_t index = 0; index < ip->dns_count; ++index) {
        esp_netif_dns_info_t dns = {0};
        dns.ip.type = ESP_IPADDR_TYPE_V4;
        esp_netif_set_ip4_addr(
            &dns.ip.u_addr.ip4,
            ip->dns[index][0],
            ip->dns[index][1],
            ip->dns[index][2],
            ip->dns[index][3]
        );
        ESP_RETURN_ON_ERROR(esp_netif_set_dns_info(netif, kinds[index], &dns), TAG, "DNS failed");
    }
    return ESP_OK;
}

/* Why the last njorun_d1001_wifi_connect() failed, as the numbers of the header's list. */
static int s_wifi_failure;

int njorun_d1001_wifi_last_failure(void)
{
    return s_wifi_failure;
}

esp_err_t njorun_d1001_wifi_connect(
    const char *ssid,
    const char *password,
    const njorun_d1001_ip_config_t *ip
)
{
    s_wifi_failure = 3; /* other, until something more precise is known */
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
    const esp_err_t configured = apply_ip_config(s_wifi_netif, ip, false);
    if (configured != ESP_OK) {
        ESP_LOGW(TAG, "IP setup failed: %s", esp_err_to_name(configured));
        s_wifi_failure = 2; /* address */
        return configured;
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
        s_wifi_failure = 0;
        start_time_sync();
        return ESP_OK;
    }
    /* A disconnect event is the driver saying the network could not be joined (name, password,
     * reach). Without one the wait ran out, which means no address came in time. */
    if ((bits & WIFI_FAILED_BIT) != 0) {
        s_wifi_failure = 1; /* association */
        return ESP_ERR_WIFI_CONN;
    }
    s_wifi_failure = 2; /* address */
    return ESP_ERR_TIMEOUT;
}

esp_err_t njorun_d1001_wifi_ipv4(uint8_t address[4])
{
    if (address == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (!s_wifi_initialized || s_wifi_netif == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    esp_netif_ip_info_t info = {0};
    ESP_RETURN_ON_ERROR(
        esp_netif_get_ip_info(s_wifi_netif, &info),
        TAG,
        "station IPv4 lookup failed"
    );
    if (info.ip.addr == 0) {
        return ESP_ERR_INVALID_STATE;
    }
    address[0] = esp_ip4_addr1(&info.ip);
    address[1] = esp_ip4_addr2(&info.ip);
    address[2] = esp_ip4_addr3(&info.ip);
    address[3] = esp_ip4_addr4(&info.ip);
    return ESP_OK;
}

/* Wired Ethernet (plan 006 slice 5, decision N8). Built only with CONFIG_NJORUN_ETHERNET, for a
 * board with a PHY on the RMII pins of the ESP32-P4. The D1001 has none, so this has not run on
 * hardware. Without the option the functions answer "not supported" and the board has no wired
 * interface as far as the firmware is concerned. */
static int s_eth_failure;

int njorun_d1001_ethernet_last_failure(void)
{
    return s_eth_failure;
}

#if CONFIG_NJORUN_ETHERNET

#include "esp_eth.h"
#include "esp_eth_mac_esp.h"
#include "esp_eth_phy.h"

static esp_eth_handle_t s_eth_handle;
static volatile bool s_eth_link;
static bool s_eth_started;

static void eth_event_handler(
    void *argument,
    esp_event_base_t event_base,
    int32_t event_id,
    void *event_data
)
{
    (void)argument;
    (void)event_data;
    if (event_base != ETH_EVENT) {
        return;
    }
    if (event_id == ETHERNET_EVENT_CONNECTED) {
        s_eth_link = true;
    } else if (event_id == ETHERNET_EVENT_DISCONNECTED) {
        s_eth_link = false;
    }
}

/* Creates the driver, the netif and the glue between them. Nothing runs until esp_eth_start(). */
static esp_err_t ethernet_install(void)
{
    ESP_RETURN_ON_ERROR(net_base_init(), TAG, "network base init failed");
    const esp_netif_config_t netif_config = ESP_NETIF_DEFAULT_ETH();
    s_eth_netif = esp_netif_new(&netif_config);
    if (s_eth_netif == NULL) {
        return ESP_ERR_NO_MEM;
    }
    eth_mac_config_t mac_config = ETH_MAC_DEFAULT_CONFIG();
    eth_esp32_emac_config_t emac_config = ETH_ESP32_EMAC_DEFAULT_CONFIG();
    emac_config.smi_gpio.mdc_num = CONFIG_NJORUN_ETHERNET_MDC_GPIO;
    emac_config.smi_gpio.mdio_num = CONFIG_NJORUN_ETHERNET_MDIO_GPIO;
    esp_eth_mac_t *mac = esp_eth_mac_new_esp32(&emac_config, &mac_config);
    eth_phy_config_t phy_config = ETH_PHY_DEFAULT_CONFIG();
    phy_config.phy_addr = CONFIG_NJORUN_ETHERNET_PHY_ADDR;
    phy_config.reset_gpio_num = CONFIG_NJORUN_ETHERNET_RESET_GPIO;
    esp_eth_phy_t *phy = esp_eth_phy_new_generic(&phy_config);
    if (mac == NULL || phy == NULL) {
        return ESP_ERR_NO_MEM;
    }
    const esp_eth_config_t config = ETH_DEFAULT_CONFIG(mac, phy);
    ESP_RETURN_ON_ERROR(esp_eth_driver_install(&config, &s_eth_handle), TAG, "Ethernet driver failed");
    esp_eth_netif_glue_handle_t glue = esp_eth_new_netif_glue(s_eth_handle);
    if (glue == NULL) {
        return ESP_ERR_NO_MEM;
    }
    ESP_RETURN_ON_ERROR(esp_netif_attach(s_eth_netif, glue), TAG, "Ethernet attach failed");
    ESP_RETURN_ON_ERROR(
        esp_event_handler_register(ETH_EVENT, ESP_EVENT_ANY_ID, eth_event_handler, NULL),
        TAG,
        "Ethernet event handler failed"
    );
    return ESP_OK;
}

bool njorun_d1001_ethernet_present(void)
{
    return true;
}

esp_err_t njorun_d1001_ethernet_start(const njorun_d1001_ip_config_t *ip)
{
    s_eth_failure = 3;
    if (!s_eth_started) {
        const esp_err_t installed = ethernet_install();
        if (installed != ESP_OK) {
            ESP_LOGE(TAG, "Ethernet install failed: %s", esp_err_to_name(installed));
            return installed;
        }
    }
    /* The setup is applied while the link may already be up, so it never waits for an event. */
    const esp_err_t configured = apply_ip_config(s_eth_netif, ip, true);
    if (configured != ESP_OK) {
        ESP_LOGW(TAG, "Ethernet IP setup failed: %s", esp_err_to_name(configured));
        s_eth_failure = 2;
        return configured;
    }
    if (!s_eth_started) {
        ESP_RETURN_ON_ERROR(esp_eth_start(s_eth_handle), TAG, "Ethernet start failed");
        s_eth_started = true;
    }
    return ESP_OK;
}

esp_err_t njorun_d1001_ethernet_ipv4(uint8_t address[4])
{
    if (address == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (!s_eth_started || s_eth_netif == NULL || !s_eth_link) {
        return ESP_ERR_INVALID_STATE;
    }
    esp_netif_ip_info_t info = {0};
    ESP_RETURN_ON_ERROR(esp_netif_get_ip_info(s_eth_netif, &info), TAG, "Ethernet IPv4 lookup failed");
    if (info.ip.addr == 0) {
        return ESP_ERR_INVALID_STATE;
    }
    address[0] = esp_ip4_addr1(&info.ip);
    address[1] = esp_ip4_addr2(&info.ip);
    address[2] = esp_ip4_addr3(&info.ip);
    address[3] = esp_ip4_addr4(&info.ip);
    return ESP_OK;
}

#else /* CONFIG_NJORUN_ETHERNET */

bool njorun_d1001_ethernet_present(void)
{
    return false;
}

esp_err_t njorun_d1001_ethernet_start(const njorun_d1001_ip_config_t *ip)
{
    (void)ip;
    s_eth_failure = 3;
    return ESP_ERR_NOT_SUPPORTED;
}

esp_err_t njorun_d1001_ethernet_ipv4(uint8_t address[4])
{
    (void)address;
    return ESP_ERR_NOT_SUPPORTED;
}

#endif /* CONFIG_NJORUN_ETHERNET */

esp_err_t njorun_d1001_ethernet_wait(uint32_t timeout_ms)
{
    uint8_t address[4];
    /* The address is polled, not awaited by event: a fixed address on a link that is up already
     * posts no event. A link that never came up and an address that never came are told apart. */
    const TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(timeout_ms);
    for (;;) {
        const esp_err_t seen = njorun_d1001_ethernet_ipv4(address);
        if (seen == ESP_OK) {
            s_eth_failure = 0;
            return ESP_OK;
        }
        if (seen == ESP_ERR_NOT_SUPPORTED) {
            s_eth_failure = 3;
            return seen;
        }
        if (xTaskGetTickCount() >= deadline) {
            break;
        }
        vTaskDelay(pdMS_TO_TICKS(100));
    }
#if CONFIG_NJORUN_ETHERNET
    s_eth_failure = s_eth_link ? 2 : 1; /* address : link */
#endif
    return ESP_ERR_TIMEOUT;
}

esp_err_t njorun_d1001_prefer_interface(int interface)
{
    esp_netif_t *netif = interface == NJORUN_D1001_INTERFACE_ETHERNET ? s_eth_netif : s_wifi_netif;
    if (netif == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    /* esp-netif picks the default route by its own priority (Wi-Fi above Ethernet). The wired
     * link has priority here (N8), so the choice is made explicitly. */
    return esp_netif_set_default_netif(netif);
}

esp_err_t njorun_d1001_tcp_probe(
    int interface,
    const char *host,
    uint16_t port,
    uint32_t timeout_ms
)
{
    esp_netif_t *netif = interface == NJORUN_D1001_INTERFACE_ETHERNET ? s_eth_netif : s_wifi_netif;
    if (host == NULL || netif == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    char name[16] = {0};
    ESP_RETURN_ON_ERROR(esp_netif_get_netif_impl_name(netif, name), TAG, "interface name unknown");
    char service[8];
    snprintf(service, sizeof(service), "%u", (unsigned)port);
    const struct addrinfo hints = {.ai_family = AF_INET, .ai_socktype = SOCK_STREAM};
    struct addrinfo *found = NULL;
    if (getaddrinfo(host, service, &hints, &found) != 0 || found == NULL) {
        return ESP_ERR_NOT_FOUND;
    }
    esp_err_t result = ESP_FAIL;
    const int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock >= 0) {
        struct ifreq bound = {0};
        snprintf(bound.ifr_name, sizeof(bound.ifr_name), "%s", name);
        if (setsockopt(sock, SOL_SOCKET, SO_BINDTODEVICE, &bound, sizeof(bound)) == 0) {
            fcntl(sock, F_SETFL, fcntl(sock, F_GETFL, 0) | O_NONBLOCK);
            const int started = connect(sock, found->ai_addr, found->ai_addrlen);
            if (started == 0) {
                result = ESP_OK;
            } else if (errno == EINPROGRESS) {
                fd_set writable;
                FD_ZERO(&writable);
                FD_SET(sock, &writable);
                struct timeval wait = {
                    .tv_sec = (time_t)(timeout_ms / 1000),
                    .tv_usec = (suseconds_t)((timeout_ms % 1000) * 1000),
                };
                if (select(sock + 1, NULL, &writable, NULL, &wait) > 0) {
                    int error = 0;
                    socklen_t length = sizeof(error);
                    getsockopt(sock, SOL_SOCKET, SO_ERROR, &error, &length);
                    result = error == 0 ? ESP_OK : ESP_FAIL;
                } else {
                    result = ESP_ERR_TIMEOUT;
                }
            }
        }
        close(sock);
    }
    freeaddrinfo(found);
    return result;
}

esp_err_t njorun_d1001_wifi_rssi(int8_t *rssi)
{
    if (rssi == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (!s_wifi_initialized) {
        return ESP_ERR_INVALID_STATE;
    }
    wifi_ap_record_t record;
    esp_err_t result = esp_wifi_sta_get_ap_info(&record);
    if (result != ESP_OK) {
        return result;
    }
    *rssi = record.rssi;
    return ESP_OK;
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

/* Turns a failed connection into one of the NJORUN_D1001_ERR_* causes the app explains. */
static esp_err_t classify_connect_failure(esp_err_t tls_error, int tls_stack, int sock_errno)
{
    if (tls_error == ESP_ERR_ESP_TLS_CANNOT_RESOLVE_HOSTNAME) {
        return NJORUN_D1001_ERR_DNS;
    }
    if (tls_error == ESP_ERR_ESP_TLS_CONNECTION_TIMEOUT
        || tls_error == ESP_ERR_ESP_TLS_SERVER_HANDSHAKE_TIMEOUT
        || sock_errno == ETIMEDOUT || sock_errno == EINPROGRESS) {
        return NJORUN_D1001_ERR_TIMEOUT;
    }
    if (sock_errno == ECONNREFUSED) {
        return NJORUN_D1001_ERR_REFUSED;
    }
    if (sock_errno == EHOSTUNREACH || sock_errno == ENETUNREACH || sock_errno == ECONNABORTED) {
        return NJORUN_D1001_ERR_UNREACHABLE;
    }
    if (tls_stack != 0 || tls_error == ESP_ERR_MBEDTLS_SSL_HANDSHAKE_FAILED) {
        return NJORUN_D1001_ERR_CERTIFICATE;
    }
    if (tls_error == ESP_ERR_ESP_TLS_FAILED_CONNECT_TO_HOST) {
        /* lwIP reports a select() timeout without errno: no answer within the timeout. */
        return NJORUN_D1001_ERR_TIMEOUT;
    }
    return ESP_FAIL;
}

static esp_err_t http_failure_cause(esp_http_client_handle_t client, esp_err_t result)
{
    if (result == ESP_OK || result == ESP_ERR_INVALID_STATE) {
        return result;
    }
    int tls_stack = 0;
    const esp_err_t tls_error =
        esp_http_client_get_and_clear_last_tls_error(client, &tls_stack, NULL);
    const esp_err_t cause =
        classify_connect_failure(tls_error, tls_stack, esp_http_client_get_errno(client));
    return cause == ESP_FAIL ? result : cause;
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
        result = http_failure_cause(client, esp_http_client_perform(client));
    }
    if (result == ESP_OK) {
        const int status = esp_http_client_get_status_code(client);
        if (status == 400 || status == 401 || status == 403) {
            result = ESP_ERR_NOT_ALLOWED;
        } else if (status != 200) {
            result = ESP_ERR_INVALID_RESPONSE;
        }
    }
    if (result == ESP_OK && context.overflow) {
        result = ESP_ERR_NO_MEM;
    }
    *response_length = context.length;
    esp_http_client_cleanup(client);
    return result;
}

static bool s_serial_installed;

static esp_err_t serial_install(void)
{
    if (s_serial_installed) {
        return ESP_OK;
    }
    usb_serial_jtag_driver_config_t config = USB_SERIAL_JTAG_DRIVER_CONFIG_DEFAULT();
    config.rx_buffer_size = 1024;
    config.tx_buffer_size = 1024;
    esp_err_t result = usb_serial_jtag_driver_install(&config);
    if (result == ESP_OK || result == ESP_ERR_INVALID_STATE) {
        s_serial_installed = true;
        return ESP_OK;
    }
    return result;
}

esp_err_t njorun_d1001_serial_read(
    uint8_t *buffer,
    size_t capacity,
    uint32_t timeout_ms,
    size_t *length
)
{
    if (buffer == NULL || length == NULL || capacity == 0) {
        return ESP_ERR_INVALID_ARG;
    }
    *length = 0;
    ESP_RETURN_ON_ERROR(serial_install(), TAG, "USB serial driver");
    const int count = usb_serial_jtag_read_bytes(buffer, capacity, pdMS_TO_TICKS(timeout_ms));
    if (count < 0) {
        return ESP_FAIL;
    }
    *length = (size_t)count;
    return ESP_OK;
}

esp_err_t njorun_d1001_serial_write(const uint8_t *data, size_t length)
{
    if (data == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(serial_install(), TAG, "USB serial driver");
    size_t done = 0;
    while (done < length) {
        const int written = usb_serial_jtag_write_bytes(data + done, length - done, pdMS_TO_TICKS(200));
        if (written <= 0) {
            return ESP_ERR_TIMEOUT;
        }
        done += (size_t)written;
    }
    return usb_serial_jtag_wait_tx_done(pdMS_TO_TICKS(200));
}

esp_err_t njorun_d1001_set_utc(int64_t seconds)
{
    const struct timeval now = {.tv_sec = (time_t)seconds, .tv_usec = 0};
    return settimeofday(&now, NULL) == 0 ? ESP_OK : ESP_FAIL;
}

esp_err_t njorun_d1001_http_post_json(
    const char *url,
    const char *body,
    char *response,
    size_t response_capacity,
    size_t *response_length,
    int *status
)
{
    if (url == NULL || body == NULL || response == NULL || response_length == NULL
        || status == NULL || response_capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    *response_length = 0;
    *status = 0;
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
        result = esp_http_client_set_header(client, "Content-Type", "application/json");
    }
    if (result == ESP_OK) {
        result = esp_http_client_set_post_field(client, body, (int)body_length);
    }
    if (result == ESP_OK) {
        result = http_failure_cause(client, esp_http_client_perform(client));
    }
    if (result == ESP_OK) {
        *status = esp_http_client_get_status_code(client);
        if (context.overflow) {
            result = ESP_ERR_NO_MEM;
        }
    }
    *response_length = context.length;
    esp_http_client_cleanup(client);
    return result;
}

esp_err_t njorun_d1001_http_get_bearer(
    const char *url,
    const char *token,
    char *response,
    size_t response_capacity,
    size_t *response_length
)
{
    if (url == NULL || token == NULL || response == NULL || response_length == NULL
        || response_capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    *response_length = 0;
    ESP_RETURN_ON_ERROR(require_valid_clock_for_tls(url), TAG, "TLS requires synchronized time");
    char authorization[1100];
    if (snprintf(authorization, sizeof(authorization), "Bearer %s", token)
        >= (int)sizeof(authorization)) {
        return ESP_ERR_INVALID_SIZE;
    }
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
        .buffer_size = 2048,
        .disable_auto_redirect = true,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (client == NULL) {
        return ESP_ERR_NO_MEM;
    }
    esp_err_t result = esp_http_client_set_header(client, "Authorization", authorization);
    if (result == ESP_OK) {
        result = http_failure_cause(client, esp_http_client_perform(client));
    }
    if (result == ESP_OK) {
        const int status = esp_http_client_get_status_code(client);
        if (status == 401 || status == 403) {
            result = ESP_ERR_NOT_ALLOWED;
        } else if (status != 200) {
            result = ESP_ERR_INVALID_RESPONSE;
        }
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



/* Persistent Home Assistant WebSocket: one request at a time, state_changed events queued. */

#define HA_EVENT_QUEUE_LENGTH 64
#define HA_EVENT_MAX_BYTES (96 * 1024)

typedef struct {
    esp_websocket_client_handle_t client;
    EventGroupHandle_t events;
    SemaphoreHandle_t lock;
    QueueHandle_t event_queue;
    char *buffer;
    size_t capacity;
    size_t length;
    bool overflow;
    char *event_buffer;
    size_t event_length;
    bool in_event;
    bool event_dropped;
    volatile bool failed;
    volatile bool events_lost;
    esp_err_t failure_cause;
} ha_session_t;

static ha_session_t s_ha;

static bool ha_message_is_event(const char *data, int length)
{
    static const char needle[] = "\"type\":\"event\"";
    const int needle_length = (int)sizeof(needle) - 1;
    const int limit = length < 48 ? length : 48;
    for (int index = 0; index + needle_length <= limit; ++index) {
        if (memcmp(data + index, needle, (size_t)needle_length) == 0) {
            return true;
        }
    }
    return false;
}

static bool ha_chunk_is_last(const esp_websocket_event_data_t *data)
{
    return data->fin && data->payload_len >= 0 && data->payload_offset >= 0
        && data->data_len >= 0
        && (size_t)data->payload_offset + (size_t)data->data_len >= (size_t)data->payload_len;
}

static void ha_free_pending_event(void)
{
    free(s_ha.event_buffer);
    s_ha.event_buffer = NULL;
    s_ha.event_length = 0;
}

static void ha_session_event_handler(
    void *argument,
    esp_event_base_t event_base,
    int32_t event_id,
    void *event_data
)
{
    (void)argument;
    (void)event_base;
    if (event_id == WEBSOCKET_EVENT_CONNECTED) {
        xEventGroupSetBits(s_ha.events, WEBSOCKET_CONNECTED_BIT);
        return;
    }
    if (event_id == WEBSOCKET_EVENT_ERROR || event_id == WEBSOCKET_EVENT_DISCONNECTED
        || event_id == WEBSOCKET_EVENT_CLOSED) {
        const esp_websocket_event_data_t *detail = event_data;
        printf(
            "Njörun D1001 HA WebSocket %s: close code %d, tls %d/%d, http %d, errno %d\n",
            event_id == WEBSOCKET_EVENT_ERROR ? "error"
                : event_id == WEBSOCKET_EVENT_CLOSED ? "closed" : "disconnected",
            detail != NULL ? detail->close_status_code : -1,
            detail != NULL ? detail->error_handle.esp_tls_last_esp_err : 0,
            detail != NULL ? detail->error_handle.esp_tls_stack_err : 0,
            detail != NULL ? detail->error_handle.esp_ws_handshake_status_code : 0,
            detail != NULL ? detail->error_handle.esp_transport_sock_errno : 0
        );
        if (detail != NULL && s_ha.failure_cause == ESP_OK) {
            const esp_err_t cause = classify_connect_failure(
                detail->error_handle.esp_tls_last_esp_err,
                detail->error_handle.esp_tls_stack_err,
                detail->error_handle.esp_transport_sock_errno
            );
            s_ha.failure_cause = cause == ESP_FAIL ? ESP_OK : cause;
        }
        s_ha.failed = true;
        xEventGroupSetBits(s_ha.events, WEBSOCKET_FAILED_BIT);
        return;
    }
    if (event_id != WEBSOCKET_EVENT_DATA || event_data == NULL) {
        return;
    }
    const esp_websocket_event_data_t *data = event_data;
    if (data->op_code == 0x8) {
        printf("Njörun D1001 HA WebSocket close frame, code %d\n", data->close_status_code);
        return;
    }
    if (data->op_code != 0x0 && data->op_code != 0x1) {
        return;
    }
    if (data->payload_offset == 0) {
        ha_free_pending_event();
        s_ha.in_event = s_ha.buffer == NULL || ha_message_is_event(data->data_ptr, data->data_len);
        if (s_ha.in_event) {
            s_ha.event_dropped = data->payload_len <= 0 || data->payload_len > HA_EVENT_MAX_BYTES;
            if (!s_ha.event_dropped) {
                s_ha.event_buffer = heap_caps_malloc(
                    (size_t)data->payload_len + 1,
                    MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT
                );
                s_ha.event_dropped = s_ha.event_buffer == NULL;
            }
        } else {
            s_ha.length = 0;
            s_ha.overflow = false;
        }
    }
    if (s_ha.in_event) {
        if (!s_ha.event_dropped) {
            const size_t offset = data->payload_offset < 0 ? 0 : (size_t)data->payload_offset;
            const size_t chunk = data->data_len < 0 ? 0 : (size_t)data->data_len;
            if (offset != s_ha.event_length || offset + chunk > (size_t)data->payload_len) {
                s_ha.event_dropped = true;
            } else {
                memcpy(s_ha.event_buffer + offset, data->data_ptr, chunk);
                s_ha.event_length += chunk;
            }
        }
        if (ha_chunk_is_last(data)) {
            if (!s_ha.event_dropped && s_ha.event_length == (size_t)data->payload_len) {
                s_ha.event_buffer[s_ha.event_length] = '\0';
                if (xQueueSend(s_ha.event_queue, &s_ha.event_buffer, 0) == pdTRUE) {
                    s_ha.event_buffer = NULL;
                } else {
                    s_ha.events_lost = true;
                }
            } else {
                s_ha.events_lost = true;
            }
            ha_free_pending_event();
            s_ha.in_event = false;
        }
        return;
    }
    if (s_ha.buffer == NULL) {
        return;
    }
    if (data->payload_offset < 0 || (size_t)data->payload_offset != s_ha.length
        || data->data_len < 0) {
        s_ha.overflow = true;
    } else if (!s_ha.overflow) {
        const size_t chunk = (size_t)data->data_len;
        if (chunk > s_ha.capacity - 1 - s_ha.length) {
            s_ha.overflow = true;
        } else {
            memcpy(s_ha.buffer + s_ha.length, data->data_ptr, chunk);
            s_ha.length += chunk;
            s_ha.buffer[s_ha.length] = '\0';
        }
    }
    if (ha_chunk_is_last(data)) {
        xEventGroupSetBits(s_ha.events, WEBSOCKET_DATA_BIT);
    }
}

static esp_err_t ha_wait(EventBits_t bits, uint32_t timeout_ms)
{
    const EventBits_t received = xEventGroupWaitBits(
        s_ha.events,
        bits | WEBSOCKET_FAILED_BIT,
        pdTRUE,
        pdFALSE,
        pdMS_TO_TICKS(timeout_ms)
    );
    if ((received & bits) != 0) {
        return ESP_OK;
    }
    return (received & WEBSOCKET_FAILED_BIT) != 0 ? ESP_ERR_INVALID_RESPONSE : ESP_ERR_TIMEOUT;
}

static void ha_arm_response(char *buffer, size_t capacity)
{
    xEventGroupClearBits(s_ha.events, WEBSOCKET_DATA_BIT);
    s_ha.length = 0;
    s_ha.overflow = false;
    buffer[0] = '\0';
    s_ha.capacity = capacity;
    s_ha.buffer = buffer;
}

static esp_err_t ha_send_and_wait(
    const char *message,
    char *response,
    size_t capacity,
    uint32_t timeout_ms
)
{
    ha_arm_response(response, capacity);
    const int length = (int)strlen(message);
    const int sent = esp_websocket_client_send_text(
        s_ha.client,
        message,
        length,
        pdMS_TO_TICKS(15000)
    );
    if (sent != length) {
        s_ha.failed = true;
        return ESP_FAIL;
    }
    return ha_wait(WEBSOCKET_DATA_BIT, timeout_ms);
}

void njorun_d1001_ha_session_close(void)
{
    if (s_ha.client != NULL) {
        esp_websocket_client_stop(s_ha.client);
        esp_websocket_client_destroy(s_ha.client);
        s_ha.client = NULL;
    }
    s_ha.buffer = NULL;
    ha_free_pending_event();
    s_ha.in_event = false;
    if (s_ha.event_queue != NULL) {
        char *pending = NULL;
        while (xQueueReceive(s_ha.event_queue, &pending, 0) == pdTRUE) {
            free(pending);
        }
    }
    s_ha.failed = true;
}

esp_err_t njorun_d1001_ha_session_open(const char *websocket_url, const char *auth_message)
{
    if (websocket_url == NULL || auth_message == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(
        require_valid_clock_for_tls(websocket_url),
        TAG,
        "TLS requires synchronized time"
    );
    if (s_ha.events == NULL) {
        s_ha.events = xEventGroupCreate();
        s_ha.lock = xSemaphoreCreateMutex();
        s_ha.event_queue = xQueueCreate(HA_EVENT_QUEUE_LENGTH, sizeof(char *));
        if (s_ha.events == NULL || s_ha.lock == NULL || s_ha.event_queue == NULL) {
            return ESP_ERR_NO_MEM;
        }
    }
    xSemaphoreTake(s_ha.lock, portMAX_DELAY);
    njorun_d1001_ha_session_close();
    xEventGroupClearBits(
        s_ha.events,
        WEBSOCKET_CONNECTED_BIT | WEBSOCKET_DATA_BIT | WEBSOCKET_FAILED_BIT
    );
    s_ha.failed = false;
    s_ha.events_lost = false;
    s_ha.failure_cause = ESP_OK;

    const esp_websocket_client_config_t config = {
        .uri = websocket_url,
        .disable_auto_reconnect = true,
        .buffer_size = 4096,
        .network_timeout_ms = 15000,
        .ping_interval_sec = 20,
        .pingpong_timeout_sec = 45,
        .keep_alive_enable = true,
        .keep_alive_idle = 30,
        .keep_alive_interval = 10,
        .keep_alive_count = 3,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    s_ha.client = esp_websocket_client_init(&config);
    esp_err_t result = s_ha.client == NULL ? ESP_ERR_NO_MEM : ESP_OK;
    if (result == ESP_OK) {
        result = esp_websocket_register_events(
            s_ha.client,
            WEBSOCKET_EVENT_ANY,
            ha_session_event_handler,
            NULL
        );
    }
    char handshake[512];
    if (result == ESP_OK) {
        ha_arm_response(handshake, sizeof(handshake));
        result = esp_websocket_client_start(s_ha.client);
    }
    if (result == ESP_OK) {
        result = ha_wait(WEBSOCKET_CONNECTED_BIT, 15000);
        if (result == ESP_ERR_TIMEOUT) {
            result = NJORUN_D1001_ERR_TIMEOUT;
        } else if (result != ESP_OK && s_ha.failure_cause != ESP_OK) {
            result = s_ha.failure_cause;
        }
    }
    if (result == ESP_OK) {
        result = ha_wait(WEBSOCKET_DATA_BIT, 15000);
    }
    if (result == ESP_OK && strstr(handshake, "\"auth_required\"") == NULL) {
        result = ESP_ERR_INVALID_RESPONSE;
    }
    if (result == ESP_OK) {
        result = ha_send_and_wait(auth_message, handshake, sizeof(handshake), 15000);
    }
    if (result == ESP_OK && strstr(handshake, "\"auth_ok\"") == NULL) {
        result = ESP_ERR_NOT_ALLOWED;
    }
    s_ha.buffer = NULL;
    if (result != ESP_OK) {
        njorun_d1001_ha_session_close();
    }
    xSemaphoreGive(s_ha.lock);
    return result;
}

esp_err_t njorun_d1001_ha_session_request(
    const char *message,
    char *response,
    size_t response_capacity,
    size_t *response_length,
    uint32_t timeout_ms
)
{
    if (message == NULL || response == NULL || response_length == NULL || response_capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    *response_length = 0;
    if (s_ha.client == NULL || s_ha.failed) {
        return ESP_ERR_INVALID_STATE;
    }
    xSemaphoreTake(s_ha.lock, portMAX_DELAY);
    esp_err_t result = ha_send_and_wait(message, response, response_capacity, timeout_ms);
    s_ha.buffer = NULL;
    if (result == ESP_OK && s_ha.overflow) {
        result = ESP_ERR_NO_MEM;
    }
    *response_length = s_ha.length;
    xSemaphoreGive(s_ha.lock);
    return result;
}

esp_err_t njorun_d1001_ha_session_send(const char *message)
{
    if (message == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (s_ha.client == NULL || s_ha.failed) {
        return ESP_ERR_INVALID_STATE;
    }
    const int length = (int)strlen(message);
    const int sent = esp_websocket_client_send_text(
        s_ha.client,
        message,
        length,
        pdMS_TO_TICKS(5000)
    );
    return sent == length ? ESP_OK : ESP_FAIL;
}

esp_err_t njorun_d1001_ha_session_poll_event(
    char *buffer,
    size_t capacity,
    size_t *length,
    uint32_t timeout_ms
)
{
    if (buffer == NULL || length == NULL || capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    *length = 0;
    if (s_ha.event_queue == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    char *event = NULL;
    if (xQueueReceive(s_ha.event_queue, &event, pdMS_TO_TICKS(timeout_ms)) != pdTRUE) {
        return s_ha.failed ? ESP_ERR_INVALID_STATE : ESP_ERR_TIMEOUT;
    }
    const size_t event_length = strlen(event);
    if (event_length + 1 > capacity) {
        free(event);
        return ESP_ERR_NO_MEM;
    }
    memcpy(buffer, event, event_length + 1);
    *length = event_length;
    free(event);
    return ESP_OK;
}

bool njorun_d1001_ha_session_take_events_lost(void)
{
    const bool lost = s_ha.events_lost;
    s_ha.events_lost = false;
    return lost;
}

bool njorun_d1001_ha_session_alive(void)
{
    return s_ha.client != NULL && !s_ha.failed;
}


#define STORE_NAMESPACE "njorun"

static esp_err_t store_init(void)
{
    static bool ready;
    if (ready) {
        return ESP_OK;
    }
    esp_err_t result = nvs_flash_init();
    if (result == ESP_ERR_NVS_NO_FREE_PAGES || result == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_RETURN_ON_ERROR(nvs_flash_erase(), TAG, "incompatible NVS erase failed");
        result = nvs_flash_init();
    }
    ready = result == ESP_OK;
    return result;
}

esp_err_t njorun_d1001_store_get(
    const char *key,
    char *value,
    size_t capacity,
    size_t *length
)
{
    if (key == NULL || value == NULL || length == NULL || capacity < 2) {
        return ESP_ERR_INVALID_ARG;
    }
    *length = 0;
    value[0] = '\0';
    ESP_RETURN_ON_ERROR(store_init(), TAG, "settings storage unavailable");
    nvs_handle_t handle;
    esp_err_t result = nvs_open(STORE_NAMESPACE, NVS_READONLY, &handle);
    if (result == ESP_ERR_NVS_NOT_FOUND) {
        return ESP_ERR_NOT_FOUND;
    }
    ESP_RETURN_ON_ERROR(result, TAG, "settings open failed");
    size_t size = capacity;
    result = nvs_get_str(handle, key, value, &size);
    nvs_close(handle);
    if (result == ESP_ERR_NVS_NOT_FOUND) {
        return ESP_ERR_NOT_FOUND;
    }
    if (result == ESP_OK) {
        *length = size > 0 ? size - 1 : 0;
    }
    return result;
}

esp_err_t njorun_d1001_store_set(const char *key, const char *value)
{
    if (key == NULL || value == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(store_init(), TAG, "settings storage unavailable");
    nvs_handle_t handle;
    ESP_RETURN_ON_ERROR(
        nvs_open(STORE_NAMESPACE, NVS_READWRITE, &handle),
        TAG,
        "settings open failed"
    );
    esp_err_t result = nvs_set_str(handle, key, value);
    if (result == ESP_OK) {
        result = nvs_commit(handle);
    }
    nvs_close(handle);
    return result;
}

esp_err_t njorun_d1001_store_erase_all(void)
{
    ESP_RETURN_ON_ERROR(store_init(), TAG, "settings storage unavailable");
    nvs_handle_t handle;
    ESP_RETURN_ON_ERROR(
        nvs_open(STORE_NAMESPACE, NVS_READWRITE, &handle),
        TAG,
        "settings open failed"
    );
    esp_err_t result = nvs_erase_all(handle);
    if (result == ESP_OK) {
        result = nvs_commit(handle);
    }
    nvs_close(handle);
    return result;
}


/* Last crash, kept in RTC memory that survives a software reset (not a power cycle). */
#define CRASH_MAGIC 0x4E4A4352u /* "NJCR" */
typedef struct {
    uint32_t magic;
    char message[256];
} crash_record_t;

static RTC_NOINIT_ATTR crash_record_t s_crash;

void njorun_d1001_crash_save(const char *message)
{
    if (message == NULL) {
        return;
    }
    strncpy(s_crash.message, message, sizeof(s_crash.message) - 1);
    s_crash.message[sizeof(s_crash.message) - 1] = '\0';
    s_crash.magic = CRASH_MAGIC;
}

size_t njorun_d1001_crash_take(char *buffer, size_t capacity)
{
    if (buffer == NULL || capacity < 2 || s_crash.magic != CRASH_MAGIC) {
        return 0;
    }
    s_crash.magic = 0;
    s_crash.message[sizeof(s_crash.message) - 1] = '\0';
    const size_t length = strnlen(s_crash.message, capacity - 1);
    memcpy(buffer, s_crash.message, length);
    buffer[length] = '\0';
    return length;
}

const char *njorun_d1001_reset_reason(void)
{
    switch (esp_reset_reason()) {
    case ESP_RST_POWERON: return "power-on";
    case ESP_RST_EXT: return "external";
    case ESP_RST_SW: return "software";
    case ESP_RST_PANIC: return "panic";
    case ESP_RST_INT_WDT: return "interrupt watchdog";
    case ESP_RST_TASK_WDT: return "task watchdog";
    case ESP_RST_WDT: return "other watchdog";
    case ESP_RST_DEEPSLEEP: return "deep sleep";
    case ESP_RST_BROWNOUT: return "brownout";
    case ESP_RST_SDIO: return "SDIO";
    case ESP_RST_USB: return "USB";
    case ESP_RST_JTAG: return "JTAG";
    case ESP_RST_EFUSE: return "eFuse";
    case ESP_RST_PWR_GLITCH: return "power glitch";
    case ESP_RST_CPU_LOCKUP: return "CPU lockup";
    default: return "unknown";
    }
}


/* ---------------------------------------------------------------------------------------
 * Logging: every console line (the app's own and ESP-IDF's) also goes to
 *  - a crash ring in RTC memory that survives a software reset, read on the next boot, and
 *  - a queue in PSRAM that the log sink drains.
 * The hook never calls back into Rust, allocates or blocks, so it is safe in small tasks.
 * --------------------------------------------------------------------------------------- */

#define LOG_RING_MAGIC 0x4E4A4C47u /* "NJLG" */
#define LOG_RING_BYTES (6 * 1024)
#define LOG_QUEUE_BYTES (32 * 1024)
#define LOG_LINE_MAX 200

typedef struct {
    uint32_t magic;
    uint32_t boot;
    uint32_t head;   /* next write offset */
    uint32_t used;   /* bytes in the ring, up to LOG_RING_BYTES */
    char data[LOG_RING_BYTES];
} log_ring_t;

static RTC_NOINIT_ATTR log_ring_t s_log_ring;
static portMUX_TYPE s_log_lock = portMUX_INITIALIZER_UNLOCKED;
static char *s_log_queue;
static uint32_t s_log_queue_head;
static uint32_t s_log_queue_used;
static uint32_t s_log_dropped;
static char *s_log_previous;
static size_t s_log_previous_len;
static uint32_t s_log_previous_boot;
static vprintf_like_t s_log_console;

static void ring_put(char *buffer, uint32_t capacity, uint32_t *head, uint32_t *used,
                     const char *text, size_t length)
{
    for (size_t i = 0; i < length; ++i) {
        buffer[*head] = text[i];
        *head = (*head + 1) % capacity;
    }
    *used = (*used + length > capacity) ? capacity : *used + length;
}

void njorun_d1001_log_append(const char *line, size_t length)
{
    if (line == NULL || length == 0) {
        return;
    }
    if (length > LOG_LINE_MAX) {
        length = LOG_LINE_MAX;
    }
    const bool newline = line[length - 1] == '\n';
    portENTER_CRITICAL_SAFE(&s_log_lock);
    if (s_log_ring.magic == LOG_RING_MAGIC) {
        ring_put(s_log_ring.data, LOG_RING_BYTES, &s_log_ring.head, &s_log_ring.used, line, length);
        if (!newline) {
            ring_put(s_log_ring.data, LOG_RING_BYTES, &s_log_ring.head, &s_log_ring.used, "\n", 1);
        }
    }
    if (s_log_queue != NULL) {
        const size_t needed = length + (newline ? 0 : 1);
        if (s_log_queue_used + needed > LOG_QUEUE_BYTES) {
            s_log_dropped++;
        } else {
            ring_put(s_log_queue, LOG_QUEUE_BYTES, &s_log_queue_head, &s_log_queue_used, line, length);
            if (!newline) {
                ring_put(s_log_queue, LOG_QUEUE_BYTES, &s_log_queue_head, &s_log_queue_used, "\n", 1);
            }
        }
    }
    portEXIT_CRITICAL_SAFE(&s_log_lock);
}

static int log_hook(const char *format, va_list args)
{
    va_list copy;
    va_copy(copy, args);
    const int written = s_log_console != NULL ? s_log_console(format, args) : vprintf(format, args);
    if (!xPortInIsrContext()) {
        char line[LOG_LINE_MAX + 1];
        const int length = vsnprintf(line, sizeof(line), format, copy);
        if (length > 0) {
            njorun_d1001_log_append(line, (size_t)length < sizeof(line) ? (size_t)length : sizeof(line) - 1);
        }
    }
    va_end(copy);
    return written;
}

/* Linearizes a ring into `out` (oldest first), skipping a partial first line. */
static size_t ring_copy(const char *buffer, uint32_t capacity, uint32_t head, uint32_t used,
                        char *out, size_t out_capacity)
{
    if (used == 0 || out_capacity < 2) {
        return 0;
    }
    uint32_t start = (head + capacity - used) % capacity;
    size_t length = 0;
    bool skipping = used == capacity;
    for (uint32_t i = 0; i < used && length + 1 < out_capacity; ++i) {
        const char c = buffer[(start + i) % capacity];
        if (skipping) {
            skipping = c != '\n';
            continue;
        }
        out[length++] = c;
    }
    out[length] = '\0';
    return length;
}

esp_err_t njorun_d1001_log_init(void)
{
    if (s_log_queue != NULL) {
        return ESP_OK;
    }
    s_log_previous = heap_caps_malloc(LOG_RING_BYTES + 1, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    s_log_queue = heap_caps_malloc(LOG_QUEUE_BYTES, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (s_log_previous == NULL || s_log_queue == NULL) {
        return ESP_ERR_NO_MEM;
    }
    portENTER_CRITICAL(&s_log_lock);
    const bool valid = s_log_ring.magic == LOG_RING_MAGIC && s_log_ring.used <= LOG_RING_BYTES
        && s_log_ring.head < LOG_RING_BYTES;
    if (valid) {
        s_log_previous_len = ring_copy(s_log_ring.data, LOG_RING_BYTES, s_log_ring.head,
                                       s_log_ring.used, s_log_previous, LOG_RING_BYTES + 1);
        s_log_previous_boot = s_log_ring.boot;
    }
    const uint32_t boot = valid ? s_log_ring.boot + 1 : 1;
    s_log_ring.magic = LOG_RING_MAGIC;
    s_log_ring.boot = boot;
    s_log_ring.head = 0;
    s_log_ring.used = 0;
    portEXIT_CRITICAL(&s_log_lock);
    s_log_console = esp_log_set_vprintf(log_hook);
    return ESP_OK;
}

uint32_t njorun_d1001_log_boot(void)
{
    return s_log_ring.boot;
}

size_t njorun_d1001_log_previous(char *buffer, size_t capacity, uint32_t *boot)
{
    if (buffer == NULL || capacity < 2 || s_log_previous == NULL) {
        return 0;
    }
    const size_t length = s_log_previous_len < capacity - 1 ? s_log_previous_len : capacity - 1;
    memcpy(buffer, s_log_previous, length);
    buffer[length] = '\0';
    if (boot != NULL) {
        *boot = s_log_previous_boot;
    }
    return length;
}

size_t njorun_d1001_log_drain(char *buffer, size_t capacity, uint32_t *dropped)
{
    if (buffer == NULL || capacity < 2 || s_log_queue == NULL) {
        return 0;
    }
    portENTER_CRITICAL(&s_log_lock);
    /* Only whole lines, oldest first, as many as fit. */
    const uint32_t start = (s_log_queue_head + LOG_QUEUE_BYTES - s_log_queue_used) % LOG_QUEUE_BYTES;
    size_t taken = 0;
    size_t last_newline = 0;
    for (uint32_t i = 0; i < s_log_queue_used && taken + 1 < capacity; ++i) {
        const char c = s_log_queue[(start + i) % LOG_QUEUE_BYTES];
        buffer[taken++] = c;
        if (c == '\n') {
            last_newline = taken;
        }
    }
    s_log_queue_used -= (uint32_t)last_newline;
    if (dropped != NULL) {
        *dropped = s_log_dropped;
        s_log_dropped = 0;
    }
    portEXIT_CRITICAL(&s_log_lock);
    buffer[last_newline] = '\0';
    return last_newline;
}

esp_err_t njorun_d1001_http_post(
    const char *url,
    const char *content_type,
    const char *body,
    size_t body_length,
    uint32_t timeout_ms
)
{
    if (url == NULL || content_type == NULL || body == NULL || body_length > INT_MAX) {
        return ESP_ERR_INVALID_ARG;
    }
    ESP_RETURN_ON_ERROR(require_valid_clock_for_tls(url), TAG, "TLS requires synchronized time");
    char discard[64];
    http_response_t context = {
        .buffer = discard,
        .capacity = sizeof(discard),
    };
    const esp_http_client_config_t config = {
        .url = url,
        .event_handler = http_event_handler,
        .user_data = &context,
        .timeout_ms = (int)timeout_ms,
        .disable_auto_redirect = true,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (client == NULL) {
        return ESP_ERR_NO_MEM;
    }
    esp_err_t result = esp_http_client_set_method(client, HTTP_METHOD_POST);
    if (result == ESP_OK) {
        result = esp_http_client_set_header(client, "Content-Type", content_type);
    }
    if (result == ESP_OK) {
        result = esp_http_client_set_post_field(client, body, (int)body_length);
    }
    if (result == ESP_OK) {
        result = http_failure_cause(client, esp_http_client_perform(client));
    }
    if (result == ESP_OK) {
        const int status = esp_http_client_get_status_code(client);
        if (status < 200 || status >= 300) {
            result = ESP_ERR_INVALID_RESPONSE;
        }
    }
    esp_http_client_cleanup(client);
    return result;
}


/* ---- Firmware install (decision D7) ---- */

typedef struct {
    esp_http_client_handle_t client;
    esp_ota_handle_t ota;
    const esp_partition_t *slot;
    bool ota_open;
} ota_state_t;

static ota_state_t s_ota;

esp_err_t njorun_d1001_ota_partitions(uint32_t *ota_count, uint32_t *slot_bytes)
{
    if (ota_count == NULL || slot_bytes == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    uint32_t count = 0;
    for (esp_partition_subtype_t subtype = ESP_PARTITION_SUBTYPE_APP_OTA_MIN;
         subtype < ESP_PARTITION_SUBTYPE_APP_OTA_MAX;
         subtype++) {
        if (esp_partition_find_first(ESP_PARTITION_TYPE_APP, subtype, NULL) != NULL) {
            count++;
        }
    }
    const esp_partition_t *next = esp_ota_get_next_update_partition(NULL);
    *ota_count = count;
    *slot_bytes = (next != NULL) ? (uint32_t)next->size : 0;
    return ESP_OK;
}

bool njorun_d1001_ota_verdict_pending(void)
{
    const esp_partition_t *running = esp_ota_get_running_partition();
    esp_ota_img_states_t state;
    return running != NULL && esp_ota_get_state_partition(running, &state) == ESP_OK
        && state == ESP_OTA_IMG_PENDING_VERIFY;
}

esp_err_t njorun_d1001_ota_mark_valid(void)
{
    return esp_ota_mark_app_valid_cancel_rollback();
}

void njorun_d1001_ota_rollback(void)
{
    /* Returns only when the rollback was not possible; restart so the bootloader decides. */
    (void)esp_ota_mark_app_invalid_rollback_and_reboot();
    esp_restart();
}

void njorun_d1001_ota_http_close(void)
{
    if (s_ota.client != NULL) {
        esp_http_client_close(s_ota.client);
        esp_http_client_cleanup(s_ota.client);
        s_ota.client = NULL;
    }
}

esp_err_t njorun_d1001_ota_http_open(const char *url, const char *token, uint32_t *content_length)
{
    if (url == NULL || token == NULL || content_length == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (s_ota.client != NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    *content_length = 0;
    ESP_RETURN_ON_ERROR(require_valid_clock_for_tls(url), TAG, "TLS requires synchronized time");
    char authorization[1100];
    if (snprintf(authorization, sizeof(authorization), "Bearer %s", token)
        >= (int)sizeof(authorization)) {
        return ESP_ERR_INVALID_SIZE;
    }
    /* No redirects: the token must go only to the host the install command named. */
    const esp_http_client_config_t config = {
        .url = url,
        .timeout_ms = 15000,
        .buffer_size = 2048,
        .disable_auto_redirect = true,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t client = esp_http_client_init(&config);
    if (client == NULL) {
        return ESP_ERR_NO_MEM;
    }
    s_ota.client = client;
    esp_err_t result = esp_http_client_set_header(client, "Authorization", authorization);
    if (result == ESP_OK) {
        result = http_failure_cause(client, esp_http_client_open(client, 0));
    }
    if (result == ESP_OK) {
        const int64_t length = esp_http_client_fetch_headers(client);
        const int status = esp_http_client_get_status_code(client);
        if (status == 401 || status == 403) {
            result = ESP_ERR_NOT_ALLOWED;
        } else if (status != 200 || length < 0 || length > UINT32_MAX) {
            result = ESP_ERR_INVALID_RESPONSE;
        } else {
            *content_length = (uint32_t)length;
        }
    }
    if (result != ESP_OK) {
        njorun_d1001_ota_http_close();
    }
    return result;
}

esp_err_t njorun_d1001_ota_http_read(void *buffer, size_t capacity, size_t *length)
{
    if (buffer == NULL || length == NULL || capacity == 0 || s_ota.client == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    *length = 0;
    const int read = esp_http_client_read(s_ota.client, buffer, (int)capacity);
    if (read < 0) {
        return ESP_ERR_INVALID_RESPONSE;
    }
    if (read == 0 && !esp_http_client_is_complete_data_received(s_ota.client)) {
        return ESP_ERR_INVALID_RESPONSE;
    }
    *length = (size_t)read;
    return ESP_OK;
}

esp_err_t njorun_d1001_ota_begin(uint32_t size)
{
    if (s_ota.ota_open) {
        return ESP_ERR_INVALID_STATE;
    }
    s_ota.slot = esp_ota_get_next_update_partition(NULL);
    if (s_ota.slot == NULL) {
        return ESP_ERR_NOT_FOUND;
    }
    if (size == 0 || size > s_ota.slot->size) {
        s_ota.slot = NULL;
        return ESP_ERR_INVALID_SIZE;
    }
    const esp_err_t result = esp_ota_begin(s_ota.slot, size, &s_ota.ota);
    if (result != ESP_OK) {
        s_ota.slot = NULL;
        return result;
    }
    s_ota.ota_open = true;
    return ESP_OK;
}

esp_err_t njorun_d1001_ota_write(const void *data, size_t length)
{
    if (!s_ota.ota_open) {
        return ESP_ERR_INVALID_STATE;
    }
    return esp_ota_write(s_ota.ota, data, length);
}

esp_err_t njorun_d1001_ota_end(void)
{
    if (!s_ota.ota_open) {
        return ESP_ERR_INVALID_STATE;
    }
    s_ota.ota_open = false;
    const esp_err_t result = esp_ota_end(s_ota.ota);
    if (result != ESP_OK) {
        s_ota.slot = NULL;
    }
    return result;
}

void njorun_d1001_ota_abort(void)
{
    if (s_ota.ota_open) {
        (void)esp_ota_abort(s_ota.ota);
        s_ota.ota_open = false;
    }
    s_ota.slot = NULL;
    njorun_d1001_ota_http_close();
}

esp_err_t njorun_d1001_ota_activate(void)
{
    if (s_ota.ota_open || s_ota.slot == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    return esp_ota_set_boot_partition(s_ota.slot);
}

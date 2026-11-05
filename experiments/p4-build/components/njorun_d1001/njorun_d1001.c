// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#include "njorun_d1001.h"

#include <stdbool.h>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "esp_cache.h"
#include "esp_check.h"
#include "esp_lcd_jd9365_8.h"
#include "esp_lcd_mipi_dsi.h"
#include "esp_lcd_panel_ops.h"
#include "esp_ldo_regulator.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "soc/pmu_reg.h"
#include "soc/soc.h"

static const char *TAG = "njorun_d1001";

enum {
    I2C1_SDA = 20,
    I2C1_SCL = 21,
    LCD_BACKLIGHT = 14,
    PCA9535_ADDRESS = 0x20,
    PCA9535_OUTPUT_0 = 0x02,
    PCA9535_CONFIG_0 = 0x06,
    EXP_LCD_POWER = 1U << 0,
    EXP_LCD_RESET = 1U << 2,
    EXP_LCD_BACKLIGHT_ENABLE = 1U << 7,
    EXP_POWER_HOLD = 1U << 8,
};

static i2c_master_bus_handle_t s_i2c_bus;
static i2c_master_dev_handle_t s_expander;
static esp_ldo_channel_handle_t s_dsi_ldo;
static esp_lcd_dsi_bus_handle_t s_dsi_bus;
static esp_lcd_panel_io_handle_t s_panel_io;
static esp_lcd_panel_handle_t s_panel;
static uint16_t *s_framebuffer;
static uint16_t s_expander_outputs;
static bool s_backlight_initialized;

static esp_err_t expander_write_pair(uint8_t first_register, uint16_t value)
{
    const uint8_t bytes[] = {
        first_register,
        (uint8_t)(value & 0xffU),
        (uint8_t)(value >> 8),
    };
    return i2c_master_transmit(s_expander, bytes, sizeof(bytes), 100);
}

static esp_err_t expander_commit(void)
{
    return expander_write_pair(PCA9535_OUTPUT_0, s_expander_outputs);
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
    ESP_RETURN_ON_ERROR(expander_write_pair(PCA9535_CONFIG_0, 0), TAG, "PCA9535 direction failed");
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

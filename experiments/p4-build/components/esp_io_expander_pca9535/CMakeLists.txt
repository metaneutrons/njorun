# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later

# Compatibility surface for Seeed's GSL3670 component. Njörun owns the actual
# PCA9535 instance so that unused D1001 power rails retain safe output levels.
idf_component_register(
    INCLUDE_DIRS "include"
    REQUIRES
        esp_driver_i2c
        espressif__esp_io_expander
)

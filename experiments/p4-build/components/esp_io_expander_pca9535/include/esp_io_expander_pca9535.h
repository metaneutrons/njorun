// SPDX-FileCopyrightText: 2026 Fabian Schmieder
// SPDX-License-Identifier: GPL-3.0-or-later

#pragma once

/* Seeed's GSL3670 driver includes this board-specific header but uses only the
 * generic esp_io_expander API. The D1001 adapter provides that handle without
 * invoking the unsafe output-reset defaults of Seeed's PCA9535 component. */
#include "esp_io_expander.h"

#pragma once

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

esp_err_t ble_provisioner_init(void);
esp_err_t ble_provisioner_start_scan(void);

#ifdef __cplusplus
}
#endif

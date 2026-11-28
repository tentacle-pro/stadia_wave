#pragma once

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define STADIA_SOFTAP_SSID     "StadiaWave-PoC"
#define STADIA_SOFTAP_PASS     "stadia1234"
#define STADIA_SOFTAP_CHANNEL  6

esp_err_t wifi_ap_init(void);

#ifdef __cplusplus
}
#endif

#pragma once

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define POC3_SOFTAP_SSID     "StadiaWave-PoC"
#define POC3_SOFTAP_PASS     "stadia1234"
#define POC3_SOFTAP_CHANNEL  6

esp_err_t wifi_ap_init(void);

#ifdef __cplusplus
}
#endif

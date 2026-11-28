#include "dns_sntp.h"
#include "dtls_srtp_test.h"
#include "https_server.h"
#include "wifi_ap.h"

#include "esp_log.h"
#include "esp_psram.h"
#include "nvs_flash.h"
#include "psa/crypto.h"

static const char *TAG = "stage1_poc3";

void app_main(void)
{
    esp_err_t ret = nvs_flash_init();
    if (ret == ESP_ERR_NVS_NO_FREE_PAGES || ret == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        ret = nvs_flash_init();
    }
    ESP_ERROR_CHECK(ret);
    ESP_ERROR_CHECK(psa_crypto_init() == PSA_SUCCESS ? ESP_OK : ESP_FAIL);

    ESP_LOGI(TAG, "Stage-1 PoC-3 starting (PSRAM size: %zu bytes)",
             esp_psram_is_initialized() ? esp_psram_get_size() : 0);

    // 1. Verify DTLS 1.2 + use_srtp key export locally first
    ESP_ERROR_CHECK(dtls_srtp_run_selftest());

    // 2. Start SoftAP + DNS Sink + SNTP + GTS HTTPS Server with SNI Clearcut block
    ESP_ERROR_CHECK(wifi_ap_init());
    ESP_ERROR_CHECK(dns_sntp_start());
    ESP_ERROR_CHECK(https_server_start());

    ESP_LOGI(TAG, "Ready! Power on your provisioned Stadia Controller to test GTS TLS & SNI block.");
}

#include "ble_provisioner.h"
#include "wifi_ap.h"

#include "driver/gpio.h"
#include "esp_log.h"
#include "esp_psram.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs_flash.h"

static const char *TAG = "stage1_poc2";
#define BOOT_BUTTON_GPIO GPIO_NUM_0

void app_main(void)
{
    esp_err_t ret = nvs_flash_init();
    if (ret == ESP_ERR_NVS_NO_FREE_PAGES || ret == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        ret = nvs_flash_init();
    }
    ESP_ERROR_CHECK(ret);

    ESP_LOGI(TAG, "Stage-1 PoC-2 starting (PSRAM size: %zu bytes)",
             esp_psram_is_initialized() ? esp_psram_get_size() : 0);

    ESP_ERROR_CHECK(wifi_ap_init());
    ESP_ERROR_CHECK(ble_provisioner_init());

    gpio_config_t io_conf = {
        .pin_bit_mask = (1ULL << BOOT_BUTTON_GPIO),
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
    };
    gpio_config(&io_conf);

    int prev = 1;
    while (1) {
        int cur = gpio_get_level(BOOT_BUTTON_GPIO);
        if (prev == 1 && cur == 0) {
            ESP_LOGI(TAG, "BOOT button pressed -> starting BLE provisioning");
            ble_provisioner_start_scan();
        }
        prev = cur;
        vTaskDelay(pdMS_TO_TICKS(50));
    }
}

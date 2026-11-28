#include "ble_provisioner.h"
#include "display_rpc.h"
#include "dns_sntp.h"
#include "driver/gpio.h"
#include "esp_log.h"
#include "esp_psram.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "game_session.h"
#include "https_server.h"
#include "nvs_flash.h"
#include "rtc_endpoint.h"
#include "session_signaling.h"
#include "usb_bridge.h"
#include "wifi_ap.h"

static const char *TAG = "stadia_adapter";

#define BOOT_BTN_GPIO GPIO_NUM_0

static void boot_button_task(void *arg)
{
    (void)arg;
    gpio_config_t io_conf = {
        .pin_bit_mask = (1ULL << BOOT_BTN_GPIO),
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    gpio_config(&io_conf);

    bool prev_pressed = false;
    while (1) {
        bool pressed = (gpio_get_level(BOOT_BTN_GPIO) == 0);
        if (pressed && !prev_pressed) {
            ESP_LOGI(TAG, "BOOT button pressed -> starting BLE provisioning scan...");
            ble_provisioner_start_scan();
        }
        prev_pressed = pressed;
        vTaskDelay(pdMS_TO_TICKS(50));
    }
}

void app_main(void)
{
    esp_err_t ret = nvs_flash_init();
    if (ret == ESP_ERR_NVS_NO_FREE_PAGES || ret == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        ret = nvs_flash_init();
    }
    ESP_ERROR_CHECK(ret);

    ESP_LOGI(TAG, "Stadia Wireless Adapter starting (PSRAM=%zu bytes)",
             esp_psram_is_initialized() ? esp_psram_get_size() : 0);

    session_signaling_init();
    ESP_ERROR_CHECK(usb_bridge_init());
    ESP_ERROR_CHECK(wifi_ap_init());
    ESP_ERROR_CHECK(dns_sntp_start());
    ESP_ERROR_CHECK(rtc_endpoint_start());
    ESP_ERROR_CHECK(game_session_start());
    usb_bridge_set_rumble_cb(game_session_set_rumble);
    display_rpc_set_input_cb(usb_bridge_update_input);
    ESP_ERROR_CHECK(display_rpc_start());
    ESP_ERROR_CHECK(https_server_start());
    ESP_ERROR_CHECK(ble_provisioner_init());

    xTaskCreatePinnedToCore(boot_button_task, "boot_btn", 3072, NULL, 3, NULL, 0);

    ESP_LOGI(TAG, "Gateway + Signaling + BLE Provisioner ready (SoftAP='%s')", STADIA_SOFTAP_SSID);
}

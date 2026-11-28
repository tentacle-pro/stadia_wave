#include "wifi_ap.h"

#include <string.h>
#include "esp_event.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "esp_netif.h"
#include "esp_wifi.h"

static const char *TAG = "wifi_ap";

static void wifi_event_handler(void *arg, esp_event_base_t event_base,
                               int32_t event_id, void *event_data)
{
    (void)arg;
    if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_AP_STACONNECTED) {
        wifi_event_ap_staconnected_t *ev = (wifi_event_ap_staconnected_t *)event_data;
        ESP_LOGI(TAG, "Station connected: " MACSTR ", AID=%d", MAC2STR(ev->mac), ev->aid);
    } else if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_AP_STADISCONNECTED) {
        wifi_event_ap_stadisconnected_t *ev = (wifi_event_ap_stadisconnected_t *)event_data;
        ESP_LOGI(TAG, "Station disconnected: " MACSTR ", AID=%d, reason=%d",
                 MAC2STR(ev->mac), ev->aid, ev->reason);
    } else if (event_base == IP_EVENT && event_id == IP_EVENT_AP_STAIPASSIGNED) {
        ip_event_ap_staipassigned_t *ev = (ip_event_ap_staipassigned_t *)event_data;
        ESP_LOGI(TAG, "Controller IP assigned: " IPSTR, IP2STR(&ev->ip));
    }
}

esp_err_t wifi_ap_init(void)
{
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    esp_netif_create_default_wifi_ap();

    wifi_init_config_t cfg = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&cfg));

    ESP_ERROR_CHECK(esp_event_handler_instance_register(
        WIFI_EVENT, ESP_EVENT_ANY_ID, &wifi_event_handler, NULL, NULL));
    ESP_ERROR_CHECK(esp_event_handler_instance_register(
        IP_EVENT, IP_EVENT_AP_STAIPASSIGNED, &wifi_event_handler, NULL, NULL));

    wifi_config_t wifi_config = {
        .ap = {
            .ssid = POC3_SOFTAP_SSID,
            .ssid_len = strlen(POC3_SOFTAP_SSID),
            .channel = POC3_SOFTAP_CHANNEL,
            .password = POC3_SOFTAP_PASS,
            .max_connection = 4,
            .authmode = WIFI_AUTH_WPA2_PSK,
            .pairwise_cipher = WIFI_CIPHER_TYPE_CCMP,
            .pmf_cfg = {
                .capable = false,
                .required = false,
            },
        },
    };

    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_AP));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_AP, &wifi_config));
    ESP_ERROR_CHECK(esp_wifi_start());

    ESP_LOGI(TAG, "SoftAP started: SSID='%s', PASS='%s', CH=%d",
             POC3_SOFTAP_SSID, POC3_SOFTAP_PASS, POC3_SOFTAP_CHANNEL);
    return ESP_OK;
}

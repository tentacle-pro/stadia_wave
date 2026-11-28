idf_component_register(SRCS "main.c" "wifi_ap.c" "ble_provisioner.c"
                       INCLUDE_DIRS "."
                       REQUIRES esp_wifi esp_netif esp_event nvs_flash bt mbedtls esp_driver_gpio esp_psram esp_rom)

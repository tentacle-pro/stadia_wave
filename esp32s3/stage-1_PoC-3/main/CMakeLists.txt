idf_component_register(SRCS "main.c" "wifi_ap.c" "dns_sntp.c" "https_server.c" "dtls_srtp_test.c"
                       INCLUDE_DIRS "."
                       EMBED_TXTFILES "certs/gts_fullchain.pem" "certs/gts_privkey.pem"
                       REQUIRES esp_wifi esp_netif esp_event nvs_flash mbedtls lwip esp_psram esp_timer)

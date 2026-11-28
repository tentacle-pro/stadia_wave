idf_component_register(
    SRCS
        "main.c"
        "wifi_ap.c"
        "dns_sntp.c"
        "ble_provisioner.c"
        "session_signaling.c"
        "https_server.c"
        "rtc_endpoint.c"
        "display_rpc.c"
        "proto_gen/controller_message.pb.c"
        "proto_gen/discovery_api.pb.c"
        "proto_gen/input.pb.c"
        "proto_gen/rpc_message.pb.c"
        "proto_gen/streamer_message.pb.c"
        "proto_gen/webrtc_sdp.pb.c"
        "proto_gen/yeti_clientanalytics.pb.c"
    INCLUDE_DIRS
        "."
        "proto_gen"
    EMBED_TXTFILES
        "certs/gts_fullchain.pem"
        "certs/gts_privkey.pem"
    REQUIRES
        esp_psram
        nvs_flash
        mbedtls
        esp_wifi
        esp_netif
        esp_event
        bt
        lwip
        esp_driver_gpio
        esp_rom
        esp_timer
        espressif__cjson
)

idf_build_get_property(build_components BUILD_COMPONENTS)
if(tinyusb IN_LIST build_components)
    set(tinyusb_name tinyusb)
else()
    set(tinyusb_name espressif__tinyusb)
endif()

idf_component_get_property(tusb_lib ${tinyusb_name} COMPONENT_LIB)
idf_component_get_property(uac_dir espressif__usb_device_uac COMPONENT_DIR)

target_include_directories(${tusb_lib} PUBLIC
    "${CMAKE_CURRENT_LIST_DIR}"
    "${uac_dir}/tusb_uac"
)
target_sources(${tusb_lib} PUBLIC "${CMAKE_CURRENT_LIST_DIR}/usb_descriptors.c")

idf_component_get_property(nanopb_lib nikas-belogolov__nanopb COMPONENT_LIB)
target_compile_definitions(${nanopb_lib} PUBLIC PB_FIELD_32BIT=1)

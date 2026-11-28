idf_component_register(SRCS "main.c"
                       INCLUDE_DIRS ".")

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

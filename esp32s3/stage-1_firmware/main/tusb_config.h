#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include "sdkconfig.h"
#include "uac_config.h"
#include "uac_descriptors.h"
#include "tusb_config_uac.h"

// Board / Port Configuration
#define CFG_TUSB_RHPORT0_MODE     (OPT_MODE_DEVICE | OPT_MODE_FULL_SPEED)
#define CONFIG_USB_HS             0

// Common TinyUSB Configuration
#ifndef CFG_TUSB_MCU
#error CFG_TUSB_MCU must be defined
#endif

#ifndef CFG_TUSB_OS
#define CFG_TUSB_OS               OPT_OS_FREERTOS
#endif

#ifndef ESP_PLATFORM
#define ESP_PLATFORM              1
#endif

#ifndef CFG_TUSB_DEBUG
#define CFG_TUSB_DEBUG            0
#endif

#define CFG_TUSB_OS_INC_PATH      freertos/

// Enable Device stack
#define CFG_TUD_ENABLED           1

#ifndef CFG_TUSB_MEM_SECTION
#define CFG_TUSB_MEM_SECTION
#endif

#ifndef CFG_TUSB_MEM_ALIGN
#define CFG_TUSB_MEM_ALIGN        __attribute__((aligned(4)))
#endif

#ifndef CFG_TUD_ENDPOINT0_SIZE
#define CFG_TUD_ENDPOINT0_SIZE    64
#endif

// Enable HID alongside UAC
#define CFG_TUD_HID               1
#define CFG_TUD_HID_EP_BUFSIZE    64

#ifdef __cplusplus
}
#endif

#pragma once

#include "display_rpc.h"
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*usb_rumble_cb_t)(uint8_t strong_left, uint8_t weak_right);

esp_err_t usb_bridge_init(void);
void usb_bridge_update_input(const stadia_controller_input_t *in);
void usb_bridge_set_rumble_cb(usb_rumble_cb_t cb);

#ifdef __cplusplus
}
#endif

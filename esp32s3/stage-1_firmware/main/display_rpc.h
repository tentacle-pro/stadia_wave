#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint32_t buttons;
    int16_t lx;
    int16_t ly;
    int16_t rx;
    int16_t ry;
    uint8_t l2;
    uint8_t r2;
    bool headset_plugged;
    float battery_level;
} stadia_controller_input_t;

typedef void (*display_rpc_input_cb_t)(const stadia_controller_input_t *input);

esp_err_t display_rpc_start(void);
bool display_rpc_is_ready(void);
void display_rpc_set_input_cb(display_rpc_input_cb_t cb);
esp_err_t display_rpc_send_set_audio_mode(uint32_t mode, bool f3);
esp_err_t display_rpc_send_set_microphone(bool enabled);
esp_err_t display_rpc_send_start_game(const char *game_key);

#ifdef __cplusplus
}
#endif

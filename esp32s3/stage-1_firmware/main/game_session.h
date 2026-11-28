#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define GAME_LOCAL_PORT 40002

esp_err_t game_session_start(void);
bool game_session_is_connected(void);

// Handles ControllerToDisplay.proxy_request (`body` is serialized inner RpcMessage).
// Writes serialized DisplayToController (ProxyResponse) into `out_dtc_buf`.
bool game_session_handle_proxy_request(const uint8_t *inner_rpc_buf, size_t inner_rpc_len,
                                       uint8_t *out_dtc_buf, size_t out_dtc_max, size_t *out_dtc_len);

void game_session_set_rumble(uint8_t left_8bit, uint8_t right_8bit);

#ifdef __cplusplus
}
#endif

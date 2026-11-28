#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define RTC_LOCAL_PORT          40000
#define WIRE_ID_RTP             0x0066
#define WIRE_ID_DC_RPC_CTRL     0x0074
#define WIRE_ID_DC_RPC_DISPLAY  0x0075
#define WIRE_ID_GAME_RPC        0xFFFA

typedef void (*rtc_wire_rx_cb_t)(uint16_t msg_id, const uint8_t *payload, size_t len);

esp_err_t rtc_endpoint_start(void);
bool rtc_endpoint_is_dtls_connected(void);
bool rtc_endpoint_is_srtp_ready(void);
void rtc_endpoint_set_wire_rx_cb(rtc_wire_rx_cb_t cb);
esp_err_t rtc_endpoint_send_wire(uint16_t msg_id, const uint8_t *payload, size_t len);
esp_err_t rtc_endpoint_send_audio_opus(const uint8_t *opus, size_t len, uint32_t timestamp);
bool rtc_endpoint_recv_mic_opus(uint8_t *out_buf, size_t max_len, size_t *out_len);

#ifdef __cplusplus
}
#endif

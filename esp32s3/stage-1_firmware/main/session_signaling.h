#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    bool valid;
    char ice_ufrag[16];
    char ice_password[64];
    char fingerprint[128];
    uint32_t audio_ssrc;
    bool raw_dtls;
    bool delayed_acks;
    bool use_rtp_timestamps;
} stadia_remote_rtc_info_t;

void session_signaling_init(void);
void session_signaling_set_dtls_fingerprint(const char *sha256_fp);
void session_signaling_set_local_endpoint(const char *ip, uint16_t port);

bool session_signaling_get_remote_rtc(stadia_remote_rtc_info_t *out);
bool session_signaling_is_discovery_resolved(void);

bool session_signaling_handle_firmware_check(const uint8_t *req, size_t req_len,
                                             uint8_t *resp, size_t resp_max, size_t *resp_len);

bool session_signaling_handle_register(const uint8_t *req, size_t req_len,
                                       uint8_t *resp, size_t resp_max, size_t *resp_len);

bool session_signaling_handle_discover(const uint8_t *req, size_t req_len,
                                       uint8_t *resp, size_t resp_max, size_t *resp_len);

bool session_signaling_handle_gmsg(const uint8_t *req, size_t req_len,
                                   uint8_t *resp, size_t resp_max, size_t *resp_len,
                                   uint32_t hold_ms);

bool session_signaling_handle_resolve(const uint8_t *req, size_t req_len,
                                      uint8_t *resp, size_t resp_max, size_t *resp_len);

#ifdef __cplusplus
}
#endif

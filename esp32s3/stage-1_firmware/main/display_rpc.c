#include "display_rpc.h"
#include "rtc_endpoint.h"
#include "session_signaling.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "controller_message.pb.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "pb_decode.h"
#include "pb_encode.h"
#include "rpc_message.pb.h"

static const char *TAG = "disp_rpc";

typedef struct {
    char *buf;
    size_t cap;
} str_dec_t;

typedef struct {
    uint8_t *buf;
    size_t cap;
    size_t len;
} bytes_dec_t;

typedef struct {
    const uint8_t *data;
    size_t len;
} bytes_enc_t;

typedef struct {
    const char *str;
} str_enc_t;

static volatile bool s_ready = false;
static uint32_t s_next_ticket = 1;
static uint32_t s_get_state_ticket = 0;
static display_rpc_input_cb_t s_input_cb = NULL;
static stadia_controller_input_t s_last_input = {0};

static bool dec_str_cb(pb_istream_t *stream, const pb_field_t *field, void **arg)
{
    (void)field;
    str_dec_t *c = (str_dec_t *)(*arg);
    size_t n = stream->bytes_left;
    if (!c || !c->buf || c->cap == 0) {
        return pb_read(stream, NULL, n);
    }
    size_t k = n < c->cap - 1 ? n : c->cap - 1;
    if (k > 0 && !pb_read(stream, (pb_byte_t *)c->buf, k)) {
        return false;
    }
    c->buf[k] = '\0';
    if (n > k) {
        return pb_read(stream, NULL, n - k);
    }
    return true;
}

static bool dec_bytes_cb(pb_istream_t *stream, const pb_field_t *field, void **arg)
{
    (void)field;
    bytes_dec_t *c = (bytes_dec_t *)(*arg);
    size_t n = stream->bytes_left;
    if (!c || !c->buf || n > c->cap) {
        return pb_read(stream, NULL, n);
    }
    if (n > 0 && !pb_read(stream, (pb_byte_t *)c->buf, n)) {
        return false;
    }
    c->len = n;
    return true;
}

static bool enc_bytes_cb(pb_ostream_t *stream, const pb_field_t *field, void *const *arg)
{
    const bytes_enc_t *c = (const bytes_enc_t *)(*arg);
    if (!c || !c->data || c->len == 0) return true;
    return pb_encode_tag_for_field(stream, field) &&
           pb_encode_string(stream, (const pb_byte_t *)c->data, c->len);
}

static bool enc_str_cb(pb_ostream_t *stream, const pb_field_t *field, void *const *arg)
{
    const char *s = (const char *)(*arg);
    if (!s) return true;
    return pb_encode_tag_for_field(stream, field) &&
           pb_encode_string(stream, (const pb_byte_t *)s, strlen(s));
}

static esp_err_t send_rpc_request(const gotham_display_DisplayToController *dtc, uint32_t *out_ticket)
{
    uint8_t *inner = malloc(256);
    uint8_t *outer = malloc(320);
    if (!inner || !outer) {
        free(inner);
        free(outer);
        return ESP_ERR_NO_MEM;
    }

    pb_ostream_t is = pb_ostream_from_buffer(inner, 256);
    if (!pb_encode(&is, gotham_display_DisplayToController_fields, dtc)) {
        ESP_LOGE(TAG, "DisplayToController encode failed: %s", PB_GET_ERROR(&is));
        free(inner);
        free(outer);
        return ESP_FAIL;
    }

    uint32_t ticket = s_next_ticket++;
    bytes_enc_t pay_ctx = {.data = inner, .len = is.bytes_written};

    gotham_rpc_RpcMessage rpc = gotham_rpc_RpcMessage_init_zero;
    rpc.which_payload = gotham_rpc_RpcMessage_request_tag;
    rpc.payload.request.has_ticket = true;
    rpc.payload.request.ticket = ticket;
    rpc.payload.request.payload.funcs.encode = enc_bytes_cb;
    rpc.payload.request.payload.arg = &pay_ctx;

    pb_ostream_t os = pb_ostream_from_buffer(outer, 320);
    esp_err_t ret = ESP_FAIL;
    if (pb_encode(&os, gotham_rpc_RpcMessage_fields, &rpc)) {
        ret = rtc_endpoint_send_wire(WIRE_ID_DC_RPC_DISPLAY, outer, os.bytes_written);
        if (out_ticket) *out_ticket = ticket;
    } else {
        ESP_LOGE(TAG, "RpcMessage encode failed: %s", PB_GET_ERROR(&os));
    }

    free(inner);
    free(outer);
    return ret;
}

static esp_err_t send_rpc_response(uint32_t ticket, gotham_rpc_StatusCode code,
                                   const gotham_display_DisplayToController *dtc)
{
    uint8_t *inner = malloc(256);
    uint8_t *outer = malloc(320);
    if (!inner || !outer) {
        free(inner);
        free(outer);
        return ESP_ERR_NO_MEM;
    }

    size_t inner_len = 0;
    if (dtc) {
        pb_ostream_t is = pb_ostream_from_buffer(inner, 256);
        if (!pb_encode(&is, gotham_display_DisplayToController_fields, dtc)) {
            free(inner);
            free(outer);
            return ESP_FAIL;
        }
        inner_len = is.bytes_written;
    }

    bytes_enc_t pay_ctx = {.data = inner, .len = inner_len};
    gotham_rpc_RpcMessage rpc = gotham_rpc_RpcMessage_init_zero;
    rpc.which_payload = gotham_rpc_RpcMessage_response_tag;
    rpc.payload.response.has_ticket = true;
    rpc.payload.response.ticket = ticket;
    rpc.payload.response.has_code = true;
    rpc.payload.response.code = code;
    rpc.payload.response.error_message.funcs.encode = enc_str_cb;
    rpc.payload.response.error_message.arg = (void *)"";
    if (inner_len > 0) {
        rpc.payload.response.payload.funcs.encode = enc_bytes_cb;
        rpc.payload.response.payload.arg = &pay_ctx;
    }

    pb_ostream_t os = pb_ostream_from_buffer(outer, 320);
    esp_err_t ret = ESP_FAIL;
    if (pb_encode(&os, gotham_rpc_RpcMessage_fields, &rpc)) {
        ret = rtc_endpoint_send_wire(WIRE_ID_DC_RPC_DISPLAY, outer, os.bytes_written);
    }

    free(inner);
    free(outer);
    return ret;
}

static void handle_controller_to_display_req(uint32_t ticket, const uint8_t *buf, size_t len)
{
    gotham_display_ControllerToDisplay *ctd = calloc(1, sizeof(gotham_display_ControllerToDisplay));
    if (!ctd) return;

    pb_istream_t is = pb_istream_from_buffer(buf, len);
    if (!pb_decode(&is, gotham_display_ControllerToDisplay_fields, ctd)) {
        ESP_LOGW(TAG, "CTD req decode failed: %s", PB_GET_ERROR(&is));
        free(ctd);
        return;
    }

    if (ctd->which_variant == gotham_display_ControllerToDisplay_get_current_time_tag) {
        gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
        dtc.has_current_time = true;
        dtc.current_time.has_controller_time_us = true;
        dtc.current_time.controller_time_us = ctd->variant.get_current_time.controller_time_us;
        dtc.current_time.has_display_time_us = true;
        dtc.current_time.display_time_us = 1764300000000000LL + esp_timer_get_time();
        send_rpc_response(ticket, gotham_rpc_StatusCode_OK, &dtc);
    } else if (ctd->which_variant == gotham_display_ControllerToDisplay_keepalive_tag) {
        send_rpc_response(ticket, gotham_rpc_StatusCode_OK, NULL);
    } else if (ctd->which_variant == gotham_display_ControllerToDisplay_disconnect_tag) {
        ESP_LOGW(TAG, "Controller DisconnectRequest (reason=%lu)",
                 (unsigned long)ctd->variant.disconnect.reason);
        send_rpc_response(ticket, gotham_rpc_StatusCode_OK, NULL);
        s_ready = false;
    } else if (ctd->which_variant == gotham_display_ControllerToDisplay_proxy_request_tag) {
        ESP_LOGI(TAG, "Controller ProxyRequest (ticket=%lu)", (unsigned long)ticket);
        send_rpc_response(ticket, gotham_rpc_StatusCode_UNIMPLEMENTED, NULL);
    } else {
        ESP_LOGD(TAG, "Unhandled CTD request variant=%d (ticket=%lu)",
                 ctd->which_variant, (unsigned long)ticket);
        send_rpc_response(ticket, gotham_rpc_StatusCode_UNIMPLEMENTED, NULL);
    }

    free(ctd);
}

static void handle_controller_to_display_resp(uint32_t ticket, gotham_rpc_StatusCode code,
                                              const uint8_t *buf, size_t len)
{
    if (ticket == s_get_state_ticket) {
        if (code != gotham_rpc_StatusCode_OK) {
            ESP_LOGW(TAG, "GetControllerStateResponse err=%d (retrying)", (int)code);
            return;
        }
        gotham_display_ControllerToDisplay *ctd = calloc(1, sizeof(gotham_display_ControllerToDisplay));
        if (!ctd) return;

        char dev_id[48] = {0};
        char mfg[32] = {0};
        char prod[24] = {0};
        char hw[16] = {0};
        char fw[16] = {0};
        char serial[16] = {0};
        str_dec_t d_dev = {dev_id, sizeof(dev_id)};
        str_dec_t d_mfg = {mfg, sizeof(mfg)};
        str_dec_t d_prod = {prod, sizeof(prod)};
        str_dec_t d_hw = {hw, sizeof(hw)};
        str_dec_t d_fw = {fw, sizeof(fw)};
        str_dec_t d_ser = {serial, sizeof(serial)};

        ctd->variant.controller_state.device_id.funcs.decode = dec_str_cb;
        ctd->variant.controller_state.device_id.arg = &d_dev;
        ctd->variant.controller_state.f12.funcs.decode = dec_str_cb;
        ctd->variant.controller_state.f12.arg = &d_mfg;
        ctd->variant.controller_state.f14.funcs.decode = dec_str_cb;
        ctd->variant.controller_state.f14.arg = &d_prod;
        ctd->variant.controller_state.f15.funcs.decode = dec_str_cb;
        ctd->variant.controller_state.f15.arg = &d_hw;
        ctd->variant.controller_state.f16.funcs.decode = dec_str_cb;
        ctd->variant.controller_state.f16.arg = &d_fw;
        ctd->variant.controller_state.f17.funcs.decode = dec_str_cb;
        ctd->variant.controller_state.f17.arg = &d_ser;

        pb_istream_t is = pb_istream_from_buffer(buf, len);
        if (pb_decode(&is, gotham_display_ControllerToDisplay_fields, ctd) &&
            ctd->which_variant == gotham_display_ControllerToDisplay_controller_state_tag) {
            s_last_input.headset_plugged = ctd->variant.controller_state.f4;
            s_ready = true;
            ESP_LOGI(TAG, ">>> MILESTONE: Display Link READY! <<<");
            ESP_LOGI(TAG, "  HW: mfg='%s', prod='%s', hw='%s', fw='%s', serial='%s', headset=%d",
                     mfg, prod, hw, fw, serial, s_last_input.headset_plugged);
        }
        free(ctd);
    } else {
        ESP_LOGI(TAG, "RPC Response ticket=%lu, code=%d, len=%zu",
                 (unsigned long)ticket, (int)code, len);
    }
}

static void handle_controller_to_display_notif(const uint8_t *buf, size_t len)
{
    gotham_display_ControllerToDisplay *ctd = calloc(1, sizeof(gotham_display_ControllerToDisplay));
    if (!ctd) return;

    pb_istream_t is = pb_istream_from_buffer(buf, len);
    if (pb_decode(&is, gotham_display_ControllerToDisplay_fields, ctd)) {
        if (ctd->which_variant == gotham_display_ControllerToDisplay_message3_tag) {
            const gotham_display_Message3 *m = &ctd->variant.message3;
            s_last_input.buttons = m->f1;
            s_last_input.lx = (int16_t)m->f2;
            s_last_input.ly = (int16_t)m->f3;
            s_last_input.rx = (int16_t)m->f4;
            s_last_input.ry = (int16_t)m->f5;
            s_last_input.l2 = (uint8_t)m->f6;
            s_last_input.r2 = (uint8_t)m->f7;
            if (s_input_cb) {
                s_input_cb(&s_last_input);
            } else {
                ESP_LOGI(TAG, "Input: btn=0x%05lx L=(%d,%d) R=(%d,%d) trig=(%u,%u)",
                         (unsigned long)s_last_input.buttons,
                         s_last_input.lx, s_last_input.ly,
                         s_last_input.rx, s_last_input.ry,
                         s_last_input.l2, s_last_input.r2);
            }
        } else if (ctd->which_variant == gotham_display_ControllerToDisplay_audio_configuration_changed_tag) {
            s_last_input.headset_plugged = (ctd->variant.audio_configuration_changed.mode != 0);
            ESP_LOGI(TAG, "Headset jack: %s (mode=%lu)",
                     s_last_input.headset_plugged ? "PLUGGED" : "UNPLUGGED",
                     (unsigned long)ctd->variant.audio_configuration_changed.mode);
        } else if (ctd->which_variant == gotham_display_ControllerToDisplay_notification10_tag) {
            s_last_input.battery_level = ctd->variant.notification10.value;
            ESP_LOGI(TAG, "Battery: %.0f%%", s_last_input.battery_level * 100.0f);
        }
    }
    free(ctd);
}

static void on_wire_rx(uint16_t msg_id, const uint8_t *payload, size_t len)
{
    if (msg_id != WIRE_ID_DC_RPC_CTRL && msg_id != WIRE_ID_DC_RPC_DISPLAY) {
        ESP_LOGD(TAG, "Non-display WireHeader2 id=0x%04x, len=%zu", msg_id, len);
        return;
    }

    uint8_t *inner = malloc(1024);
    if (!inner) return;
    bytes_dec_t pay_ctx = {.buf = inner, .cap = 1024, .len = 0};

    gotham_rpc_RpcMessage rpc = gotham_rpc_RpcMessage_init_zero;
    if (len > 0 && payload[0] == 0x0a) {
        rpc.payload.request.payload.funcs.decode = dec_bytes_cb;
        rpc.payload.request.payload.arg = &pay_ctx;
    } else if (len > 0 && payload[0] == 0x12) {
        rpc.payload.response.payload.funcs.decode = dec_bytes_cb;
        rpc.payload.response.payload.arg = &pay_ctx;
    } else if (len > 0 && payload[0] == 0x1a) {
        rpc.payload.notification.payload.funcs.decode = dec_bytes_cb;
        rpc.payload.notification.payload.arg = &pay_ctx;
    }

    pb_istream_t is = pb_istream_from_buffer(payload, len);
    if (!pb_decode(&is, gotham_rpc_RpcMessage_fields, &rpc)) {
        ESP_LOGW(TAG, "RpcMessage decode failed: %s", PB_GET_ERROR(&is));
        free(inner);
        return;
    }

    if (rpc.which_payload == gotham_rpc_RpcMessage_request_tag) {
        handle_controller_to_display_req(rpc.payload.request.ticket, inner, pay_ctx.len);
    } else if (rpc.which_payload == gotham_rpc_RpcMessage_response_tag) {
        handle_controller_to_display_resp(rpc.payload.response.ticket,
                                          rpc.payload.response.code,
                                          inner, pay_ctx.len);
    } else if (rpc.which_payload == gotham_rpc_RpcMessage_notification_tag) {
        handle_controller_to_display_notif(inner, pay_ctx.len);
    }

    free(inner);
}

static void display_rpc_task(void *arg)
{
    (void)arg;
    bool was_dtls = false;
    int64_t dtls_up_us = 0;
    int64_t last_probe_us = 0;

    while (1) {
        bool dtls = rtc_endpoint_is_dtls_connected();
        int64_t now = esp_timer_get_time();

        if (!dtls) {
            was_dtls = false;
            s_ready = false;
            vTaskDelay(pdMS_TO_TICKS(100));
            continue;
        }

        if (!was_dtls) {
            was_dtls = true;
            dtls_up_us = now;
            last_probe_us = 0;
        }

        // Wait for ResolveDiscovery (or 1.5s fallback after DTLS) before sending GetControllerState
        bool can_start = session_signaling_is_discovery_resolved() || (now - dtls_up_us >= 1500000);
        if (can_start && !s_ready && (last_probe_us == 0 || now - last_probe_us >= 1500000)) {
            last_probe_us = now;
            gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
            dtc.has_get_controller_state = true;
            dtc.get_controller_state.has_display_supports_send_log_report = true;
            dtc.get_controller_state.display_supports_send_log_report = false;
            ESP_LOGI(TAG, "Sending GetControllerStateRequest...");
            send_rpc_request(&dtc, &s_get_state_ticket);
        }

        vTaskDelay(pdMS_TO_TICKS(100));
    }
}

esp_err_t display_rpc_start(void)
{
    rtc_endpoint_set_wire_rx_cb(on_wire_rx);
    xTaskCreatePinnedToCore(display_rpc_task, "disp_rpc", 6144, NULL, 5, NULL, 0);
    return ESP_OK;
}

bool display_rpc_is_ready(void)
{
    return s_ready;
}

void display_rpc_set_input_cb(display_rpc_input_cb_t cb)
{
    s_input_cb = cb;
}

esp_err_t display_rpc_send_set_audio_mode(uint32_t mode, bool f3)
{
    gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
    dtc.has_set_audio_mode = true;
    dtc.set_audio_mode.has_mode = true;
    dtc.set_audio_mode.mode = mode;
    dtc.set_audio_mode.has_f3 = true;
    dtc.set_audio_mode.f3 = f3;
    return send_rpc_request(&dtc, NULL);
}

esp_err_t display_rpc_send_set_microphone(bool enabled)
{
    gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
    dtc.has_set_microphone = true;
    dtc.set_microphone.has_value = true;
    dtc.set_microphone.value = enabled;
    return send_rpc_request(&dtc, NULL);
}

esp_err_t display_rpc_send_start_game(const char *game_key)
{
    gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
    dtc.has_start_game = true;
    dtc.start_game.value.funcs.encode = enc_str_cb;
    dtc.start_game.value.arg = (void *)(game_key ? game_key : "stadia-wave-01");
    dtc.start_game.has_number = true;
    dtc.start_game.number = 1;
    return send_rpc_request(&dtc, NULL);
}

#include "display_rpc.h"
#include "game_session.h"
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
static bool s_mic_enabled = true;
static uint32_t s_prev_buttons = 0;

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

static esp_err_t send_raw_rpc_response(uint32_t ticket, gotham_rpc_StatusCode code,
                                       const uint8_t *dtc_bytes, size_t dtc_len)
{
    uint8_t *outer = malloc(dtc_len + 96);
    if (!outer) return ESP_ERR_NO_MEM;

    bytes_enc_t pay_ctx = {.data = dtc_bytes, .len = dtc_len};
    gotham_rpc_RpcMessage rpc = gotham_rpc_RpcMessage_init_zero;
    rpc.which_payload = gotham_rpc_RpcMessage_response_tag;
    rpc.payload.response.has_ticket = true;
    rpc.payload.response.ticket = ticket;
    rpc.payload.response.has_code = true;
    rpc.payload.response.code = code;
    rpc.payload.response.error_message.funcs.encode = enc_str_cb;
    rpc.payload.response.error_message.arg = (void *)"";
    if (dtc_bytes && dtc_len > 0) {
        rpc.payload.response.payload.funcs.encode = enc_bytes_cb;
        rpc.payload.response.payload.arg = &pay_ctx;
    }

    pb_ostream_t os = pb_ostream_from_buffer(outer, dtc_len + 96);
    esp_err_t ret = ESP_FAIL;
    if (pb_encode(&os, gotham_rpc_RpcMessage_fields, &rpc)) {
        ret = rtc_endpoint_send_wire(WIRE_ID_DC_RPC_DISPLAY, outer, os.bytes_written);
    }
    free(outer);
    return ret;
}

static esp_err_t send_rpc_response(uint32_t ticket, gotham_rpc_StatusCode code,
                                   const gotham_display_DisplayToController *dtc)
{
    if (!dtc) {
        return send_raw_rpc_response(ticket, code, NULL, 0);
    }
    uint8_t *inner = malloc(256);
    if (!inner) return ESP_ERR_NO_MEM;
    pb_ostream_t is = pb_ostream_from_buffer(inner, 256);
    esp_err_t ret = ESP_FAIL;
    if (pb_encode(&is, gotham_display_DisplayToController_fields, dtc)) {
        ret = send_raw_rpc_response(ticket, code, inner, is.bytes_written);
    }
    free(inner);
    return ret;
}

static bool read_proto_field_hdr(pb_istream_t *is, uint32_t *field_num, uint32_t *wire_type)
{
    uint64_t tag = 0;
    if (!pb_decode_varint(is, &tag)) {
        return false;
    }
    *field_num = (uint32_t)(tag >> 3);
    *wire_type = (uint32_t)(tag & 0x07);
    return true;
}

static bool extract_oneof_submsg(const uint8_t *buf, size_t len,
                                 uint32_t *out_tag, const uint8_t **out_sub, size_t *out_sub_len)
{
    pb_istream_t is = pb_istream_from_buffer(buf, len);
    uint32_t fnum = 0, wtype = 0;
    if (!read_proto_field_hdr(&is, &fnum, &wtype) || wtype != PB_WT_STRING) {
        return false;
    }
    uint64_t slen = 0;
    if (!pb_decode_varint(&is, &slen) || slen > is.bytes_left) {
        return false;
    }
    *out_tag = fnum;
    *out_sub = buf + (len - is.bytes_left);
    *out_sub_len = (size_t)slen;
    return true;
}

static void handle_controller_to_display_req(uint32_t ticket, const uint8_t *buf, size_t len)
{
    uint32_t variant = 0;
    const uint8_t *sub = NULL;
    size_t sub_len = 0;
    if (!extract_oneof_submsg(buf, len, &variant, &sub, &sub_len)) {
        ESP_LOGW(TAG, "CTD req extract failed (len=%zu)", len);
        return;
    }

    if (variant == gotham_display_ControllerToDisplay_get_current_time_tag) {
        gotham_display_GetCurrentTimeRequest req = gotham_display_GetCurrentTimeRequest_init_zero;
        pb_istream_t is = pb_istream_from_buffer(sub, sub_len);
        if (pb_decode(&is, gotham_display_GetCurrentTimeRequest_fields, &req)) {
            gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
            dtc.has_current_time = true;
            dtc.current_time.has_controller_time_us = true;
            dtc.current_time.controller_time_us = req.controller_time_us;
            dtc.current_time.has_display_time_us = true;
            dtc.current_time.display_time_us = 1764300000000000LL + esp_timer_get_time();
            send_rpc_response(ticket, gotham_rpc_StatusCode_OK, &dtc);
        }
    } else if (variant == gotham_display_ControllerToDisplay_keepalive_tag) {
        send_rpc_response(ticket, gotham_rpc_StatusCode_OK, NULL);
    } else if (variant == gotham_display_ControllerToDisplay_disconnect_tag) {
        gotham_display_DisconnectRequest dreq = gotham_display_DisconnectRequest_init_zero;
        pb_istream_t is = pb_istream_from_buffer(sub, sub_len);
        pb_decode(&is, gotham_display_DisconnectRequest_fields, &dreq);
        ESP_LOGW(TAG, "Controller DisconnectRequest (reason=%lu)", (unsigned long)dreq.reason);
        send_rpc_response(ticket, gotham_rpc_StatusCode_OK, NULL);
        s_ready = false;
    } else if (variant == gotham_display_ControllerToDisplay_proxy_request_tag) {
        uint8_t *body = malloc(1024);
        uint8_t *dtc_out = malloc(640);
        if (body && dtc_out) {
            bytes_dec_t bctx = {.buf = body, .cap = 1024, .len = 0};
            gotham_display_ProxyRequest preq = gotham_display_ProxyRequest_init_zero;
            preq.body.funcs.decode = dec_bytes_cb;
            preq.body.arg = &bctx;
            pb_istream_t is = pb_istream_from_buffer(sub, sub_len);
            size_t dtc_len = 0;
            if (pb_decode(&is, gotham_display_ProxyRequest_fields, &preq) &&
                game_session_handle_proxy_request(body, bctx.len, dtc_out, 640, &dtc_len)) {
                send_raw_rpc_response(ticket, gotham_rpc_StatusCode_OK, dtc_out, dtc_len);
            } else {
                send_rpc_response(ticket, gotham_rpc_StatusCode_INTERNAL, NULL);
            }
        } else {
            send_rpc_response(ticket, gotham_rpc_StatusCode_RESOURCE_EXHAUSTED, NULL);
        }
        free(body);
        free(dtc_out);
    } else {
        ESP_LOGD(TAG, "Unhandled CTD request variant=%lu (ticket=%lu)",
                 (unsigned long)variant, (unsigned long)ticket);
        send_rpc_response(ticket, gotham_rpc_StatusCode_UNIMPLEMENTED, NULL);
    }
}

static void handle_controller_to_display_resp(uint32_t ticket, gotham_rpc_StatusCode code,
                                              const uint8_t *buf, size_t len)
{
    if (ticket == s_get_state_ticket) {
        if (code != gotham_rpc_StatusCode_OK) {
            ESP_LOGW(TAG, "GetControllerStateResponse err=%d (retrying)", (int)code);
            return;
        }
        uint32_t variant = 0;
        const uint8_t *sub = NULL;
        size_t sub_len = 0;
        if (!extract_oneof_submsg(buf, len, &variant, &sub, &sub_len) ||
            variant != gotham_display_ControllerToDisplay_controller_state_tag) {
            ESP_LOGW(TAG, "GetControllerStateResponse unexpected variant=%lu", (unsigned long)variant);
            return;
        }

        gotham_display_GetControllerStateResponse *st =
            calloc(1, sizeof(gotham_display_GetControllerStateResponse));
        if (!st) return;

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

        st->device_id.funcs.decode = dec_str_cb;
        st->device_id.arg = &d_dev;
        st->f12.funcs.decode = dec_str_cb;
        st->f12.arg = &d_mfg;
        st->f14.funcs.decode = dec_str_cb;
        st->f14.arg = &d_prod;
        st->f15.funcs.decode = dec_str_cb;
        st->f15.arg = &d_hw;
        st->f16.funcs.decode = dec_str_cb;
        st->f16.arg = &d_fw;
        st->f17.funcs.decode = dec_str_cb;
        st->f17.arg = &d_ser;

        pb_istream_t is = pb_istream_from_buffer(sub, sub_len);
        if (pb_decode(&is, gotham_display_GetControllerStateResponse_fields, st)) {
            s_last_input.headset_plugged = st->f4;
            s_ready = true;
            ESP_LOGI(TAG, ">>> MILESTONE: Display Link READY! <<<");
            ESP_LOGI(TAG, "  HW: mfg='%s', prod='%s', hw='%s', fw='%s', serial='%s', headset=%d",
                     mfg, prod, hw, fw, serial, s_last_input.headset_plugged);
            ESP_LOGI(TAG, "Sending StartGame + AudioMode(2) + SetMic(true)...");
            display_rpc_send_start_game("stadia-wave-01");
            display_rpc_send_set_audio_mode(2, true);
            display_rpc_send_set_microphone(true);
        }
        free(st);
    } else {
        ESP_LOGI(TAG, "RPC Response ticket=%lu, code=%d, len=%zu",
                 (unsigned long)ticket, (int)code, len);
    }
}

static void handle_controller_to_display_notif(const uint8_t *buf, size_t len)
{
    uint32_t variant = 0;
    const uint8_t *sub = NULL;
    size_t sub_len = 0;
    if (!extract_oneof_submsg(buf, len, &variant, &sub, &sub_len)) {
        return;
    }

    pb_istream_t is = pb_istream_from_buffer(sub, sub_len);
    if (variant == gotham_display_ControllerToDisplay_message3_tag) {
        gotham_display_Message3 m = gotham_display_Message3_init_zero;
        if (pb_decode(&is, gotham_display_Message3_fields, &m)) {
            s_last_input.buttons = m.f1;
            s_last_input.lx = (int16_t)m.f2;
            s_last_input.ly = (int16_t)m.f3;
            s_last_input.rx = (int16_t)m.f4;
            s_last_input.ry = (int16_t)m.f5;
            s_last_input.l2 = (uint8_t)m.f6;
            s_last_input.r2 = (uint8_t)m.f7;

            // Assistant button (bit 10) toggles controller microphone (solid white LED when off)
            bool asst_now = (m.f1 & (1U << 10)) != 0;
            bool asst_prev = (s_prev_buttons & (1U << 10)) != 0;
            s_prev_buttons = m.f1;
            if (asst_now && !asst_prev && s_ready) {
                s_mic_enabled = !s_mic_enabled;
                ESP_LOGI(TAG, "Assistant pressed -> Controller Mic %s",
                         s_mic_enabled ? "ON (LED blinking)" : "OFF (LED solid)");
                display_rpc_send_set_microphone(s_mic_enabled);
            }

            if (s_input_cb) {
                s_input_cb(&s_last_input);
            } else {
                ESP_LOGI(TAG, "Input: btn=0x%05lx L=(%d,%d) R=(%d,%d) trig=(%u,%u)",
                         (unsigned long)s_last_input.buttons,
                         s_last_input.lx, s_last_input.ly,
                         s_last_input.rx, s_last_input.ry,
                         s_last_input.l2, s_last_input.r2);
            }
        }
    } else if (variant == gotham_display_ControllerToDisplay_audio_configuration_changed_tag) {
        gotham_display_AudioConfigurationChanged ac = gotham_display_AudioConfigurationChanged_init_zero;
        if (pb_decode(&is, gotham_display_AudioConfigurationChanged_fields, &ac)) {
            s_last_input.headset_plugged = (ac.mode != 0);
            ESP_LOGI(TAG, "Headset jack: %s (mode=%lu)",
                     s_last_input.headset_plugged ? "PLUGGED" : "UNPLUGGED",
                     (unsigned long)ac.mode);
        }
    } else if (variant == gotham_display_ControllerToDisplay_notification10_tag) {
        gotham_display_FloatValue fv = gotham_display_FloatValue_init_zero;
        if (pb_decode(&is, gotham_display_FloatValue_fields, &fv)) {
            s_last_input.battery_level = fv.value;
            ESP_LOGI(TAG, "Battery: %.0f%%", s_last_input.battery_level * 100.0f);
        }
    }
}

static void on_wire_rx(uint16_t msg_id, const uint8_t *payload, size_t len)
{
    if (msg_id != WIRE_ID_DC_RPC_CTRL && msg_id != WIRE_ID_DC_RPC_DISPLAY) {
        ESP_LOGD(TAG, "Non-display WireHeader2 id=0x%04x, len=%zu", msg_id, len);
        return;
    }

    uint32_t rpc_tag = 0;
    const uint8_t *sub = NULL;
    size_t sub_len = 0;
    if (!extract_oneof_submsg(payload, len, &rpc_tag, &sub, &sub_len)) {
        ESP_LOGW(TAG, "RpcMessage extract failed (len=%zu)", len);
        return;
    }

    uint8_t *inner = malloc(1024);
    if (!inner) return;
    bytes_dec_t pay_ctx = {.buf = inner, .cap = 1024, .len = 0};
    pb_istream_t is = pb_istream_from_buffer(sub, sub_len);

    if (rpc_tag == gotham_rpc_RpcMessage_request_tag) {
        gotham_rpc_RpcRequest req = gotham_rpc_RpcRequest_init_zero;
        req.payload.funcs.decode = dec_bytes_cb;
        req.payload.arg = &pay_ctx;
        if (pb_decode(&is, gotham_rpc_RpcRequest_fields, &req)) {
            handle_controller_to_display_req(req.ticket, inner, pay_ctx.len);
        }
    } else if (rpc_tag == gotham_rpc_RpcMessage_response_tag) {
        gotham_rpc_RpcResponse resp = gotham_rpc_RpcResponse_init_zero;
        resp.payload.funcs.decode = dec_bytes_cb;
        resp.payload.arg = &pay_ctx;
        if (pb_decode(&is, gotham_rpc_RpcResponse_fields, &resp)) {
            handle_controller_to_display_resp(resp.ticket, resp.code, inner, pay_ctx.len);
        }
    } else if (rpc_tag == gotham_rpc_RpcMessage_notification_tag) {
        gotham_rpc_RpcNotification notif = gotham_rpc_RpcNotification_init_zero;
        notif.payload.funcs.decode = dec_bytes_cb;
        notif.payload.arg = &pay_ctx;
        if (pb_decode(&is, gotham_rpc_RpcNotification_fields, &notif)) {
            handle_controller_to_display_notif(inner, pay_ctx.len);
        }
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

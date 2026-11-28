#include "session_signaling.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "discovery_api.pb.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/semphr.h"
#include "pb_decode.h"
#include "pb_encode.h"
#include "webrtc_sdp.pb.h"

static const char *TAG = "signaling";

#define EV_GMSG_QUEUED BIT0

// Host candidate priority: (126 << 24) | (65535 << 8) | 255
#define HOST_CANDIDATE_PRIORITY 2130706431U

#define LOCAL_ICE_UFRAG "stadia1"
#define LOCAL_ICE_PWD   "stadiacast_ice_password_123456"
#define REG_TOKEN       "local-reg-token-0001"
#define DISPLAY_ID      "local-stadia-display-0001"
#define DISPLAY_TOKEN   "local-display-token-0001"

typedef struct {
    char *buf;
    size_t cap;
} str_decode_ctx_t;

typedef struct {
    uint8_t *buf;
    size_t cap;
    size_t len;
} bytes_decode_ctx_t;

typedef struct {
    const uint8_t *data;
    size_t len;
} bytes_encode_ctx_t;

static struct {
    SemaphoreHandle_t lock;
    EventGroupHandle_t ev;
    char local_ip[40];
    uint16_t local_port;
    char dtls_fp[128];
    stadia_remote_rtc_info_t remote_rtc;
    bool has_pending_gmsg;
    int64_t pending_gmsg_id;
    uint8_t pending_payload[1024];
    size_t pending_payload_len;
    int64_t gmsg_seq;
    bool discovery_resolved;
} s_sig;

static bool decode_string_cb(pb_istream_t *stream, const pb_field_t *field, void **arg)
{
    (void)field;
    str_decode_ctx_t *ctx = (str_decode_ctx_t *)(*arg);
    size_t to_read = stream->bytes_left;
    if (!ctx || !ctx->buf || ctx->cap == 0) {
        return pb_read(stream, NULL, to_read);
    }
    size_t keep = to_read < ctx->cap - 1 ? to_read : ctx->cap - 1;
    if (keep > 0 && !pb_read(stream, (pb_byte_t *)ctx->buf, keep)) {
        return false;
    }
    ctx->buf[keep] = '\0';
    if (to_read > keep) {
        return pb_read(stream, NULL, to_read - keep);
    }
    return true;
}

static bool decode_bytes_cb(pb_istream_t *stream, const pb_field_t *field, void **arg)
{
    (void)field;
    bytes_decode_ctx_t *ctx = (bytes_decode_ctx_t *)(*arg);
    size_t to_read = stream->bytes_left;
    if (!ctx || !ctx->buf || to_read > ctx->cap) {
        ESP_LOGW(TAG, "bytes field truncated/skipped (len=%zu)", to_read);
        return pb_read(stream, NULL, to_read);
    }
    if (to_read > 0 && !pb_read(stream, (pb_byte_t *)ctx->buf, to_read)) {
        return false;
    }
    ctx->len = to_read;
    return true;
}

static bool encode_string_cb(pb_ostream_t *stream, const pb_field_t *field, void *const *arg)
{
    const char *str = (const char *)(*arg);
    if (!str) {
        return true;
    }
    if (!pb_encode_tag_for_field(stream, field)) {
        return false;
    }
    return pb_encode_string(stream, (const pb_byte_t *)str, strlen(str));
}

static bool encode_bytes_cb(pb_ostream_t *stream, const pb_field_t *field, void *const *arg)
{
    const bytes_encode_ctx_t *ctx = (const bytes_encode_ctx_t *)(*arg);
    if (!ctx || !ctx->data || ctx->len == 0) {
        return true;
    }
    if (!pb_encode_tag_for_field(stream, field)) {
        return false;
    }
    return pb_encode_string(stream, (const pb_byte_t *)ctx->data, ctx->len);
}

void session_signaling_init(void)
{
    memset(&s_sig, 0, sizeof(s_sig));
    s_sig.lock = xSemaphoreCreateMutex();
    s_sig.ev = xEventGroupCreate();
    snprintf(s_sig.local_ip, sizeof(s_sig.local_ip), "192.168.4.1");
    s_sig.local_port = 40000;
    snprintf(s_sig.dtls_fp, sizeof(s_sig.dtls_fp),
             "sha-256 00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:"
             "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF");
    ESP_LOGI(TAG, "Signaling state machine initialized (endpoint=%s:%u)",
             s_sig.local_ip, s_sig.local_port);
}

void session_signaling_set_dtls_fingerprint(const char *sha256_fp)
{
    if (!sha256_fp) return;
    xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    snprintf(s_sig.dtls_fp, sizeof(s_sig.dtls_fp), "%s", sha256_fp);
    xSemaphoreGive(s_sig.lock);
    ESP_LOGI(TAG, "DTLS fingerprint updated: %s", sha256_fp);
}

void session_signaling_set_local_endpoint(const char *ip, uint16_t port)
{
    if (!ip) return;
    xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    snprintf(s_sig.local_ip, sizeof(s_sig.local_ip), "%s", ip);
    s_sig.local_port = port;
    xSemaphoreGive(s_sig.lock);
}

bool session_signaling_get_remote_rtc(stadia_remote_rtc_info_t *out)
{
    if (!out) return false;
    xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    *out = s_sig.remote_rtc;
    xSemaphoreGive(s_sig.lock);
    return out->valid;
}

bool session_signaling_is_discovery_resolved(void)
{
    xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    bool resolved = s_sig.discovery_resolved;
    xSemaphoreGive(s_sig.lock);
    return resolved;
}

static bool process_offer_and_queue_answer(const uint8_t *offer_bundle, size_t offer_len, const char *via)
{
    if (!offer_bundle || offer_len == 0) {
        return false;
    }

    chrome_cloudcast_proto_api_common_WebRtcSdpAndIceCandidates *in_bundle =
        calloc(1, sizeof(chrome_cloudcast_proto_api_common_WebRtcSdpAndIceCandidates));
    chrome_cloudcast_proto_api_common_WebRtcSdpAndIceCandidates *out_bundle =
        calloc(1, sizeof(chrome_cloudcast_proto_api_common_WebRtcSdpAndIceCandidates));
    if (!in_bundle || !out_bundle) {
        free(in_bundle);
        free(out_bundle);
        return false;
    }

    pb_istream_t istream = pb_istream_from_buffer(offer_bundle, offer_len);
    if (!pb_decode(&istream, chrome_cloudcast_proto_api_common_WebRtcSdpAndIceCandidates_fields, in_bundle)) {
        ESP_LOGE(TAG, "Failed to decode sdp_offer_bundle (%s): %s", via, PB_GET_ERROR(&istream));
        free(in_bundle);
        free(out_bundle);
        return false;
    }

    if (!in_bundle->has_sdp ||
        in_bundle->sdp.type != chrome_cloudcast_proto_api_common_WebRtcSdp_Type_SDP_OFFER ||
        !in_bundle->sdp.has_sdp_offer) {
        ESP_LOGW(TAG, "sdp_offer_bundle (%s) missing SDP_OFFER (type=%d)",
                 via, (int)in_bundle->sdp.type);
        free(in_bundle);
        free(out_bundle);
        return false;
    }

    const chrome_cloudcast_proto_api_common_RtcSdpOffer *offer = &in_bundle->sdp.sdp_offer;
    ESP_LOGI(TAG, "Offer (%s): session=%s, ufrag=%s, ssrc=0x%08lx, raw_dtls=%d, delayed_acks=%d, cands=%u",
             via, offer->rtc_session_id, offer->ice_ufrag, (unsigned long)offer->audio_ssrc,
             offer->raw_dtls, offer->delayed_acks, (unsigned)in_bundle->ice_candidate_count);

    for (pb_size_t i = 0; i < in_bundle->ice_candidate_count; i++) {
        if (in_bundle->ice_candidate[i].has_ice_candidate) {
            const chrome_cloudcast_proto_api_common_RtcIceCandidate *c =
                &in_bundle->ice_candidate[i].ice_candidate;
            ESP_LOGI(TAG, "  Remote ICE cand[%u]: %s:%lu",
                     (unsigned)i, c->local_endpoint_ip, (unsigned long)c->local_endpoint_port);
        }
    }

    xSemaphoreTake(s_sig.lock, portMAX_DELAY);

    s_sig.remote_rtc.valid = true;
    snprintf(s_sig.remote_rtc.ice_ufrag, sizeof(s_sig.remote_rtc.ice_ufrag), "%s", offer->ice_ufrag);
    snprintf(s_sig.remote_rtc.ice_password, sizeof(s_sig.remote_rtc.ice_password), "%s", offer->ice_password);
    snprintf(s_sig.remote_rtc.fingerprint, sizeof(s_sig.remote_rtc.fingerprint), "%s", offer->fingerprint);
    s_sig.remote_rtc.audio_ssrc = offer->audio_ssrc;
    s_sig.remote_rtc.raw_dtls = offer->raw_dtls;
    s_sig.remote_rtc.delayed_acks = offer->delayed_acks;
    s_sig.remote_rtc.use_rtp_timestamps = offer->use_rtp_timestamps;
    s_sig.discovery_resolved = false;

    // Build WebRtcSdpAndIceCandidates answer bundle
    out_bundle->has_sdp = true;
    out_bundle->sdp.type = chrome_cloudcast_proto_api_common_WebRtcSdp_Type_SDP_ANSWER;
    out_bundle->sdp.has_sdp_answer = true;

    chrome_cloudcast_proto_api_common_RtcSdpAnswer *ans = &out_bundle->sdp.sdp_answer;
    snprintf(ans->ice_ufrag, sizeof(ans->ice_ufrag), "%s", LOCAL_ICE_UFRAG);
    snprintf(ans->ice_password, sizeof(ans->ice_password), "%s", LOCAL_ICE_PWD);
    snprintf(ans->fingerprint, sizeof(ans->fingerprint), "%s", s_sig.dtls_fp);
    ans->client_type = chrome_cloudcast_proto_api_common_RtcSdpAnswer_ClientType_DAYDREAM;
    ans->use_boreal = false;
    ans->has_audio_format = true;
    ans->audio_format.codec = chrome_cloudcast_proto_api_common_RtcSdpAudioCodec_AUDIO_CODEC_OPUS;
    ans->audio_format.channels = 2;
    ans->audio_format.sample_rate = 48000;
    ans->raw_dtls = offer->raw_dtls;
    ans->delayed_acks = offer->delayed_acks;
    ans->network_type = chrome_cloudcast_proto_api_common_RtcSdpNetworkType_NETWORK_TYPE_WIFI;
    ans->use_rtp_timestamps = offer->use_rtp_timestamps;

    out_bundle->ice_candidate_count = 1;
    out_bundle->ice_candidate[0].type = chrome_cloudcast_proto_api_common_WebRtcSdp_Type_ICE_CANDIDATE;
    out_bundle->ice_candidate[0].has_ice_candidate = true;
    chrome_cloudcast_proto_api_common_RtcIceCandidate *cand = &out_bundle->ice_candidate[0].ice_candidate;
    snprintf(cand->sdp_mid, sizeof(cand->sdp_mid), "0");
    cand->sdp_mline_index = 0;
    snprintf(cand->foundation, sizeof(cand->foundation), "1");
    cand->priority = HOST_CANDIDATE_PRIORITY;
    snprintf(cand->local_endpoint_ip, sizeof(cand->local_endpoint_ip), "%s", s_sig.local_ip);
    cand->local_endpoint_port = s_sig.local_port;

    pb_ostream_t ostream = pb_ostream_from_buffer(s_sig.pending_payload, sizeof(s_sig.pending_payload));
    bool ok = pb_encode(&ostream, chrome_cloudcast_proto_api_common_WebRtcSdpAndIceCandidates_fields, out_bundle);
    if (ok) {
        s_sig.pending_payload_len = ostream.bytes_written;
        s_sig.gmsg_seq++;
        s_sig.pending_gmsg_id = s_sig.gmsg_seq;
        s_sig.has_pending_gmsg = true;
        xEventGroupSetBits(s_sig.ev, EV_GMSG_QUEUED);
        ESP_LOGI(TAG, ">>> Queued SDP Answer bundle for GMsg id=%lld (%zu bytes, %s:%u) <<<",
                 (long long)s_sig.pending_gmsg_id, s_sig.pending_payload_len,
                 s_sig.local_ip, s_sig.local_port);
    } else {
        ESP_LOGE(TAG, "Failed to encode answer bundle: %s", PB_GET_ERROR(&ostream));
    }

    xSemaphoreGive(s_sig.lock);

    free(in_bundle);
    free(out_bundle);
    return ok;
}

bool session_signaling_handle_firmware_check(const uint8_t *req, size_t req_len,
                                             uint8_t *resp, size_t resp_max, size_t *resp_len)
{
    (void)req;
    (void)req_len;
    gotham_discovery_FirmwareVersionCheckResponse msg = gotham_discovery_FirmwareVersionCheckResponse_init_zero;
    msg.has_requires_update = true;
    msg.requires_update = false;

    pb_ostream_t stream = pb_ostream_from_buffer(resp, resp_max);
    if (!pb_encode(&stream, gotham_discovery_FirmwareVersionCheckResponse_fields, &msg)) {
        ESP_LOGE(TAG, "FirmwareVersionCheck encode failed: %s", PB_GET_ERROR(&stream));
        return false;
    }
    *resp_len = stream.bytes_written;
    return true;
}

bool session_signaling_handle_register(const uint8_t *req, size_t req_len,
                                       uint8_t *resp, size_t resp_max, size_t *resp_len)
{
    char device_id[64] = {0};
    char attempt_id[64] = {0};
    uint8_t *offer_buf = malloc(1024);
    if (!offer_buf) return false;

    str_decode_ctx_t dev_ctx = {.buf = device_id, .cap = sizeof(device_id)};
    str_decode_ctx_t att_ctx = {.buf = attempt_id, .cap = sizeof(attempt_id)};
    bytes_decode_ctx_t offer_ctx = {.buf = offer_buf, .cap = 1024, .len = 0};

    gotham_discovery_RegisterControllerRequest msg = gotham_discovery_RegisterControllerRequest_init_zero;
    msg.device_id.funcs.decode = decode_string_cb;
    msg.device_id.arg = &dev_ctx;
    msg.attempt_id.funcs.decode = decode_string_cb;
    msg.attempt_id.arg = &att_ctx;
    msg.discovery.sdp_offer_bundle.funcs.decode = decode_bytes_cb;
    msg.discovery.sdp_offer_bundle.arg = &offer_ctx;

    if (req && req_len > 0) {
        pb_istream_t istream = pb_istream_from_buffer(req, req_len);
        if (!pb_decode(&istream, gotham_discovery_RegisterControllerRequest_fields, &msg)) {
            ESP_LOGW(TAG, "RegisterControllerRequest decode warning: %s", PB_GET_ERROR(&istream));
        }
    }

    ESP_LOGI(TAG, "RegisterController: device_id='%s', attempt='%s', has_discovery=%d, offer_len=%zu",
             device_id, attempt_id, msg.has_discovery, offer_ctx.len);

    if (msg.has_discovery && offer_ctx.len > 0) {
        process_offer_and_queue_answer(offer_buf, offer_ctx.len, "RegisterController");
    }
    free(offer_buf);

    char reg_id[64];
    snprintf(reg_id, sizeof(reg_id), "local-%.40s", device_id[0] ? device_id : "controller");

    gotham_discovery_RegisterControllerResponse out = gotham_discovery_RegisterControllerResponse_init_zero;
    out.registration_id.funcs.encode = encode_string_cb;
    out.registration_id.arg = reg_id;
    out.poll_interval_ms = 1000;
    out.registration_token.funcs.encode = encode_string_cb;
    out.registration_token.arg = (void *)REG_TOKEN;

    if (msg.has_discovery) {
        out.which_result = gotham_discovery_RegisterControllerResponse_display_tag;
        out.result.display.display_id.funcs.encode = encode_string_cb;
        out.result.display.display_id.arg = (void *)DISPLAY_ID;
        out.result.display.display_token.funcs.encode = encode_string_cb;
        out.result.display.display_token.arg = (void *)DISPLAY_TOKEN;
    }

    pb_ostream_t ostream = pb_ostream_from_buffer(resp, resp_max);
    if (!pb_encode(&ostream, gotham_discovery_RegisterControllerResponse_fields, &out)) {
        ESP_LOGE(TAG, "RegisterControllerResponse encode failed: %s", PB_GET_ERROR(&ostream));
        return false;
    }
    *resp_len = ostream.bytes_written;
    return true;
}

bool session_signaling_handle_discover(const uint8_t *req, size_t req_len,
                                       uint8_t *resp, size_t resp_max, size_t *resp_len)
{
    char device_id[64] = {0};
    uint8_t *offer_buf = malloc(1024);
    if (!offer_buf) return false;

    str_decode_ctx_t dev_ctx = {.buf = device_id, .cap = sizeof(device_id)};
    bytes_decode_ctx_t offer_ctx = {.buf = offer_buf, .cap = 1024, .len = 0};

    gotham_discovery_DiscoverDisplayRequest msg = gotham_discovery_DiscoverDisplayRequest_init_zero;
    msg.device_id.funcs.decode = decode_string_cb;
    msg.device_id.arg = &dev_ctx;
    msg.sdp_offer_bundle.funcs.decode = decode_bytes_cb;
    msg.sdp_offer_bundle.arg = &offer_ctx;

    if (req && req_len > 0) {
        pb_istream_t istream = pb_istream_from_buffer(req, req_len);
        if (!pb_decode(&istream, gotham_discovery_DiscoverDisplayRequest_fields, &msg)) {
            ESP_LOGW(TAG, "DiscoverDisplayRequest decode warning: %s", PB_GET_ERROR(&istream));
        }
    }

    ESP_LOGI(TAG, "DiscoverDisplay: device_id='%s', offer_len=%zu", device_id, offer_ctx.len);
    if (offer_ctx.len > 0) {
        process_offer_and_queue_answer(offer_buf, offer_ctx.len, "DiscoverDisplay");
    }
    free(offer_buf);

    gotham_discovery_DiscoverDisplayResponse out = gotham_discovery_DiscoverDisplayResponse_init_zero;
    out.display_id.funcs.encode = encode_string_cb;
    out.display_id.arg = (void *)DISPLAY_ID;
    out.display_token.funcs.encode = encode_string_cb;
    out.display_token.arg = (void *)DISPLAY_TOKEN;

    pb_ostream_t ostream = pb_ostream_from_buffer(resp, resp_max);
    if (!pb_encode(&ostream, gotham_discovery_DiscoverDisplayResponse_fields, &out)) {
        ESP_LOGE(TAG, "DiscoverDisplayResponse encode failed: %s", PB_GET_ERROR(&ostream));
        return false;
    }
    *resp_len = ostream.bytes_written;
    return true;
}

bool session_signaling_handle_gmsg(const uint8_t *req, size_t req_len,
                                   uint8_t *resp, size_t resp_max, size_t *resp_len,
                                   uint32_t hold_ms)
{
    char subscription[96] = {0};
    str_decode_ctx_t sub_ctx = {.buf = subscription, .cap = sizeof(subscription)};

    gotham_discovery_GmsgPollRequest poll = gotham_discovery_GmsgPollRequest_init_zero;
    poll.subscription.funcs.decode = decode_string_cb;
    poll.subscription.arg = &sub_ctx;

    if (req && req_len > 0) {
        pb_istream_t istream = pb_istream_from_buffer(req, req_len);
        if (!pb_decode(&istream, gotham_discovery_GmsgPollRequest_fields, &poll)) {
            ESP_LOGW(TAG, "GmsgPollRequest decode warning: %s", PB_GET_ERROR(&istream));
        }
    }

    bool is_ack = poll.has_last_message && poll.last_message.has_id;

    xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    if (is_ack) {
        ESP_LOGI(TAG, "GMsg poll ACK received: id=%lld (ack=%d)",
                 (long long)poll.last_message.id, poll.ack);
        if (s_sig.has_pending_gmsg && s_sig.pending_gmsg_id == poll.last_message.id) {
            s_sig.has_pending_gmsg = false;
            ESP_LOGI(TAG, ">>> GMsg id=%lld ACKed by controller! <<<",
                     (long long)poll.last_message.id);
        }
    }

    if (!s_sig.has_pending_gmsg && !is_ack && hold_ms > 0) {
        xEventGroupClearBits(s_sig.ev, EV_GMSG_QUEUED);
        xSemaphoreGive(s_sig.lock);
        xEventGroupWaitBits(s_sig.ev, EV_GMSG_QUEUED, pdTRUE, pdFALSE, pdMS_TO_TICKS(hold_ms));
        xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    }

    if (!s_sig.has_pending_gmsg) {
        xSemaphoreGive(s_sig.lock);
        *resp_len = 0;
        return true;
    }

    int64_t msg_id = s_sig.pending_gmsg_id;
    bytes_encode_ctx_t payload_ctx = {
        .data = s_sig.pending_payload,
        .len = s_sig.pending_payload_len,
    };
    xSemaphoreGive(s_sig.lock);

    gotham_discovery_ControllerCommand cmd = gotham_discovery_ControllerCommand_init_zero;
    cmd.payload.funcs.encode = encode_bytes_cb;
    cmd.payload.arg = &payload_ctx;

    pb_extension_t ext = pb_extension_init_zero;
    ext.type = &gotham_discovery_controller_command;
    ext.dest = &cmd;

    gotham_discovery_GmsgEnvelope env = gotham_discovery_GmsgEnvelope_init_zero;
    env.has_message = true;
    env.message.has_id = true;
    env.message.id = msg_id;
    env.message.has_type = true;
    env.message.type = 42;
    env.message.extensions = &ext;

    pb_ostream_t ostream = pb_ostream_from_buffer(resp, resp_max);
    if (!pb_encode(&ostream, gotham_discovery_GmsgEnvelope_fields, &env)) {
        ESP_LOGE(TAG, "GmsgEnvelope encode failed: %s", PB_GET_ERROR(&ostream));
        return false;
    }
    *resp_len = ostream.bytes_written;
    ESP_LOGI(TAG, ">>> Sent GmsgEnvelope id=%lld (%zu bytes) <<<",
             (long long)msg_id, *resp_len);
    return true;
}

bool session_signaling_handle_resolve(const uint8_t *req, size_t req_len,
                                      uint8_t *resp, size_t resp_max, size_t *resp_len)
{
    (void)resp;
    (void)resp_max;
    char device_id[64] = {0};
    char display_id[64] = {0};
    str_decode_ctx_t dev_ctx = {.buf = device_id, .cap = sizeof(device_id)};
    str_decode_ctx_t disp_ctx = {.buf = display_id, .cap = sizeof(display_id)};

    gotham_discovery_ResolveDiscoveryRequest msg = gotham_discovery_ResolveDiscoveryRequest_init_zero;
    msg.device_id.funcs.decode = decode_string_cb;
    msg.device_id.arg = &dev_ctx;
    msg.display_id.funcs.decode = decode_string_cb;
    msg.display_id.arg = &disp_ctx;

    if (req && req_len > 0) {
        pb_istream_t istream = pb_istream_from_buffer(req, req_len);
        pb_decode(&istream, gotham_discovery_ResolveDiscoveryRequest_fields, &msg);
    }

    xSemaphoreTake(s_sig.lock, portMAX_DELAY);
    s_sig.discovery_resolved = true;
    xSemaphoreGive(s_sig.lock);

    ESP_LOGI(TAG, ">>> MILESTONE: ResolveDiscovery received (dev='%s', disp='%s')! <<<",
             device_id, display_id);
    *resp_len = 0;
    return true;
}

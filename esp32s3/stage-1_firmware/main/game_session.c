#include "game_session.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "controller_message.pb.h"
#include "esp_log.h"
#include "esp_random.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "input.pb.h"
#include "lwip/inet.h"
#include "lwip/sockets.h"
#include "mbedtls/error.h"
#include "mbedtls/pk.h"
#include "mbedtls/ssl.h"
#include "mbedtls/x509_crt.h"
#include "pb_decode.h"
#include "pb_encode.h"
#include "psa/crypto.h"
#include "rpc_message.pb.h"
#include "streamer_message.pb.h"
#include "webrtc_sdp.pb.h"

static const char *TAG = "game_sess";

extern const uint8_t gts_fullchain_pem_start[] asm("_binary_gts_fullchain_pem_start");
extern const uint8_t gts_fullchain_pem_end[]   asm("_binary_gts_fullchain_pem_end");
extern const uint8_t gts_privkey_pem_start[]   asm("_binary_gts_privkey_pem_start");
extern const uint8_t gts_privkey_pem_end[]     asm("_binary_gts_privkey_pem_end");

#define STUN_MAGIC          0x2112A442U
#define STUN_BINDING_REQ    0x0001
#define STUN_BINDING_OK     0x0101
#define STUN_ATTR_USERNAME  0x0006
#define STUN_ATTR_MI        0x0008
#define STUN_ATTR_XORMAPPED 0x0020
#define STUN_ATTR_PRIORITY  0x0024
#define STUN_ATTR_FP        0x8028
#define STUN_ATTR_ICE_CTLD  0x8029

#define GAME_ICE_UFRAG      "game1"
#define GAME_ICE_PWD        "stadiacast_game_password_123456"
#define HOST_CAND_PRIORITY  2130706431U

#define WIRE_ID_RTP         0x0066
#define WIRE_ID_GAME_RPC    0xFFFA
#define DTLS_RX_Q_CAP       12
#define GAME_TX_Q_CAP       12
#define GAME_MAX_PAYLOAD    512

typedef struct {
    uint8_t data[1500];
    size_t len;
} game_dgram_t;

typedef struct {
    uint16_t msg_id;
    uint16_t len;
    uint8_t payload[GAME_MAX_PAYLOAD];
} game_tx_item_t;

typedef struct {
    uint32_t int_ms;
    uint32_t fin_ms;
    int64_t start_us;
} game_timer_t;

typedef struct {
    uint8_t *buf;
    size_t cap;
    size_t len;
} bytes_dec_t;

typedef struct {
    const uint8_t *data;
    size_t len;
} bytes_enc_t;

static int s_udp_fd = -1;
static struct sockaddr_in s_peer_addr;
static bool s_has_peer = false;
static volatile bool s_dtls_connected = false;
static bool s_rtp_framed = false;
static uint16_t s_rtp_seq = 1;
static uint32_t s_feedback_seq = 1;

static char s_dtls_fp[128] = {0};
static char s_remote_ufrag[16] = {0};
static char s_remote_pwd[64] = {0};

static game_dgram_t *s_rx_q = NULL;
static int s_rx_head = 0, s_rx_tail = 0, s_rx_count = 0;
static QueueHandle_t s_tx_q = NULL;

static volatile uint16_t s_rumble_left = 0;
static volatile uint16_t s_rumble_right = 0;
static volatile bool s_rumble_dirty = false;
static int64_t s_last_rumble_tx_us = 0;

static bool dec_bytes_cb(pb_istream_t *stream, const pb_field_t *field, void **arg)
{
    (void)field;
    bytes_dec_t *c = (bytes_dec_t *)(*arg);
    size_t n = stream->bytes_left;
    if (!c || !c->buf || n > c->cap) return pb_read(stream, NULL, n);
    if (n > 0 && !pb_read(stream, (pb_byte_t *)c->buf, n)) return false;
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

static bool enc_feedback_series_cb(pb_ostream_t *stream, const pb_field_t *field, void *const *arg)
{
    const chrome_cloudcast_streaming_InputMessageSlim *slim =
        (const chrome_cloudcast_streaming_InputMessageSlim *)(*arg);
    return pb_encode_tag_for_field(stream, field) &&
           pb_encode_submessage(stream, chrome_cloudcast_streaming_InputMessageSlim_fields, slim);
}

static void timer_set(void *ctx, uint32_t int_ms, uint32_t fin_ms)
{
    game_timer_t *t = (game_timer_t *)ctx;
    t->int_ms = int_ms;
    t->fin_ms = fin_ms;
    if (fin_ms != 0) t->start_us = esp_timer_get_time();
}

static int timer_get(void *ctx)
{
    game_timer_t *t = (game_timer_t *)ctx;
    if (t->fin_ms == 0) return -1;
    int64_t el = (esp_timer_get_time() - t->start_us) / 1000;
    if (el >= (int64_t)t->fin_ms) return 2;
    if (el >= (int64_t)t->int_ms) return 1;
    return 0;
}

static int bio_send(void *ctx, const unsigned char *buf, size_t len)
{
    (void)ctx;
    if (!s_has_peer || s_udp_fd < 0) return MBEDTLS_ERR_SSL_WANT_WRITE;
    int r = sendto(s_udp_fd, buf, len, 0, (struct sockaddr *)&s_peer_addr, sizeof(s_peer_addr));
    if (r < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) return MBEDTLS_ERR_SSL_WANT_WRITE;
        return -1;
    }
    return r;
}

static int bio_recv(void *ctx, unsigned char *buf, size_t len)
{
    (void)ctx;
    if (s_rx_count == 0) return MBEDTLS_ERR_SSL_WANT_READ;
    size_t plen = s_rx_q[s_rx_head].len;
    size_t copy = plen < len ? plen : len;
    memcpy(buf, s_rx_q[s_rx_head].data, copy);
    s_rx_head = (s_rx_head + 1) % DTLS_RX_Q_CAP;
    s_rx_count--;
    return (int)copy;
}

static uint32_t ieee_crc32(const uint8_t *buf, size_t len)
{
    uint32_t crc = 0xFFFFFFFFU;
    for (size_t i = 0; i < len; i++) {
        crc ^= buf[i];
        for (int b = 0; b < 8; b++) {
            crc = (crc >> 1) ^ (0xEDB88320U & (uint32_t)(-(int32_t)(crc & 1U)));
        }
    }
    return ~crc;
}

static bool hmac_sha1(const uint8_t *key, size_t key_len, const uint8_t *data, size_t dlen, uint8_t out[20])
{
    psa_key_attributes_t attr = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_usage_flags(&attr, PSA_KEY_USAGE_SIGN_MESSAGE);
    psa_set_key_algorithm(&attr, PSA_ALG_HMAC(PSA_ALG_SHA_1));
    psa_set_key_type(&attr, PSA_KEY_TYPE_HMAC);
    psa_key_id_t kid = 0;
    if (psa_import_key(&attr, key, key_len, &kid) != PSA_SUCCESS) return false;
    size_t mlen = 0;
    psa_status_t st = psa_mac_compute(kid, PSA_ALG_HMAC(PSA_ALG_SHA_1), data, dlen, out, 20, &mlen);
    psa_destroy_key(kid);
    return st == PSA_SUCCESS && mlen == 20;
}

static size_t stun_attr(uint8_t *b, size_t off, uint16_t t, const void *v, uint16_t l)
{
    b[off] = (t >> 8) & 0xFF; b[off + 1] = t & 0xFF;
    b[off + 2] = (l >> 8) & 0xFF; b[off + 3] = l & 0xFF;
    if (l > 0 && v) memcpy(b + off + 4, v, l);
    size_t pad = (4 - (l % 4)) % 4;
    if (pad > 0) memset(b + off + 4 + l, 0, pad);
    return off + 4 + l + pad;
}

static void send_stun_req(const struct sockaddr_in *to)
{
    if (!s_remote_pwd[0]) return;
    uint8_t req[128] = {0};
    req[0] = (STUN_BINDING_REQ >> 8) & 0xFF; req[1] = STUN_BINDING_REQ & 0xFF;
    req[4] = (STUN_MAGIC >> 24) & 0xFF; req[5] = (STUN_MAGIC >> 16) & 0xFF;
    req[6] = (STUN_MAGIC >> 8) & 0xFF; req[7] = STUN_MAGIC & 0xFF;
    esp_fill_random(req + 8, 12);

    size_t off = 20;
    char uname[32];
    int ulen = snprintf(uname, sizeof(uname), "%s:%s", s_remote_ufrag, GAME_ICE_UFRAG);
    off = stun_attr(req, off, STUN_ATTR_USERNAME, uname, (uint16_t)ulen);
    uint8_t prio[4] = {(HOST_CAND_PRIORITY >> 24) & 0xFF, (HOST_CAND_PRIORITY >> 16) & 0xFF,
                       (HOST_CAND_PRIORITY >> 8) & 0xFF, HOST_CAND_PRIORITY & 0xFF};
    off = stun_attr(req, off, STUN_ATTR_PRIORITY, prio, 4);
    uint8_t tie[8] = {0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99};
    off = stun_attr(req, off, STUN_ATTR_ICE_CTLD, tie, 8);

    uint16_t lmi = (uint16_t)(off - 20 + 24);
    req[2] = (lmi >> 8) & 0xFF; req[3] = lmi & 0xFF;
    uint8_t mac[20] = {0};
    hmac_sha1((const uint8_t *)s_remote_pwd, strlen(s_remote_pwd), req, off, mac);
    off = stun_attr(req, off, STUN_ATTR_MI, mac, 20);

    uint16_t ltot = (uint16_t)(off - 20 + 8);
    req[2] = (ltot >> 8) & 0xFF; req[3] = ltot & 0xFF;
    uint32_t crc = ieee_crc32(req, off) ^ 0x5354554EU;
    uint8_t fp[4] = {(crc >> 24) & 0xFF, (crc >> 16) & 0xFF, (crc >> 8) & 0xFF, crc & 0xFF};
    off = stun_attr(req, off, STUN_ATTR_FP, fp, 4);

    sendto(s_udp_fd, req, off, 0, (const struct sockaddr *)to, sizeof(*to));
}

static void handle_stun(const uint8_t *pkt, size_t len, const struct sockaddr_in *from)
{
    if (len < 20) return;
    uint16_t mtype = (uint16_t)((pkt[0] << 8) | pkt[1]);
    uint32_t magic = (uint32_t)((pkt[4] << 24) | (pkt[5] << 16) | (pkt[6] << 8) | pkt[7]);
    if (magic != STUN_MAGIC || mtype != STUN_BINDING_REQ) return;

    if (!s_has_peer || s_peer_addr.sin_addr.s_addr != from->sin_addr.s_addr ||
        s_peer_addr.sin_port != from->sin_port) {
        s_peer_addr = *from;
        s_has_peer = true;
        ESP_LOGI(TAG, "Game ICE peer bound: %s:%u", inet_ntoa(from->sin_addr), ntohs(from->sin_port));
    }

    uint8_t resp[64] = {0};
    resp[0] = (STUN_BINDING_OK >> 8) & 0xFF; resp[1] = STUN_BINDING_OK & 0xFF;
    resp[2] = 0; resp[3] = 36;
    memcpy(resp + 4, pkt + 4, 16);

    size_t off = 20;
    uint16_t xp = ntohs(from->sin_port) ^ (uint16_t)(STUN_MAGIC >> 16);
    uint32_t xip = ntohl(from->sin_addr.s_addr) ^ STUN_MAGIC;
    uint8_t xa[8] = {0, 1, (xp >> 8) & 0xFF, xp & 0xFF,
                     (xip >> 24) & 0xFF, (xip >> 16) & 0xFF, (xip >> 8) & 0xFF, xip & 0xFF};
    off = stun_attr(resp, off, STUN_ATTR_XORMAPPED, xa, 8);

    uint8_t mac[20] = {0};
    hmac_sha1((const uint8_t *)GAME_ICE_PWD, strlen(GAME_ICE_PWD), resp, off, mac);
    off = stun_attr(resp, off, STUN_ATTR_MI, mac, 20);

    resp[2] = 0; resp[3] = 44;
    uint32_t crc = ieee_crc32(resp, off) ^ 0x5354554EU;
    uint8_t fp[4] = {(crc >> 24) & 0xFF, (crc >> 16) & 0xFF, (crc >> 8) & 0xFF, crc & 0xFF};
    off = stun_attr(resp, off, STUN_ATTR_FP, fp, 4);

    sendto(s_udp_fd, resp, off, 0, (const struct sockaddr *)from, sizeof(*from));
}

static void queue_game_tx(uint16_t msg_id, const uint8_t *payload, size_t len)
{
    if (!s_tx_q || len > GAME_MAX_PAYLOAD) return;
    game_tx_item_t *it = malloc(sizeof(game_tx_item_t));
    if (!it) return;
    it->msg_id = msg_id;
    it->len = (uint16_t)len;
    if (payload && len > 0) memcpy(it->payload, payload, len);
    xQueueSend(s_tx_q, it, 0);
    free(it);
}

static void send_rumble_packet(uint16_t left, uint16_t right)
{
    chrome_cloudcast_streaming_InputMessageSlim slim = chrome_cloudcast_streaming_InputMessageSlim_init_zero;
    slim.has_relative_time_us = true;
    slim.relative_time_us = 0;
    slim.has_input_feedback_message_v2 = true;
    slim.input_feedback_message_v2.has_gamepad_vibration = true;
    slim.input_feedback_message_v2.gamepad_vibration.has_id = true;
    slim.input_feedback_message_v2.gamepad_vibration.id = 0;
    slim.input_feedback_message_v2.gamepad_vibration.has_left_motor_speed = true;
    slim.input_feedback_message_v2.gamepad_vibration.left_motor_speed = left;
    slim.input_feedback_message_v2.gamepad_vibration.has_right_motor_speed = true;
    slim.input_feedback_message_v2.gamepad_vibration.right_motor_speed = right;

    chrome_cloudcast_streaming_InputProtocolMessage msg = chrome_cloudcast_streaming_InputProtocolMessage_init_zero;
    msg.has_input_message_series = true;
    msg.input_message_series.has_sequence_number = true;
    msg.input_message_series.sequence_number = s_feedback_seq++;
    msg.input_message_series.has_base_timestamp_us = true;
    msg.input_message_series.base_timestamp_us = (uint64_t)esp_timer_get_time();
    msg.input_message_series.messages.funcs.encode = enc_feedback_series_cb;
    msg.input_message_series.messages.arg = &slim;

    uint8_t pb_buf[128];
    pb_ostream_t os = pb_ostream_from_buffer(pb_buf, sizeof(pb_buf));
    if (!pb_encode(&os, chrome_cloudcast_streaming_InputProtocolMessage_fields, &msg)) {
        ESP_LOGE(TAG, "InputFeedback encode failed: %s", PB_GET_ERROR(&os));
        return;
    }

    if (s_rtp_framed) {
        uint8_t rtp_buf[140];
        rtp_buf[0] = 0x80;
        rtp_buf[1] = 96;
        rtp_buf[2] = (s_rtp_seq >> 8) & 0xFF;
        rtp_buf[3] = s_rtp_seq & 0xFF;
        s_rtp_seq++;
        memset(rtp_buf + 4, 0, 8);
        memcpy(rtp_buf + 12, pb_buf, os.bytes_written);
        queue_game_tx(WIRE_ID_RTP, rtp_buf, 12 + os.bytes_written);
    } else {
        queue_game_tx(WIRE_ID_RTP, pb_buf, os.bytes_written);
    }
}

static void handle_game_wire(const uint8_t *rec, size_t len)
{
    if (len < 8) return;
    uint16_t msg_id = (uint16_t)(rec[2] | (rec[3] << 8));
    const uint8_t *pay = rec + 8;
    size_t plen = len - 8;

    if (msg_id == WIRE_ID_RTP && plen > 0) {
        if (plen >= 12 && (pay[0] & 0xC0) == 0x80) {
            s_rtp_framed = true;
            pay += 12;
            plen -= 12;
        }
        chrome_cloudcast_streaming_InputProtocolMessage in_msg =
            chrome_cloudcast_streaming_InputProtocolMessage_init_zero;
        pb_istream_t is = pb_istream_from_buffer(pay, plen);
        if (pb_decode(&is, chrome_cloudcast_streaming_InputProtocolMessage_fields, &in_msg) &&
            in_msg.has_input_message_series && in_msg.input_message_series.has_sequence_number) {
            chrome_cloudcast_streaming_InputProtocolMessage ack =
                chrome_cloudcast_streaming_InputProtocolMessage_init_zero;
            ack.has_ack_sequence_number = true;
            ack.ack_sequence_number = in_msg.input_message_series.sequence_number;
            uint8_t abuf[16];
            pb_ostream_t os = pb_ostream_from_buffer(abuf, sizeof(abuf));
            if (pb_encode(&os, chrome_cloudcast_streaming_InputProtocolMessage_fields, &ack)) {
                if (s_rtp_framed) {
                    uint8_t rtp[32] = {0x80, 96};
                    rtp[2] = (s_rtp_seq >> 8) & 0xFF; rtp[3] = s_rtp_seq & 0xFF; s_rtp_seq++;
                    memcpy(rtp + 12, abuf, os.bytes_written);
                    queue_game_tx(WIRE_ID_RTP, rtp, 12 + os.bytes_written);
                } else {
                    queue_game_tx(WIRE_ID_RTP, abuf, os.bytes_written);
                }
            }
        }
    } else if (msg_id == WIRE_ID_GAME_RPC || msg_id == 0x0074) {
        // Reply empty OK to any Game RPC request
        if (plen > 0 && pay[0] == 0x0a) {
            uint8_t *sub = NULL;
            pb_istream_t is = pb_istream_from_buffer(pay + 1, plen - 1);
            uint64_t slen = 0;
            if (pb_decode_varint(&is, &slen) && slen <= is.bytes_left) {
                sub = (uint8_t *)(pay + plen - is.bytes_left);
                gotham_rpc_RpcRequest req = gotham_rpc_RpcRequest_init_zero;
                pb_istream_t ris = pb_istream_from_buffer(sub, (size_t)slen);
                if (pb_decode(&ris, gotham_rpc_RpcRequest_fields, &req)) {
                    gotham_rpc_RpcMessage resp = gotham_rpc_RpcMessage_init_zero;
                    resp.which_payload = gotham_rpc_RpcMessage_response_tag;
                    resp.payload.response.has_ticket = true;
                    resp.payload.response.ticket = req.ticket;
                    resp.payload.response.has_code = true;
                    resp.payload.response.code = gotham_rpc_StatusCode_OK;
                    resp.payload.response.error_message.funcs.encode = enc_str_cb;
                    resp.payload.response.error_message.arg = (void *)"";
                    uint8_t rbuf[32];
                    pb_ostream_t os = pb_ostream_from_buffer(rbuf, sizeof(rbuf));
                    if (pb_encode(&os, gotham_rpc_RpcMessage_fields, &resp)) {
                        queue_game_tx(msg_id, rbuf, os.bytes_written);
                    }
                }
            }
        }
    }
}

static void game_session_task(void *arg)
{
    (void)arg;
    static const mbedtls_ssl_srtp_profile srtp_profiles[] = {
        MBEDTLS_TLS_SRTP_AES128_CM_HMAC_SHA1_80,
        MBEDTLS_TLS_SRTP_UNSET,
    };

    mbedtls_x509_crt srvcert;
    mbedtls_pk_context pkey;
    mbedtls_ssl_config conf;
    mbedtls_ssl_context ssl;

    mbedtls_x509_crt_init(&srvcert);
    mbedtls_pk_init(&pkey);
    mbedtls_ssl_config_init(&conf);
    mbedtls_ssl_init(&ssl);

    mbedtls_x509_crt_parse(&srvcert, gts_fullchain_pem_start,
                           (size_t)(gts_fullchain_pem_end - gts_fullchain_pem_start));
    uint8_t digest[32] = {0};
    size_t dlen = 0;
    if (psa_hash_compute(PSA_ALG_SHA_256, srvcert.raw.p, srvcert.raw.len,
                         digest, sizeof(digest), &dlen) == PSA_SUCCESS) {
        int pos = snprintf(s_dtls_fp, sizeof(s_dtls_fp), "sha-256 ");
        for (size_t i = 0; i < 32 && pos < (int)sizeof(s_dtls_fp) - 3; i++) {
            pos += snprintf(s_dtls_fp + pos, sizeof(s_dtls_fp) - (size_t)pos, i == 0 ? "%02X" : ":%02X", digest[i]);
        }
    }

    mbedtls_pk_parse_key(&pkey, gts_privkey_pem_start,
                         (size_t)(gts_privkey_pem_end - gts_privkey_pem_start), NULL, 0);

    ESP_ERROR_CHECK(mbedtls_ssl_config_defaults(&conf, MBEDTLS_SSL_IS_SERVER,
                                                MBEDTLS_SSL_TRANSPORT_DATAGRAM,
                                                MBEDTLS_SSL_PRESET_DEFAULT));
    mbedtls_ssl_conf_min_tls_version(&conf, MBEDTLS_SSL_VERSION_TLS1_2);
    mbedtls_ssl_conf_max_tls_version(&conf, MBEDTLS_SSL_VERSION_TLS1_2);
    mbedtls_ssl_conf_ca_chain(&conf, srvcert.next, NULL);
    ESP_ERROR_CHECK(mbedtls_ssl_conf_own_cert(&conf, &srvcert, &pkey));
    mbedtls_ssl_conf_dtls_cookies(&conf, NULL, NULL, NULL);
    mbedtls_ssl_conf_dtls_srtp_protection_profiles(&conf, srtp_profiles);
    ESP_ERROR_CHECK(mbedtls_ssl_setup(&ssl, &conf));
    mbedtls_ssl_set_mtu(&ssl, 1200);

    game_timer_t timer = {0};
    mbedtls_ssl_set_bio(&ssl, NULL, bio_send, bio_recv, NULL);
    mbedtls_ssl_set_timer_cb(&ssl, &timer, timer_set, timer_get);

    s_udp_fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    struct timeval tv = {.tv_sec = 0, .tv_usec = 20000};
    setsockopt(s_udp_fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    struct sockaddr_in bind_addr = {
        .sin_family = AF_INET,
        .sin_port = htons(GAME_LOCAL_PORT),
        .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    bind(s_udp_fd, (struct sockaddr *)&bind_addr, sizeof(bind_addr));
    ESP_LOGI(TAG, "GameSession UDP listening on :%d", GAME_LOCAL_PORT);

    uint8_t *rx_buf = malloc(1600);
    uint8_t *tx_buf = malloc(GAME_MAX_PAYLOAD + 8);
    game_tx_item_t *tx_item = malloc(sizeof(game_tx_item_t));
    int64_t last_stun_us = 0;
    bool dtls_started = false;

    while (1) {
        struct sockaddr_in from;
        socklen_t flen = sizeof(from);
        int n = recvfrom(s_udp_fd, rx_buf, 1600, 0, (struct sockaddr *)&from, &flen);
        if (n > 0) {
            uint8_t b0 = rx_buf[0];
            if (b0 < 2) {
                handle_stun(rx_buf, (size_t)n, &from);
            } else if (b0 >= 20 && b0 <= 63) {
                if (!s_has_peer) {
                    s_peer_addr = from;
                    s_has_peer = true;
                }
                if (s_dtls_connected && n > 13 && rx_buf[0] == 22 && rx_buf[13] == 1) {
                    s_dtls_connected = false;
                    s_rx_head = s_rx_tail = s_rx_count = 0;
                    mbedtls_ssl_session_reset(&ssl);
                }
                dtls_started = true;
                if (s_rx_count < DTLS_RX_Q_CAP && n <= (int)sizeof(s_rx_q[0].data)) {
                    memcpy(s_rx_q[s_rx_tail].data, rx_buf, (size_t)n);
                    s_rx_q[s_rx_tail].len = (size_t)n;
                    s_rx_tail = (s_rx_tail + 1) % DTLS_RX_Q_CAP;
                    s_rx_count++;
                }
            }
        }

        int64_t now_us = esp_timer_get_time();
        if (s_has_peer && !s_dtls_connected && (now_us - last_stun_us >= 1000000)) {
            last_stun_us = now_us;
            send_stun_req(&s_peer_addr);
        }

        if (s_has_peer && !s_dtls_connected && dtls_started) {
            int hs = mbedtls_ssl_handshake(&ssl);
            if (hs == 0) {
                s_dtls_connected = true;
                ESP_LOGI(TAG, ">>> MILESTONE: GameSession DTLS Connected (Rumble Ready)! <<<");
            } else if (hs != MBEDTLS_ERR_SSL_WANT_READ && hs != MBEDTLS_ERR_SSL_WANT_WRITE) {
                ESP_LOGW(TAG, "Game DTLS handshake err: -0x%04x", -hs);
                s_rx_head = s_rx_tail = s_rx_count = 0;
                dtls_started = false;
                mbedtls_ssl_session_reset(&ssl);
            }
        }

        if (s_dtls_connected) {
            while (1) {
                int r = mbedtls_ssl_read(&ssl, rx_buf, 1600);
                if (r > 0) {
                    handle_game_wire(rx_buf, (size_t)r);
                } else if (r == MBEDTLS_ERR_SSL_WANT_READ || r == MBEDTLS_ERR_SSL_WANT_WRITE) {
                    break;
                } else {
                    s_dtls_connected = false;
                    s_rx_head = s_rx_tail = s_rx_count = 0;
                    dtls_started = false;
                    mbedtls_ssl_session_reset(&ssl);
                    break;
                }
            }

            if (s_rumble_dirty || ((s_rumble_left > 0 || s_rumble_right > 0) &&
                                   (now_us - s_last_rumble_tx_us >= 2000000))) {
                s_rumble_dirty = false;
                s_last_rumble_tx_us = now_us;
                send_rumble_packet(s_rumble_left, s_rumble_right);
            }

            while (s_dtls_connected && xQueueReceive(s_tx_q, tx_item, 0) == pdTRUE) {
                uint32_t total = 8 + tx_item->len;
                tx_buf[0] = 0; tx_buf[1] = 0;
                tx_buf[2] = tx_item->msg_id & 0xFF;
                tx_buf[3] = (tx_item->msg_id >> 8) & 0xFF;
                tx_buf[4] = total & 0xFF;
                tx_buf[5] = (total >> 8) & 0xFF;
                tx_buf[6] = (total >> 16) & 0xFF;
                tx_buf[7] = 0;
                if (tx_item->len > 0) memcpy(tx_buf + 8, tx_item->payload, tx_item->len);
                mbedtls_ssl_write(&ssl, tx_buf, total);
            }
        }
    }
}

esp_err_t game_session_start(void)
{
    s_rx_q = calloc(DTLS_RX_Q_CAP, sizeof(game_dgram_t));
    s_tx_q = xQueueCreate(GAME_TX_Q_CAP, sizeof(game_tx_item_t));
    if (!s_rx_q || !s_tx_q) return ESP_ERR_NO_MEM;
    xTaskCreatePinnedToCore(game_session_task, "game_sess", 8192, NULL, 5, NULL, 0);
    return ESP_OK;
}

bool game_session_is_connected(void)
{
    return s_dtls_connected;
}

void game_session_set_rumble(uint8_t left_8bit, uint8_t right_8bit)
{
    s_rumble_left = (uint16_t)left_8bit * 257U;
    s_rumble_right = (uint16_t)right_8bit * 257U;
    s_rumble_dirty = true;
}

bool game_session_handle_proxy_request(const uint8_t *inner_rpc_buf, size_t inner_rpc_len,
                                       uint8_t *out_dtc_buf, size_t out_dtc_max, size_t *out_dtc_len)
{
    // inner_rpc_buf is RpcMessage { request { ticket, payload: StreamerRequest } }
    if (!inner_rpc_buf || inner_rpc_len < 2 || inner_rpc_buf[0] != 0x0a) return false;

    pb_istream_t is = pb_istream_from_buffer(inner_rpc_buf + 1, inner_rpc_len - 1);
    uint64_t rlen = 0;
    if (!pb_decode_varint(&is, &rlen) || rlen > is.bytes_left) return false;
    const uint8_t *req_sub = inner_rpc_buf + inner_rpc_len - is.bytes_left;

    uint8_t *sr_buf = malloc(1024);
    uint8_t *offer_buf = malloc(1024);
    if (!sr_buf || !offer_buf) {
        free(sr_buf); free(offer_buf);
        return false;
    }

    bytes_dec_t sr_ctx = {.buf = sr_buf, .cap = 1024, .len = 0};
    gotham_rpc_RpcRequest inner_req = gotham_rpc_RpcRequest_init_zero;
    inner_req.payload.funcs.decode = dec_bytes_cb;
    inner_req.payload.arg = &sr_ctx;

    pb_istream_t ris = pb_istream_from_buffer(req_sub, (size_t)rlen);
    if (!pb_decode(&ris, gotham_rpc_RpcRequest_fields, &inner_req)) {
        free(sr_buf); free(offer_buf);
        return false;
    }

    bytes_dec_t off_ctx = {.buf = offer_buf, .cap = 1024, .len = 0};
    gotham_streamer_StreamerRequest sreq = gotham_streamer_StreamerRequest_init_zero;
    sreq.initialization.offer.funcs.decode = dec_bytes_cb;
    sreq.initialization.offer.arg = &off_ctx;

    pb_istream_t sis = pb_istream_from_buffer(sr_buf, sr_ctx.len);
    pb_decode(&sis, gotham_streamer_StreamerRequest_fields, &sreq);
    free(sr_buf);

    chrome_cloudcast_proto_api_common_RtcSdpOffer *offer =
        calloc(1, sizeof(chrome_cloudcast_proto_api_common_RtcSdpOffer));
    chrome_cloudcast_proto_api_common_RtcSdpAnswer *ans =
        calloc(1, sizeof(chrome_cloudcast_proto_api_common_RtcSdpAnswer));
    if (!offer || !ans) {
        free(offer_buf); free(offer); free(ans);
        return false;
    }

    if (off_ctx.len > 0) {
        pb_istream_t ois = pb_istream_from_buffer(offer_buf, off_ctx.len);
        pb_decode(&ois, chrome_cloudcast_proto_api_common_RtcSdpOffer_fields, offer);
    }
    free(offer_buf);

    snprintf(s_remote_ufrag, sizeof(s_remote_ufrag), "%s", offer->ice_ufrag);
    snprintf(s_remote_pwd, sizeof(s_remote_pwd), "%s", offer->ice_password);
    ESP_LOGI(TAG, "Game ProxyRequest: inner_ticket=%lu, ufrag=%s, raw_dtls=%d",
             (unsigned long)inner_req.ticket, s_remote_ufrag, offer->raw_dtls);

    snprintf(ans->ice_ufrag, sizeof(ans->ice_ufrag), "%s", GAME_ICE_UFRAG);
    snprintf(ans->ice_password, sizeof(ans->ice_password), "%s", GAME_ICE_PWD);
    snprintf(ans->fingerprint, sizeof(ans->fingerprint), "%s", s_dtls_fp);
    ans->client_type = chrome_cloudcast_proto_api_common_RtcSdpAnswer_ClientType_DAYDREAM;
    ans->raw_dtls = offer->raw_dtls;
    ans->delayed_acks = offer->delayed_acks;
    ans->network_type = chrome_cloudcast_proto_api_common_RtcSdpNetworkType_NETWORK_TYPE_WIFI;
    ans->use_rtp_timestamps = offer->use_rtp_timestamps;
    free(offer);

    chrome_cloudcast_proto_api_common_RtcIceCandidate cand =
        chrome_cloudcast_proto_api_common_RtcIceCandidate_init_zero;
    snprintf(cand.sdp_mid, sizeof(cand.sdp_mid), "0");
    cand.sdp_mline_index = 0;
    snprintf(cand.foundation, sizeof(cand.foundation), "1");
    cand.priority = HOST_CAND_PRIORITY;
    snprintf(cand.local_endpoint_ip, sizeof(cand.local_endpoint_ip), "192.168.4.1");
    cand.local_endpoint_port = GAME_LOCAL_PORT;

    uint8_t ans_buf[256], cand_buf[96], sr_resp_buf[400], inner_resp_buf[450];
    pb_ostream_t aos = pb_ostream_from_buffer(ans_buf, sizeof(ans_buf));
    pb_encode(&aos, chrome_cloudcast_proto_api_common_RtcSdpAnswer_fields, ans);
    free(ans);

    pb_ostream_t cos = pb_ostream_from_buffer(cand_buf, sizeof(cand_buf));
    pb_encode(&cos, chrome_cloudcast_proto_api_common_RtcIceCandidate_fields, &cand);

    bytes_enc_t ans_enc = {.data = ans_buf, .len = aos.bytes_written};
    bytes_enc_t cand_enc = {.data = cand_buf, .len = cos.bytes_written};

    gotham_streamer_StreamerResponse sresp = gotham_streamer_StreamerResponse_init_zero;
    sresp.has_initialization = true;
    sresp.initialization.answer.funcs.encode = enc_bytes_cb;
    sresp.initialization.answer.arg = &ans_enc;
    sresp.initialization.candidate.funcs.encode = enc_bytes_cb;
    sresp.initialization.candidate.arg = &cand_enc;

    pb_ostream_t sros = pb_ostream_from_buffer(sr_resp_buf, sizeof(sr_resp_buf));
    pb_encode(&sros, gotham_streamer_StreamerResponse_fields, &sresp);

    bytes_enc_t sr_enc = {.data = sr_resp_buf, .len = sros.bytes_written};
    gotham_rpc_RpcMessage inner_rpc_resp = gotham_rpc_RpcMessage_init_zero;
    inner_rpc_resp.which_payload = gotham_rpc_RpcMessage_response_tag;
    inner_rpc_resp.payload.response.has_ticket = true;
    inner_rpc_resp.payload.response.ticket = inner_req.ticket;
    inner_rpc_resp.payload.response.has_code = true;
    inner_rpc_resp.payload.response.code = gotham_rpc_StatusCode_OK;
    inner_rpc_resp.payload.response.error_message.funcs.encode = enc_str_cb;
    inner_rpc_resp.payload.response.error_message.arg = (void *)"";
    inner_rpc_resp.payload.response.payload.funcs.encode = enc_bytes_cb;
    inner_rpc_resp.payload.response.payload.arg = &sr_enc;

    pb_ostream_t iros = pb_ostream_from_buffer(inner_resp_buf, sizeof(inner_resp_buf));
    pb_encode(&iros, gotham_rpc_RpcMessage_fields, &inner_rpc_resp);

    bytes_enc_t dtc_enc = {.data = inner_resp_buf, .len = iros.bytes_written};
    gotham_display_DisplayToController dtc = gotham_display_DisplayToController_init_zero;
    dtc.has_proxy_response = true;
    dtc.proxy_response.data.funcs.encode = enc_bytes_cb;
    dtc.proxy_response.data.arg = &dtc_enc;

    pb_ostream_t dtcos = pb_ostream_from_buffer(out_dtc_buf, out_dtc_max);
    if (!pb_encode(&dtcos, gotham_display_DisplayToController_fields, &dtc)) {
        return false;
    }
    *out_dtc_len = dtcos.bytes_written;
    return true;
}

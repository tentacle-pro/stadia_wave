#include "dtls_srtp_test.h"

#include <stdint.h>
#include <string.h>

#include "esp_log.h"
#include "esp_timer.h"
#include "mbedtls/pk.h"
#include "mbedtls/ssl.h"
#include "mbedtls/x509_crt.h"
#include "psa/crypto.h"

static const char *TAG = "dtls_srtp";

extern const uint8_t gts_fullchain_pem_start[] asm("_binary_gts_fullchain_pem_start");
extern const uint8_t gts_fullchain_pem_end[]   asm("_binary_gts_fullchain_pem_end");
extern const uint8_t gts_privkey_pem_start[]   asm("_binary_gts_privkey_pem_start");
extern const uint8_t gts_privkey_pem_end[]     asm("_binary_gts_privkey_pem_end");

// Simple in-memory datagram ring pipe for loopback DTLS handshake self-test
typedef struct {
    uint8_t pkts[8][1500];
    size_t lens[8];
    int head;
    int tail;
    int count;
} dgram_queue_t;

typedef struct {
    dgram_queue_t *rx;
    dgram_queue_t *tx;
} mem_bio_t;

typedef struct {
    uint32_t int_ms;
    uint32_t fin_ms;
    int64_t start_us;
} dtls_timer_t;

typedef struct {
    uint8_t material[60];
    bool ready;
} srtp_export_t;

static void timer_set(void *ctx, uint32_t int_ms, uint32_t fin_ms)
{
    dtls_timer_t *t = (dtls_timer_t *)ctx;
    t->int_ms = int_ms;
    t->fin_ms = fin_ms;
    if (fin_ms != 0) {
        t->start_us = esp_timer_get_time();
    }
}

static int timer_get(void *ctx)
{
    dtls_timer_t *t = (dtls_timer_t *)ctx;
    if (t->fin_ms == 0) {
        return -1;
    }
    int64_t elapsed_ms = (esp_timer_get_time() - t->start_us) / 1000;
    if (elapsed_ms >= (int64_t)t->fin_ms) {
        return 2;
    }
    if (elapsed_ms >= (int64_t)t->int_ms) {
        return 1;
    }
    return 0;
}

static int mem_send(void *ctx, const unsigned char *buf, size_t len)
{
    mem_bio_t *bio = (mem_bio_t *)ctx;
    if (bio->tx->count >= 8 || len > sizeof(bio->tx->pkts[0])) {
        return MBEDTLS_ERR_SSL_WANT_WRITE;
    }
    memcpy(bio->tx->pkts[bio->tx->tail], buf, len);
    bio->tx->lens[bio->tx->tail] = len;
    bio->tx->tail = (bio->tx->tail + 1) % 8;
    bio->tx->count++;
    return (int)len;
}

static int mem_recv(void *ctx, unsigned char *buf, size_t len)
{
    mem_bio_t *bio = (mem_bio_t *)ctx;
    if (bio->rx->count == 0) {
        return MBEDTLS_ERR_SSL_WANT_READ;
    }
    size_t plen = bio->rx->lens[bio->rx->head];
    size_t copy = plen < len ? plen : len;
    memcpy(buf, bio->rx->pkts[bio->rx->head], copy);
    bio->rx->head = (bio->rx->head + 1) % 8;
    bio->rx->count--;
    return (int)copy;
}

static void export_keys_cb(void *p_expkey, mbedtls_ssl_key_export_type type,
                           const unsigned char *secret, size_t secret_len,
                           const unsigned char client_random[32],
                           const unsigned char server_random[32],
                           mbedtls_tls_prf_types tls_prf_type)
{
    (void)type;
    srtp_export_t *exp = (srtp_export_t *)p_expkey;
    uint8_t randbytes[64];
    memcpy(randbytes, client_random, 32);
    memcpy(randbytes + 32, server_random, 32);

    int ret = mbedtls_ssl_tls_prf(tls_prf_type, secret, secret_len,
                                  "EXTRACTOR-dtls_srtp", randbytes, sizeof(randbytes),
                                  exp->material, sizeof(exp->material));
    if (ret == 0) {
        exp->ready = true;
    } else {
        ESP_LOGE(TAG, "mbedtls_ssl_tls_prf failed: -0x%04x", -ret);
    }
}

esp_err_t dtls_srtp_run_selftest(void)
{
    ESP_LOGI(TAG, "Running DTLS 1.2 + use_srtp (EXTRACTOR-dtls_srtp) loopback self-test...");

    static const mbedtls_ssl_srtp_profile srtp_profiles[] = {
        MBEDTLS_TLS_SRTP_AES128_CM_HMAC_SHA1_80,
        MBEDTLS_TLS_SRTP_UNSET,
    };

    mbedtls_x509_crt srvcert;
    mbedtls_pk_context pkey;
    mbedtls_ssl_config srv_conf, cli_conf;
    mbedtls_ssl_context srv_ssl, cli_ssl;

    mbedtls_x509_crt_init(&srvcert);
    mbedtls_pk_init(&pkey);
    mbedtls_ssl_config_init(&srv_conf);
    mbedtls_ssl_config_init(&cli_conf);
    mbedtls_ssl_init(&srv_ssl);
    mbedtls_ssl_init(&cli_ssl);

    dgram_queue_t *q_c2s = NULL;
    dgram_queue_t *q_s2c = NULL;
    int ret = mbedtls_x509_crt_parse(&srvcert, gts_fullchain_pem_start,
                                     (size_t)(gts_fullchain_pem_end - gts_fullchain_pem_start));
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_x509_crt_parse failed: -0x%04x", -ret);
        goto fail;
    }
    ret = mbedtls_pk_parse_key(&pkey, gts_privkey_pem_start,
                               (size_t)(gts_privkey_pem_end - gts_privkey_pem_start), NULL, 0);
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_pk_parse_key failed: -0x%04x", -ret);
        goto fail;
    }

    q_c2s = calloc(1, sizeof(dgram_queue_t));
    q_s2c = calloc(1, sizeof(dgram_queue_t));
    if (!q_c2s || !q_s2c) {
        ESP_LOGE(TAG, "Failed to allocate dgram queues");
        goto fail;
    }

    // Server config (DTLS + SRTP, disable cookie verification for stateless loopback test)
    mbedtls_ssl_config_defaults(&srv_conf, MBEDTLS_SSL_IS_SERVER,
                                MBEDTLS_SSL_TRANSPORT_DATAGRAM, MBEDTLS_SSL_PRESET_DEFAULT);
    mbedtls_ssl_conf_ca_chain(&srv_conf, srvcert.next, NULL);
    mbedtls_ssl_conf_own_cert(&srv_conf, &srvcert, &pkey);
    mbedtls_ssl_conf_dtls_cookies(&srv_conf, NULL, NULL, NULL);
    mbedtls_ssl_conf_dtls_srtp_protection_profiles(&srv_conf, srtp_profiles);

    // Client config (DTLS + SRTP, optional authmode none for loopback)
    mbedtls_ssl_config_defaults(&cli_conf, MBEDTLS_SSL_IS_CLIENT,
                                MBEDTLS_SSL_TRANSPORT_DATAGRAM, MBEDTLS_SSL_PRESET_DEFAULT);
    mbedtls_ssl_conf_authmode(&cli_conf, MBEDTLS_SSL_VERIFY_NONE);
    mbedtls_ssl_conf_dtls_srtp_protection_profiles(&cli_conf, srtp_profiles);

    mbedtls_ssl_setup(&srv_ssl, &srv_conf);
    mbedtls_ssl_setup(&cli_ssl, &cli_conf);

    mem_bio_t srv_bio = {.rx = q_c2s, .tx = q_s2c};
    mem_bio_t cli_bio = {.rx = q_s2c, .tx = q_c2s};
    dtls_timer_t srv_timer = {0}, cli_timer = {0};
    srtp_export_t srv_exp = {0}, cli_exp = {0};

    mbedtls_ssl_set_bio(&srv_ssl, &srv_bio, mem_send, mem_recv, NULL);
    mbedtls_ssl_set_bio(&cli_ssl, &cli_bio, mem_send, mem_recv, NULL);
    mbedtls_ssl_set_timer_cb(&srv_ssl, &srv_timer, timer_set, timer_get);
    mbedtls_ssl_set_timer_cb(&cli_ssl, &cli_timer, timer_set, timer_get);
    mbedtls_ssl_set_export_keys_cb(&srv_ssl, export_keys_cb, &srv_exp);
    mbedtls_ssl_set_export_keys_cb(&cli_ssl, export_keys_cb, &cli_exp);

    int r_cli = MBEDTLS_ERR_SSL_WANT_READ;
    int r_srv = MBEDTLS_ERR_SSL_WANT_READ;
    for (int step = 0; step < 64; step++) {
        if (r_cli == MBEDTLS_ERR_SSL_WANT_READ || r_cli == MBEDTLS_ERR_SSL_WANT_WRITE) {
            r_cli = mbedtls_ssl_handshake(&cli_ssl);
        }
        if (r_srv == MBEDTLS_ERR_SSL_WANT_READ || r_srv == MBEDTLS_ERR_SSL_WANT_WRITE) {
            r_srv = mbedtls_ssl_handshake(&srv_ssl);
        }
        if (r_cli == 0 && r_srv == 0) {
            break;
        }
        if ((r_cli < 0 && r_cli != MBEDTLS_ERR_SSL_WANT_READ && r_cli != MBEDTLS_ERR_SSL_WANT_WRITE) ||
            (r_srv < 0 && r_srv != MBEDTLS_ERR_SSL_WANT_READ && r_srv != MBEDTLS_ERR_SSL_WANT_WRITE)) {
            ESP_LOGE(TAG, "DTLS handshake failed: cli=-0x%04x, srv=-0x%04x", -r_cli, -r_srv);
            goto fail;
        }
    }

    if (r_cli != 0 || r_srv != 0 || !srv_exp.ready || !cli_exp.ready) {
        ESP_LOGE(TAG, "DTLS-SRTP incomplete (cli=%d, srv=%d, exp=%d/%d)",
                 r_cli, r_srv, srv_exp.ready, cli_exp.ready);
        goto fail;
    }

    if (memcmp(srv_exp.material, cli_exp.material, 60) != 0) {
        ESP_LOGE(TAG, "Exported SRTP keying material mismatch!");
        goto fail;
    }

    mbedtls_dtls_srtp_info srtp_info;
    mbedtls_ssl_get_dtls_srtp_negotiation_result(&srv_ssl, &srtp_info);
    ESP_LOGI(TAG, ">>> DTLS-SRTP Self-Test PASSED! Profile=%d, 60B key[0..3]=%02x%02x%02x%02x <<<",
             (int)srtp_info.MBEDTLS_PRIVATE(chosen_dtls_srtp_profile),
             srv_exp.material[0], srv_exp.material[1], srv_exp.material[2], srv_exp.material[3]);

    free(q_c2s);
    free(q_s2c);
    mbedtls_ssl_free(&srv_ssl);
    mbedtls_ssl_free(&cli_ssl);
    mbedtls_ssl_config_free(&srv_conf);
    mbedtls_ssl_config_free(&cli_conf);
    mbedtls_pk_free(&pkey);
    mbedtls_x509_crt_free(&srvcert);
    return ESP_OK;

fail:
    free(q_c2s);
    free(q_s2c);
    mbedtls_ssl_free(&srv_ssl);
    mbedtls_ssl_free(&cli_ssl);
    mbedtls_ssl_config_free(&srv_conf);
    mbedtls_ssl_config_free(&cli_conf);
    mbedtls_pk_free(&pkey);
    mbedtls_x509_crt_free(&srvcert);
    return ESP_FAIL;
}

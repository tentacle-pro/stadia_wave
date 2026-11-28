#include "https_server.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lwip/sockets.h"
#include "mbedtls/error.h"
#include "mbedtls/pk.h"
#include "mbedtls/ssl.h"
#include "mbedtls/x509_crt.h"

static const char *TAG = "https_sni";

extern const uint8_t gts_fullchain_pem_start[] asm("_binary_gts_fullchain_pem_start");
extern const uint8_t gts_fullchain_pem_end[]   asm("_binary_gts_fullchain_pem_end");
extern const uint8_t gts_privkey_pem_start[]   asm("_binary_gts_privkey_pem_start");
extern const uint8_t gts_privkey_pem_end[]     asm("_binary_gts_privkey_pem_end");

typedef struct {
    char sni[128];
    bool blocked;
} conn_sni_ctx_t;

static int bio_send(void *ctx, const unsigned char *buf, size_t len)
{
    int fd = *(int *)ctx;
    int ret = send(fd, buf, len, 0);
    if (ret < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) {
            return MBEDTLS_ERR_SSL_WANT_WRITE;
        }
        return -1;
    }
    return ret;
}

static int bio_recv(void *ctx, unsigned char *buf, size_t len)
{
    int fd = *(int *)ctx;
    int ret = recv(fd, buf, len, 0);
    if (ret < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) {
            return MBEDTLS_ERR_SSL_WANT_READ;
        }
        return -1;
    }
    return ret;
}

static int sni_callback(void *p_info, mbedtls_ssl_context *ssl,
                        const unsigned char *name, size_t name_len)
{
    (void)p_info;
    conn_sni_ctx_t *sni_ctx = (conn_sni_ctx_t *)mbedtls_ssl_get_user_data_p(ssl);
    if (sni_ctx && name && name_len > 0) {
        size_t copy = name_len < sizeof(sni_ctx->sni) - 1 ? name_len : sizeof(sni_ctx->sni) - 1;
        memcpy(sni_ctx->sni, name, copy);
        sni_ctx->sni[copy] = '\0';

        if (strcmp(sni_ctx->sni, "play.googleapis.com") == 0) {
            sni_ctx->blocked = true;
            ESP_LOGW(TAG, ">>> SNI BLOCKED (Clearcut): %s -> sending handshake_failure alert <<<",
                     sni_ctx->sni);
            mbedtls_ssl_send_alert_message(ssl, MBEDTLS_SSL_ALERT_LEVEL_FATAL,
                                           MBEDTLS_SSL_ALERT_MSG_HANDSHAKE_FAILURE);
            return -1;
        }
        ESP_LOGI(TAG, "TLS SNI accepted: %s", sni_ctx->sni);
    }
    return 0;
}

static void send_http_response(mbedtls_ssl_context *ssl, int status, const char *status_text,
                               const char *ctype, const void *body, size_t body_len, bool close_conn)
{
    char hdr[512];
    int hlen = snprintf(hdr, sizeof(hdr),
                        "HTTP/1.1 %d %s\r\n"
                        "Content-Type: %s\r\n"
                        "Content-Length: %u\r\n"
                        "Date: Sat, 28 Nov 2026 12:00:00 GMT\r\n"
                        "Connection: %s\r\n\r\n",
                        status, status_text, ctype, (unsigned)body_len,
                        close_conn ? "close" : "keep-alive");
    mbedtls_ssl_write(ssl, (const unsigned char *)hdr, (size_t)hlen);
    if (body && body_len > 0) {
        mbedtls_ssl_write(ssl, (const unsigned char *)body, body_len);
    }
}

static void handle_http_requests(mbedtls_ssl_context *ssl, const char *sni)
{
    const size_t req_buf_sz = 16384;
    char *req_buf = malloc(req_buf_sz);
    if (!req_buf) {
        return;
    }
    while (1) {
        int total = 0;
        char *hdr_end = NULL;
        while (total < (int)req_buf_sz - 1) {
            int r = mbedtls_ssl_read(ssl, (unsigned char *)req_buf + total,
                                     req_buf_sz - 1 - (size_t)total);
            if (r == MBEDTLS_ERR_SSL_WANT_READ || r == MBEDTLS_ERR_SSL_WANT_WRITE) {
                continue;
            }
            if (r <= 0) {
                ESP_LOGI(TAG, "HTTPS conn (%s) closed/timeout: r=%d (-0x%04x), total=%d",
                         sni, r, r < 0 ? -r : 0, total);
                free(req_buf);
                return;
            }
            total += r;
            req_buf[total] = '\0';
            hdr_end = strstr(req_buf, "\r\n\r\n");
            if (hdr_end != NULL) {
                break;
            }
        }
        if (!hdr_end) {
            ESP_LOGW(TAG, "HTTPS header incomplete/too large (%s): total=%d", sni, total);
            free(req_buf);
            return;
        }

        // Parse method and path
        char method[16] = {0};
        char path[256] = {0};
        sscanf(req_buf, "%15s %255s", method, path);

        // Read remaining body if Content-Length is present
        int content_len = 0;
        char *cl = strcasestr(req_buf, "Content-Length:");
        if (cl && cl < hdr_end) {
            content_len = atoi(cl + 15);
        }
        int body_already = total - (int)(hdr_end + 4 - req_buf);
        int remaining = content_len - body_already;
        while (remaining > 0) {
            uint8_t drain[256];
            int to_read = remaining < (int)sizeof(drain) ? remaining : (int)sizeof(drain);
            int r = mbedtls_ssl_read(ssl, drain, (size_t)to_read);
            if (r <= 0) {
                if (r == MBEDTLS_ERR_SSL_WANT_READ) continue;
                free(req_buf);
                return;
            }
            remaining -= r;
        }

        ESP_LOGI(TAG, "HTTPS %s %s (SNI=%s, body=%d bytes)", method, path, sni, content_len);

        if (strncmp(path, "/generate_204", 13) == 0) {
            send_http_response(ssl, 204, "No Content", "text/plain", NULL, 0, false);
        } else if (strncmp(path, "/cast/chromecast/device/stadiaconfig", 36) == 0) {
            send_http_response(ssl, 200, "OK", "application/json", "{}", 2, false);
        } else if (strncmp(path, "/service/update2", 16) == 0) {
            static const char omaha_resp[] =
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>"
                "<response protocol=\"3.0\" server=\"prod\">"
                "<app appid=\"{00000000-0000-0000-0000-000000000000}\" status=\"ok\">"
                "<updatecheck status=\"noupdate\"/>"
                "</app></response>";
            send_http_response(ssl, 200, "OK", "application/xml", omaha_resp, strlen(omaha_resp), false);
        } else if (strncmp(path, "/v1:FirmwareVersionCheck", 24) == 0) {
            // Empty FirmwareVersionCheckResponse (requires_update = false)
            send_http_response(ssl, 200, "OK", "application/x-protobuf", NULL, 0, false);
        } else if (strncmp(path, "/v1:RegisterController", 22) == 0) {
            ESP_LOGI(TAG, ">>> MILESTONE: Controller reached RegisterController over GTS TLS! <<<");
            send_http_response(ssl, 200, "OK", "application/x-protobuf", NULL, 0, false);
        } else {
            send_http_response(ssl, 200, "OK", "application/x-protobuf", NULL, 0, false);
        }
    }
}

static void https_server_task(void *arg)
{
    (void)arg;
    mbedtls_x509_crt srvcert;
    mbedtls_pk_context pkey;
    mbedtls_ssl_config conf;
    mbedtls_ssl_context ssl;

    mbedtls_x509_crt_init(&srvcert);
    mbedtls_pk_init(&pkey);
    mbedtls_ssl_config_init(&conf);

    int ret = mbedtls_x509_crt_parse(&srvcert, gts_fullchain_pem_start,
                                     (size_t)(gts_fullchain_pem_end - gts_fullchain_pem_start));
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_x509_crt_parse failed: -0x%04x", -ret);
        vTaskDelete(NULL);
        return;
    }

    ret = mbedtls_pk_parse_key(&pkey, gts_privkey_pem_start,
                               (size_t)(gts_privkey_pem_end - gts_privkey_pem_start),
                               NULL, 0);
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_pk_parse_key failed: -0x%04x", -ret);
        vTaskDelete(NULL);
        return;
    }

    ESP_ERROR_CHECK(mbedtls_ssl_config_defaults(&conf, MBEDTLS_SSL_IS_SERVER,
                                                MBEDTLS_SSL_TRANSPORT_STREAM,
                                                MBEDTLS_SSL_PRESET_DEFAULT));
    mbedtls_ssl_conf_ca_chain(&conf, srvcert.next, NULL);
    ESP_ERROR_CHECK(mbedtls_ssl_conf_own_cert(&conf, &srvcert, &pkey));
    mbedtls_ssl_conf_sni(&conf, sni_callback, NULL);

    int listen_fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    int opt = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(443),
        .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0 || listen(listen_fd, 4) < 0) {
        ESP_LOGE(TAG, "HTTPS bind/listen :443 failed");
        close(listen_fd);
        vTaskDelete(NULL);
        return;
    }
    ESP_LOGI(TAG, "HTTPS Server (GTS Chain + SNI Clearcut Block) listening on :443");

    while (1) {
        struct sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        int client_fd = accept(listen_fd, (struct sockaddr *)&peer, &peer_len);
        if (client_fd < 0) {
            continue;
        }

        struct timeval tv = {.tv_sec = 5, .tv_usec = 0};
        setsockopt(client_fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

        mbedtls_ssl_init(&ssl);
        mbedtls_ssl_setup(&ssl, &conf);

        conn_sni_ctx_t sni_ctx = {.sni = "unknown", .blocked = false};
        mbedtls_ssl_set_user_data_p(&ssl, &sni_ctx);
        mbedtls_ssl_set_bio(&ssl, &client_fd, bio_send, bio_recv, NULL);

        while ((ret = mbedtls_ssl_handshake(&ssl)) != 0) {
            if (ret != MBEDTLS_ERR_SSL_WANT_READ && ret != MBEDTLS_ERR_SSL_WANT_WRITE) {
                if (!sni_ctx.blocked) {
                    ESP_LOGW(TAG, "TLS handshake failed (SNI=%s): -0x%04x", sni_ctx.sni, -ret);
                }
                break;
            }
        }

        if (ret == 0) {
            handle_http_requests(&ssl, sni_ctx.sni);
            mbedtls_ssl_close_notify(&ssl);
        }

        mbedtls_ssl_free(&ssl);
        close(client_fd);
    }
}

esp_err_t https_server_start(void)
{
    xTaskCreatePinnedToCore(https_server_task, "https_srv", 16384, NULL, 5, NULL, 0);
    return ESP_OK;
}

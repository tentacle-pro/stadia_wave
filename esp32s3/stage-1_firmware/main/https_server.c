#include "https_server.h"
#include "session_signaling.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

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

typedef struct {
    int client_fd;
    mbedtls_ssl_config *conf;
} conn_task_arg_t;

static mbedtls_x509_crt s_srvcert;
static mbedtls_pk_context s_pkey;
static mbedtls_ssl_config s_conf;

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
            ESP_LOGW(TAG, "SNI blocked (Clearcut): %s -> handshake_failure", sni_ctx->sni);
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

    size_t sent = 0;
    while (sent < (size_t)hlen) {
        int w = mbedtls_ssl_write(ssl, (const unsigned char *)hdr + sent, (size_t)hlen - sent);
        if (w == MBEDTLS_ERR_SSL_WANT_READ || w == MBEDTLS_ERR_SSL_WANT_WRITE) continue;
        if (w <= 0) return;
        sent += (size_t)w;
    }

    sent = 0;
    while (body && sent < body_len) {
        int w = mbedtls_ssl_write(ssl, (const unsigned char *)body + sent, body_len - sent);
        if (w == MBEDTLS_ERR_SSL_WANT_READ || w == MBEDTLS_ERR_SSL_WANT_WRITE) continue;
        if (w <= 0) return;
        sent += (size_t)w;
    }
}

static void handle_http_requests(mbedtls_ssl_context *ssl, const char *sni)
{
    const size_t req_buf_sz = 16384;
    const size_t resp_buf_sz = 2048;
    char *req_buf = malloc(req_buf_sz);
    uint8_t *resp_buf = malloc(resp_buf_sz);
    if (!req_buf || !resp_buf) {
        free(req_buf);
        free(resp_buf);
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
                ESP_LOGI(TAG, "HTTPS conn (%s) closed: r=%d (-0x%04x)",
                         sni, r, r < 0 ? -r : 0);
                free(req_buf);
                free(resp_buf);
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
            free(resp_buf);
            return;
        }

        char method[16] = {0};
        char path[256] = {0};
        sscanf(req_buf, "%15s %255s", method, path);

        int content_len = 0;
        char *cl = strcasestr(req_buf, "Content-Length:");
        if (cl && cl < hdr_end) {
            content_len = atoi(cl + 15);
        }

        int hdr_bytes = (int)(hdr_end + 4 - req_buf);
        int body_already = total - hdr_bytes;
        if (content_len < 0) content_len = 0;

        uint8_t *body_ptr = (uint8_t *)(hdr_end + 4);
        uint8_t *heap_body = NULL;

        if (content_len > 0) {
            if (hdr_bytes + content_len <= (int)req_buf_sz) {
                while (total - hdr_bytes < content_len) {
                    int r = mbedtls_ssl_read(ssl, (unsigned char *)req_buf + total,
                                             (size_t)(hdr_bytes + content_len - total));
                    if (r == MBEDTLS_ERR_SSL_WANT_READ || r == MBEDTLS_ERR_SSL_WANT_WRITE) continue;
                    if (r <= 0) {
                        free(req_buf);
                        free(resp_buf);
                        return;
                    }
                    total += r;
                }
            } else {
                heap_body = malloc((size_t)content_len);
                if (!heap_body) {
                    free(req_buf);
                    free(resp_buf);
                    return;
                }
                int copied = body_already < content_len ? body_already : content_len;
                memcpy(heap_body, body_ptr, (size_t)copied);
                while (copied < content_len) {
                    int r = mbedtls_ssl_read(ssl, heap_body + copied, (size_t)(content_len - copied));
                    if (r == MBEDTLS_ERR_SSL_WANT_READ || r == MBEDTLS_ERR_SSL_WANT_WRITE) continue;
                    if (r <= 0) {
                        free(heap_body);
                        free(req_buf);
                        free(resp_buf);
                        return;
                    }
                    copied += r;
                }
                body_ptr = heap_body;
            }
        }

        ESP_LOGI(TAG, "HTTPS %s %s (SNI=%s, body=%dB)", method, path, sni, content_len);

        size_t resp_len = 0;
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
            session_signaling_handle_firmware_check(body_ptr, (size_t)content_len,
                                                    resp_buf, resp_buf_sz, &resp_len);
            send_http_response(ssl, 200, "OK", "application/x-protobuf", resp_buf, resp_len, false);
        } else if (strncmp(path, "/v1:RegisterController", 22) == 0) {
            session_signaling_handle_register(body_ptr, (size_t)content_len,
                                              resp_buf, resp_buf_sz, &resp_len);
            send_http_response(ssl, 200, "OK", "application/x-protobuf", resp_buf, resp_len, false);
        } else if (strncmp(path, "/v1:DiscoverDisplay", 19) == 0) {
            session_signaling_handle_discover(body_ptr, (size_t)content_len,
                                              resp_buf, resp_buf_sz, &resp_len);
            send_http_response(ssl, 200, "OK", "application/x-protobuf", resp_buf, resp_len, false);
        } else if (strncmp(path, "/gmsg", 5) == 0 || strncmp(sni, "cloudcast-gmsg", 14) == 0) {
            session_signaling_handle_gmsg(body_ptr, (size_t)content_len,
                                          resp_buf, resp_buf_sz, &resp_len, 10000);
            send_http_response(ssl, 200, "OK", "application/x-protobuf", resp_buf, resp_len, false);
        } else if (strncmp(path, "/v1:ResolveDiscovery", 20) == 0) {
            session_signaling_handle_resolve(body_ptr, (size_t)content_len,
                                             resp_buf, resp_buf_sz, &resp_len);
            send_http_response(ssl, 200, "OK", "application/x-protobuf", resp_buf, resp_len, false);
        } else {
            send_http_response(ssl, 200, "OK", "application/x-protobuf", NULL, 0, false);
        }

        free(heap_body);
    }
}

static void https_conn_task(void *arg)
{
    conn_task_arg_t *carg = (conn_task_arg_t *)arg;
    int client_fd = carg->client_fd;
    mbedtls_ssl_config *conf = carg->conf;
    free(carg);

    struct timeval tv = {.tv_sec = 30, .tv_usec = 0};
    setsockopt(client_fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    mbedtls_ssl_context ssl;
    mbedtls_ssl_init(&ssl);
    mbedtls_ssl_setup(&ssl, conf);

    conn_sni_ctx_t sni_ctx = {.sni = "unknown", .blocked = false};
    mbedtls_ssl_set_user_data_p(&ssl, &sni_ctx);
    mbedtls_ssl_set_bio(&ssl, &client_fd, bio_send, bio_recv, NULL);

    int ret;
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
    vTaskDelete(NULL);
}

static void https_accept_task(void *arg)
{
    (void)arg;
    mbedtls_x509_crt_init(&s_srvcert);
    mbedtls_pk_init(&s_pkey);
    mbedtls_ssl_config_init(&s_conf);

    int ret = mbedtls_x509_crt_parse(&s_srvcert, gts_fullchain_pem_start,
                                     (size_t)(gts_fullchain_pem_end - gts_fullchain_pem_start));
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_x509_crt_parse failed: -0x%04x", -ret);
        vTaskDelete(NULL);
        return;
    }

    ret = mbedtls_pk_parse_key(&s_pkey, gts_privkey_pem_start,
                               (size_t)(gts_privkey_pem_end - gts_privkey_pem_start),
                               NULL, 0);
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_pk_parse_key failed: -0x%04x", -ret);
        vTaskDelete(NULL);
        return;
    }

    ESP_ERROR_CHECK(mbedtls_ssl_config_defaults(&s_conf, MBEDTLS_SSL_IS_SERVER,
                                                MBEDTLS_SSL_TRANSPORT_STREAM,
                                                MBEDTLS_SSL_PRESET_DEFAULT));
    mbedtls_ssl_conf_ca_chain(&s_conf, s_srvcert.next, NULL);
    ESP_ERROR_CHECK(mbedtls_ssl_conf_own_cert(&s_conf, &s_srvcert, &s_pkey));
    mbedtls_ssl_conf_sni(&s_conf, sni_callback, NULL);

    int listen_fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    int opt = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(443),
        .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0 || listen(listen_fd, 6) < 0) {
        ESP_LOGE(TAG, "HTTPS bind/listen :443 failed");
        close(listen_fd);
        vTaskDelete(NULL);
        return;
    }
    ESP_LOGI(TAG, "HTTPS Server (GTS Chain + Signaling + Clearcut Block) listening on :443");

    while (1) {
        struct sockaddr_in peer;
        socklen_t peer_len = sizeof(peer);
        int client_fd = accept(listen_fd, (struct sockaddr *)&peer, &peer_len);
        if (client_fd < 0) {
            continue;
        }

        conn_task_arg_t *carg = malloc(sizeof(conn_task_arg_t));
        if (!carg) {
            close(client_fd);
            continue;
        }
        carg->client_fd = client_fd;
        carg->conf = &s_conf;

        if (xTaskCreatePinnedToCore(https_conn_task, "https_conn", 8192, carg, 5, NULL, 0) != pdPASS) {
            ESP_LOGE(TAG, "Failed to spawn https_conn task");
            free(carg);
            close(client_fd);
        }
    }
}

esp_err_t https_server_start(void)
{
    xTaskCreatePinnedToCore(https_accept_task, "https_acc", 4096, NULL, 5, NULL, 0);
    return ESP_OK;
}

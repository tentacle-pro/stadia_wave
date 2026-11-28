#include "dns_sntp.h"

#include <stdint.h>
#include <string.h>
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lwip/sockets.h"

static const char *TAG = "dns_sntp";

// 192.168.4.1 in network byte order
#define SINK_IP_BYTES {192, 168, 4, 1}

// 2026-11-28 00:00:00 UTC in NTP epoch (1900-01-01 + 70y + 1900->2026)
// Unix 1795824000 + 2208988800 = 4004812800
#define BASE_NTP_SEC 4004812800UL

static void parse_qname(const uint8_t *pkt, size_t len, size_t *offset, char *out, size_t out_sz)
{
    size_t pos = *offset;
    size_t out_pos = 0;
    while (pos < len) {
        uint8_t label_len = pkt[pos++];
        if (label_len == 0) {
            break;
        }
        if ((label_len & 0xC0) == 0xC0 || pos + label_len > len) {
            break;
        }
        if (out_pos > 0 && out_pos + 1 < out_sz) {
            out[out_pos++] = '.';
        }
        size_t copy = (out_pos + label_len < out_sz) ? label_len : (out_sz - 1 - out_pos);
        if (copy > 0) {
            memcpy(out + out_pos, pkt + pos, copy);
            out_pos += copy;
        }
        pos += label_len;
    }
    out[out_pos] = '\0';
    *offset = pos;
}

static void dns_task(void *arg)
{
    (void)arg;
    int sock = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (sock < 0) {
        ESP_LOGE(TAG, "DNS socket failed");
        vTaskDelete(NULL);
        return;
    }

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(53),
        .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    if (bind(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        ESP_LOGE(TAG, "DNS bind :53 failed");
        close(sock);
        vTaskDelete(NULL);
        return;
    }
    ESP_LOGI(TAG, "DNS Sink listening on UDP :53 -> 192.168.4.1");

    uint8_t buf[512];
    struct sockaddr_in peer;
    socklen_t peer_len;

    while (1) {
        peer_len = sizeof(peer);
        int n = recvfrom(sock, buf, sizeof(buf), 0, (struct sockaddr *)&peer, &peer_len);
        if (n < 12) {
            continue;
        }

        uint16_t qdcount = ((uint16_t)buf[4] << 8) | buf[5];
        if (qdcount == 0) {
            continue;
        }

        size_t q_end = 12;
        char qname[128] = {0};
        parse_qname(buf, (size_t)n, &q_end, qname, sizeof(qname));
        if (q_end + 4 > (size_t)n) {
            continue;
        }

        uint16_t qtype = ((uint16_t)buf[q_end] << 8) | buf[q_end + 1];
        q_end += 4; // skip QTYPE + QCLASS

        ESP_LOGI(TAG, "DNS Query: %s (type=%u)", qname, qtype);

        // Build response in-place
        buf[2] = (uint8_t)(0x81 | (buf[2] & 0x01)); // QR=1, AA=0, copy RD
        buf[3] = 0x80; // RA=1, RCODE=0 (NOERROR)
        buf[4] = 0; buf[5] = 1; // QDCOUNT=1
        buf[8] = 0; buf[9] = 0; // NSCOUNT=0
        buf[10] = 0; buf[11] = 0; // ARCOUNT=0

        size_t resp_len = q_end;
        if (qtype == 1 && resp_len + 16 <= sizeof(buf)) { // A record
            buf[6] = 0; buf[7] = 1; // ANCOUNT=1
            const uint8_t ip[4] = SINK_IP_BYTES;
            uint8_t ans[16] = {
                0xC0, 0x0C,             // Name pointer to offset 12
                0x00, 0x01,             // TYPE A
                0x00, 0x01,             // CLASS IN
                0x00, 0x00, 0x00, 0x3C, // TTL 60s
                0x00, 0x04,             // RDLENGTH 4
                ip[0], ip[1], ip[2], ip[3]
            };
            memcpy(buf + resp_len, ans, sizeof(ans));
            resp_len += sizeof(ans);
        } else {
            // AAAA / other: empty NOERROR answer to force IPv4
            buf[6] = 0; buf[7] = 0;
        }

        sendto(sock, buf, resp_len, 0, (struct sockaddr *)&peer, peer_len);
    }
}

static void sntp_task(void *arg)
{
    (void)arg;
    int sock = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (sock < 0) {
        ESP_LOGE(TAG, "SNTP socket failed");
        vTaskDelete(NULL);
        return;
    }

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(123),
        .sin_addr.s_addr = htonl(INADDR_ANY),
    };
    if (bind(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        ESP_LOGE(TAG, "SNTP bind :123 failed");
        close(sock);
        vTaskDelete(NULL);
        return;
    }
    ESP_LOGI(TAG, "SNTP Server listening on UDP :123");

    uint8_t pkt[48];
    struct sockaddr_in peer;
    socklen_t peer_len;

    while (1) {
        peer_len = sizeof(peer);
        int n = recvfrom(sock, pkt, sizeof(pkt), 0, (struct sockaddr *)&peer, &peer_len);
        if (n < 48) {
            continue;
        }

        uint8_t orig_ts[8];
        memcpy(orig_ts, pkt + 40, 8); // client transmit timestamp -> originate timestamp

        memset(pkt, 0, sizeof(pkt));
        pkt[0] = (0 << 6) | (4 << 3) | 4; // LI=0, VN=4, Mode=4 (Server)
        pkt[1] = 1; // Stratum 1
        pkt[2] = 6; // Poll
        pkt[3] = (uint8_t)-20; // Precision

        uint32_t ntp_sec = (uint32_t)(BASE_NTP_SEC + (uint32_t)(esp_timer_get_time() / 1000000ULL));
        uint8_t sec_be[4] = {
            (uint8_t)(ntp_sec >> 24), (uint8_t)(ntp_sec >> 16),
            (uint8_t)(ntp_sec >> 8), (uint8_t)(ntp_sec & 0xFF)
        };
        memcpy(pkt + 16, sec_be, 4); // Reference
        memcpy(pkt + 24, orig_ts, 8); // Originate
        memcpy(pkt + 32, sec_be, 4); // Receive
        memcpy(pkt + 40, sec_be, 4); // Transmit

        sendto(sock, pkt, sizeof(pkt), 0, (struct sockaddr *)&peer, peer_len);
        ESP_LOGI(TAG, "SNTP answered");
    }
}

esp_err_t dns_sntp_start(void)
{
    xTaskCreatePinnedToCore(dns_task, "dns_sink", 4096, NULL, 5, NULL, 0);
    xTaskCreatePinnedToCore(sntp_task, "sntp_srv", 3072, NULL, 4, NULL, 0);
    return ESP_OK;
}

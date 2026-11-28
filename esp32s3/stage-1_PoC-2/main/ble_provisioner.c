#include "ble_provisioner.h"
#include "wifi_ap.h"

#include <ctype.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cJSON.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"
#include "host/ble_att.h"
#include "host/ble_gap.h"
#include "host/ble_gatt.h"
#include "host/ble_hs.h"
#include "host/ble_uuid.h"
#include "mbedtls/base64.h"
#include "mbedtls/pk.h"
#include "miniz.h"
#include "nimble/nimble_port.h"
#include "nimble/nimble_port_freertos.h"
#include "psa/crypto.h"

static const char *TAG = "ble_prov";

// Event bits for synchronizing NimBLE callbacks with the provisioning worker task
#define EV_NIMBLE_SYNC    BIT0
#define EV_SCAN_REQUEST   BIT1
#define EV_CONNECTED      BIT2
#define EV_CONN_FAIL      BIT3
#define EV_DISCONNECTED   BIT4
#define EV_DISC_DONE      BIT5
#define EV_GATT_OP_DONE   BIT6
#define EV_GATT_OP_FAIL   BIT7

static EventGroupHandle_t s_ev;

// GATT UUIDs (little-endian byte order for NimBLE ble_uuid128_t)
// Setup Service: 0000fea0-0000-1000-8000-00805f9b34fb
static const ble_uuid16_t UUID_SVC_SETUP = BLE_UUID16_INIT(0xFEA0);

// 0328fe40-002f-11e5-87d0-0002a5d5c51b (connect_to_network write)
static const ble_uuid128_t UUID_CHAR_CONNECT = BLE_UUID128_INIT(
    0x1b, 0xc5, 0xd5, 0xa5, 0x02, 0x00, 0xd0, 0x87,
    0xe5, 0x11, 0x2f, 0x00, 0x40, 0xfe, 0x28, 0x03);

// 0c7c6160-fab2-11e4-9fbb-0002a5d5c51b (setup state notify)
static const ble_uuid128_t UUID_CHAR_STATE = BLE_UUID128_INIT(
    0x1b, 0xc5, 0xd5, 0xa5, 0x02, 0x00, 0xbb, 0x9f,
    0xe4, 0x11, 0xb2, 0xfa, 0x60, 0x61, 0x7c, 0x0c);

// 1a6c6160-fab2-11e4-9fbb-0002a5d5c51b (window crypto control)
static const ble_uuid128_t UUID_CHAR_CRYPTO_CTRL = BLE_UUID128_INIT(
    0x1b, 0xc5, 0xd5, 0xa5, 0x02, 0x00, 0xbb, 0x9f,
    0xe4, 0x11, 0xb2, 0xfa, 0x60, 0x61, 0x6c, 0x1a);

// 1a7c6160-fab2-11e4-9fbb-0002a5d5c51b (window crypto data)
static const ble_uuid128_t UUID_CHAR_CRYPTO_DATA = BLE_UUID128_INIT(
    0x1b, 0xc5, 0xd5, 0xa5, 0x02, 0x00, 0xbb, 0x9f,
    0xe4, 0x11, 0xb2, 0xfa, 0x60, 0x61, 0x7c, 0x1a);

static const ble_uuid16_t UUID_CCCD = BLE_UUID16_INIT(BLE_GATT_DSC_CLT_CFG_UUID16);

// Discovered handles
static uint16_t s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
static uint16_t s_svc_start = 0;
static uint16_t s_svc_end = 0;
static uint16_t s_h_connect = 0;
static uint16_t s_h_state = 0;
static uint16_t s_h_state_cccd = 0;
static uint16_t s_h_crypto_ctrl = 0;
static uint16_t s_h_crypto_data = 0;

// Shared read buffer for GATT long reads (max 512B per window page)
static uint8_t s_read_buf[600];
static size_t s_read_len = 0;
static volatile bool s_busy = false;
static uint8_t s_own_addr_type = BLE_OWN_ADDR_PUBLIC;

static const char *state_name(uint8_t s)
{
    switch (s) {
    case 0x14: return "scan/netcheck start";
    case 0x16: return "scan/netcheck done";
    case 0x1E: return "connecting";
    case 0x34: return "JSON parse failure";
    case 0x3C: return "connected";
    case 0x4E: return "command JSON parse failure";
    case 0x4F: return "credential error / connect failed";
    default:   return "unknown";
    }
}

static bool adv_is_stadia_controller(const struct ble_gap_disc_desc *disc)
{
    struct ble_hs_adv_fields fields;
    if (ble_hs_adv_parse_fields(&fields, disc->data, disc->length_data) != 0) {
        return false;
    }

    // Check Fast Pair Service Data UUID 0xFE2C
    if (fields.svc_data_uuid16 != NULL && fields.svc_data_uuid16_len >= 2) {
        uint16_t uuid16 = (uint16_t)fields.svc_data_uuid16[0] | ((uint16_t)fields.svc_data_uuid16[1] << 8);
        if (uuid16 == 0xFE2C) {
            return true;
        }
    }

    // Also check local name hints ("stadia" or "setup")
    if (fields.name != NULL && fields.name_len > 0) {
        char name[64];
        size_t copy_len = fields.name_len < sizeof(name) - 1 ? fields.name_len : sizeof(name) - 1;
        for (size_t i = 0; i < copy_len; i++) {
            name[i] = (char)tolower((unsigned char)fields.name[i]);
        }
        name[copy_len] = '\0';
        if (strstr(name, "stadia") != NULL) {
            return true;
        }
    }

    return false;
}

static int gap_event_cb(struct ble_gap_event *event, void *arg)
{
    (void)arg;
    switch (event->type) {
    case BLE_GAP_EVENT_DISC:
        if (adv_is_stadia_controller(&event->disc)) {
            ESP_LOGI(TAG, "Found Stadia Controller in setup mode (RSSI=%d), connecting...",
                     event->disc.rssi);
            ble_gap_disc_cancel();
            int rc = ble_gap_connect(s_own_addr_type, &event->disc.addr, 15000, NULL, gap_event_cb, NULL);
            if (rc != 0) {
                ESP_LOGE(TAG, "ble_gap_connect failed: %d", rc);
                xEventGroupSetBits(s_ev, EV_CONN_FAIL);
            }
        }
        return 0;

    case BLE_GAP_EVENT_DISC_COMPLETE:
        if (s_conn_handle == BLE_HS_CONN_HANDLE_NONE) {
            ESP_LOGW(TAG, "BLE scan ended without connecting (reason=%d)", event->disc_complete.reason);
            xEventGroupSetBits(s_ev, EV_CONN_FAIL);
        }
        return 0;

    case BLE_GAP_EVENT_CONNECT:
        if (event->connect.status == 0) {
            s_conn_handle = event->connect.conn_handle;
            ESP_LOGI(TAG, "BLE connected (conn_handle=%u)", s_conn_handle);
            xEventGroupSetBits(s_ev, EV_CONNECTED);
        } else {
            ESP_LOGE(TAG, "BLE connect failed: status=%d", event->connect.status);
            s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
            xEventGroupSetBits(s_ev, EV_CONN_FAIL);
        }
        return 0;

    case BLE_GAP_EVENT_DISCONNECT:
        ESP_LOGI(TAG, "BLE disconnected (reason=0x%x) — controller left setup mode or dropped link",
                 event->disconnect.reason);
        s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
        xEventGroupSetBits(s_ev, EV_DISCONNECTED);
        return 0;

    case BLE_GAP_EVENT_NOTIFY_RX: {
        uint16_t len = OS_MBUF_PKTLEN(event->notify_rx.om);
        if (event->notify_rx.attr_handle == s_h_state && len > 0) {
            uint8_t buf[16];
            uint16_t copy_len = len < sizeof(buf) ? len : sizeof(buf);
            os_mbuf_copydata(event->notify_rx.om, 0, copy_len, buf);
            uint8_t st = buf[copy_len - 1];
            ESP_LOGI(TAG, "Controller STATE -> 0x%02x (%s)", st, state_name(st));
        }
        return 0;
    }

    case BLE_GAP_EVENT_MTU:
        ESP_LOGI(TAG, "MTU updated: %u", event->mtu.value);
        return 0;

    default:
        return 0;
    }
}

//--------------------------------------------------------------------+
// GATT Discovery Callbacks
//--------------------------------------------------------------------+
static int on_dsc_disc_cb(uint16_t conn_handle, const struct ble_gatt_error *error,
                          uint16_t chr_val_handle, const struct ble_gatt_dsc *dsc, void *arg)
{
    (void)conn_handle;
    (void)chr_val_handle;
    (void)arg;
    if (error->status == 0 && dsc != NULL) {
        if (ble_uuid_cmp(&dsc->uuid.u, &UUID_CCCD.u) == 0) {
            s_h_state_cccd = dsc->handle;
        }
    } else if (error->status == BLE_HS_EDONE) {
        xEventGroupSetBits(s_ev, EV_DISC_DONE);
    } else {
        ESP_LOGE(TAG, "Descriptor discovery error: %d", error->status);
        xEventGroupSetBits(s_ev, EV_CONN_FAIL);
    }
    return 0;
}

static int on_chr_disc_cb(uint16_t conn_handle, const struct ble_gatt_error *error,
                          const struct ble_gatt_chr *chr, void *arg)
{
    (void)arg;
    if (error->status == 0 && chr != NULL) {
        if (ble_uuid_cmp(&chr->uuid.u, &UUID_CHAR_CONNECT.u) == 0) {
            s_h_connect = chr->val_handle;
        } else if (ble_uuid_cmp(&chr->uuid.u, &UUID_CHAR_STATE.u) == 0) {
            s_h_state = chr->val_handle;
        } else if (ble_uuid_cmp(&chr->uuid.u, &UUID_CHAR_CRYPTO_CTRL.u) == 0) {
            s_h_crypto_ctrl = chr->val_handle;
        } else if (ble_uuid_cmp(&chr->uuid.u, &UUID_CHAR_CRYPTO_DATA.u) == 0) {
            s_h_crypto_data = chr->val_handle;
        }
    } else if (error->status == BLE_HS_EDONE) {
        if (s_h_state != 0) {
            int rc = ble_gattc_disc_all_dscs(conn_handle, s_h_state, s_svc_end, on_dsc_disc_cb, NULL);
            if (rc != 0) {
                xEventGroupSetBits(s_ev, EV_CONN_FAIL);
            }
        } else {
            xEventGroupSetBits(s_ev, EV_DISC_DONE);
        }
    } else {
        ESP_LOGE(TAG, "Characteristic discovery error: %d", error->status);
        xEventGroupSetBits(s_ev, EV_CONN_FAIL);
    }
    return 0;
}

static int on_svc_disc_cb(uint16_t conn_handle, const struct ble_gatt_error *error,
                          const struct ble_gatt_svc *service, void *arg)
{
    (void)arg;
    if (error->status == 0 && service != NULL) {
        s_svc_start = service->start_handle;
        s_svc_end = service->end_handle;
    } else if (error->status == BLE_HS_EDONE) {
        if (s_svc_start == 0) {
            ESP_LOGE(TAG, "Setup service 0xFEA0 not found");
            xEventGroupSetBits(s_ev, EV_CONN_FAIL);
            return 0;
        }
        int rc = ble_gattc_disc_all_chrs(conn_handle, s_svc_start, s_svc_end, on_chr_disc_cb, NULL);
        if (rc != 0) {
            xEventGroupSetBits(s_ev, EV_CONN_FAIL);
        }
    } else {
        ESP_LOGE(TAG, "Service discovery error: %d", error->status);
        xEventGroupSetBits(s_ev, EV_CONN_FAIL);
    }
    return 0;
}

//--------------------------------------------------------------------+
// Synchronous GATT Write / Read Helpers (called from provision_task)
//--------------------------------------------------------------------+
static int on_gatt_write_cb(uint16_t conn_handle, const struct ble_gatt_error *error,
                            struct ble_gatt_attr *attr, void *arg)
{
    (void)conn_handle;
    (void)attr;
    (void)arg;
    if (error->status == 0) {
        xEventGroupSetBits(s_ev, EV_GATT_OP_DONE);
    } else {
        ESP_LOGE(TAG, "GATT write failed: status=%d", error->status);
        xEventGroupSetBits(s_ev, EV_GATT_OP_FAIL);
    }
    return 0;
}

static int on_gatt_read_cb(uint16_t conn_handle, const struct ble_gatt_error *error,
                           struct ble_gatt_attr *attr, void *arg)
{
    (void)conn_handle;
    (void)arg;
    if (error->status == 0 && attr != NULL && attr->om != NULL) {
        uint16_t chunk_len = OS_MBUF_PKTLEN(attr->om);
        if (s_read_len + chunk_len <= sizeof(s_read_buf)) {
            os_mbuf_copydata(attr->om, 0, chunk_len, s_read_buf + s_read_len);
            s_read_len += chunk_len;
        }
        return 0; // wait for BLE_HS_EDONE
    }
    if (error->status == BLE_HS_EDONE || error->status == 0) {
        xEventGroupSetBits(s_ev, EV_GATT_OP_DONE);
    } else {
        ESP_LOGE(TAG, "GATT read failed: status=%d", error->status);
        xEventGroupSetBits(s_ev, EV_GATT_OP_FAIL);
    }
    return 0;
}

static bool gatt_write_sync(uint16_t handle, const void *data, uint16_t len)
{
    xEventGroupClearBits(s_ev, EV_GATT_OP_DONE | EV_GATT_OP_FAIL);
    int rc = ble_gattc_write_flat(s_conn_handle, handle, data, len, on_gatt_write_cb, NULL);
    if (rc != 0) {
        return false;
    }
    EventBits_t bits = xEventGroupWaitBits(s_ev, EV_GATT_OP_DONE | EV_GATT_OP_FAIL | EV_DISCONNECTED,
                                           pdTRUE, pdFALSE, pdMS_TO_TICKS(10000));
    return (bits & EV_GATT_OP_DONE) != 0;
}

static bool gatt_read_long_sync(uint16_t handle, size_t *out_len)
{
    s_read_len = 0;
    xEventGroupClearBits(s_ev, EV_GATT_OP_DONE | EV_GATT_OP_FAIL);
    int rc = ble_gattc_read_long(s_conn_handle, handle, 0, on_gatt_read_cb, NULL);
    if (rc != 0) {
        return false;
    }
    EventBits_t bits = xEventGroupWaitBits(s_ev, EV_GATT_OP_DONE | EV_GATT_OP_FAIL | EV_DISCONNECTED,
                                           pdTRUE, pdFALSE, pdMS_TO_TICKS(10000));
    if ((bits & EV_GATT_OP_DONE) == 0) {
        return false;
    }
    *out_len = s_read_len;
    return true;
}

//--------------------------------------------------------------------+
// Gzip Decompression (RFC 1952 header skip + ROM miniz tinfl)
//--------------------------------------------------------------------+
static char *decompress_window_payload(const uint8_t *raw, size_t raw_len, size_t *out_len)
{
    if (raw_len >= 16) {
        ESP_LOGI(TAG, "Window 1a head: %02x %02x %02x %02x %02x %02x %02x %02x %02x %02x",
                 raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7], raw[8], raw[9]);
    }
    if (raw_len < 18 || raw[0] != 0x1F || raw[1] != 0x8B || raw[2] != 0x08) {
        // Not gzip; return null-terminated copy as-is
        char *plain = malloc(raw_len + 1);
        if (plain) {
            memcpy(plain, raw, raw_len);
            plain[raw_len] = '\0';
            *out_len = raw_len;
        }
        return plain;
    }

    uint8_t flags = raw[3];
    size_t pos = 10;
    if (flags & 0x04) { // FEXTRA
        if (pos + 2 > raw_len) return NULL;
        uint16_t xlen = (uint16_t)raw[pos] | ((uint16_t)raw[pos + 1] << 8);
        pos += 2 + xlen;
    }
    if (flags & 0x08) { // FNAME
        while (pos < raw_len && raw[pos] != '\0') pos++;
        pos++;
    }
    if (flags & 0x10) { // FCOMMENT
        while (pos < raw_len && raw[pos] != '\0') pos++;
        pos++;
    }
    if (flags & 0x02) { // FHCRC
        pos += 2;
    }
    if (pos + 8 >= raw_len) {
        return NULL;
    }

    size_t deflate_len = raw_len - pos - 8;
    const size_t max_out = 32768;
    char *out_buf = malloc(max_out + 1);
    if (!out_buf) {
        return NULL;
    }
    size_t dec_len = tinfl_decompress_mem_to_mem(
        out_buf, max_out, raw + pos, deflate_len, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF);
    if (dec_len == TINFL_DECOMPRESS_MEM_TO_MEM_FAILED) {
        free(out_buf);
        return NULL;
    }
    out_buf[dec_len] = '\0';
    *out_len = dec_len;
    ESP_LOGI(TAG, "Decompressed Crypto Window 1a: %u -> %u bytes", (unsigned)raw_len, (unsigned)dec_len);
    return out_buf;
}

//--------------------------------------------------------------------+
// RSA PKCS#1 v1.5 Encryption (IDF v6.1.1 PSA Crypto + mbedtls_pk)
//--------------------------------------------------------------------+
static char *encrypt_wifi_password(const char *pub_b64, const char *password)
{
    size_t der_len = 0;
    uint8_t der_buf[1024];
    int ret = mbedtls_base64_decode(der_buf, sizeof(der_buf), &der_len,
                                    (const unsigned char *)pub_b64, strlen(pub_b64));
    if (ret != 0) {
        ESP_LOGE(TAG, "Base64 decode public_key failed: -0x%04x", -ret);
        return NULL;
    }

    mbedtls_pk_context pk;
    mbedtls_pk_init(&pk);
    ret = mbedtls_pk_parse_public_key(&pk, der_buf, der_len);
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_pk_parse_public_key failed: -0x%04x", -ret);
        mbedtls_pk_free(&pk);
        return NULL;
    }

    psa_key_attributes_t attrs = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_usage_flags(&attrs, PSA_KEY_USAGE_ENCRYPT);
    psa_set_key_algorithm(&attrs, PSA_ALG_RSA_PKCS1V15_CRYPT);

    mbedtls_svc_key_id_t key_id = MBEDTLS_SVC_KEY_ID_INIT;
    ret = mbedtls_pk_import_into_psa(&pk, &attrs, &key_id);
    mbedtls_pk_free(&pk);
    if (ret != 0) {
        ESP_LOGE(TAG, "mbedtls_pk_import_into_psa failed: -0x%04x", -ret);
        return NULL;
    }

    uint8_t ct[512];
    size_t ct_len = 0;
    psa_status_t st = psa_asymmetric_encrypt(key_id, PSA_ALG_RSA_PKCS1V15_CRYPT,
                                             (const uint8_t *)password, strlen(password),
                                             NULL, 0, ct, sizeof(ct), &ct_len);
    psa_destroy_key(key_id);
    if (st != PSA_SUCCESS) {
        ESP_LOGE(TAG, "psa_asymmetric_encrypt failed: %d", (int)st);
        return NULL;
    }

    size_t b64_buf_len = ((ct_len + 2) / 3) * 4 + 4;
    char *enc_b64 = malloc(b64_buf_len);
    if (!enc_b64) {
        return NULL;
    }
    size_t out_b64_len = 0;
    ret = mbedtls_base64_encode((unsigned char *)enc_b64, b64_buf_len, &out_b64_len, ct, ct_len);
    if (ret != 0) {
        free(enc_b64);
        return NULL;
    }
    enc_b64[out_b64_len] = '\0';
    return enc_b64;
}

//--------------------------------------------------------------------+
// Provisioning State Machine Worker Task
//--------------------------------------------------------------------+
static void run_provisioning_flow(void)
{
    s_busy = true;
    s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
    s_svc_start = s_svc_end = 0;
    s_h_connect = s_h_state = s_h_state_cccd = s_h_crypto_ctrl = s_h_crypto_data = 0;

    xEventGroupClearBits(s_ev, EV_CONNECTED | EV_CONN_FAIL | EV_DISCONNECTED | EV_DISC_DONE);

    struct ble_gap_disc_params disc_params = {
        .filter_duplicates = 1,
        .passive = 0,
        .itvl = BLE_GAP_SCAN_ITVL_MS(60),
        .window = BLE_GAP_SCAN_WIN_MS(30),
    };
    ESP_LOGI(TAG, "Scanning 30s for Stadia Controller in setup mode (blinking orange)...");
    int rc = ble_gap_disc(s_own_addr_type, 30000, &disc_params, gap_event_cb, NULL);
    if (rc != 0) {
        ESP_LOGE(TAG, "ble_gap_disc failed: %d", rc);
        s_busy = false;
        return;
    }

    EventBits_t bits = xEventGroupWaitBits(s_ev, EV_CONNECTED | EV_CONN_FAIL,
                                           pdTRUE, pdFALSE, pdMS_TO_TICKS(35000));
    if ((bits & EV_CONNECTED) == 0) {
        ble_gap_disc_cancel();
        ESP_LOGW(TAG, "No controller connected. Press BOOT to try again.");
        s_busy = false;
        return;
    }

    // Request larger MTU for faster 512B window reads
    ble_gattc_exchange_mtu(s_conn_handle, NULL, NULL);
    vTaskDelay(pdMS_TO_TICKS(200));

    // Discover 0xFEA0 service & characteristics
    rc = ble_gattc_disc_svc_by_uuid(s_conn_handle, &UUID_SVC_SETUP.u, on_svc_disc_cb, NULL);
    if (rc != 0) {
        goto cleanup;
    }
    bits = xEventGroupWaitBits(s_ev, EV_DISC_DONE | EV_CONN_FAIL | EV_DISCONNECTED,
                               pdTRUE, pdFALSE, pdMS_TO_TICKS(15000));
    if ((bits & EV_DISC_DONE) == 0) {
        ESP_LOGE(TAG, "GATT discovery incomplete");
        goto cleanup;
    }
    if (!s_h_connect || !s_h_crypto_ctrl || !s_h_crypto_data) {
        ESP_LOGE(TAG, "Required setup characteristics missing (connect=%u, ctrl=%u, data=%u)",
                 s_h_connect, s_h_crypto_ctrl, s_h_crypto_data);
        goto cleanup;
    }

    // Enable CHAR_STATE notifications if CCCD was found
    if (s_h_state_cccd != 0) {
        uint8_t cccd_val[2] = {0x01, 0x00};
        gatt_write_sync(s_h_state_cccd, cccd_val, sizeof(cccd_val));
    }

    // Read Window 1a (Crypto Bundle) page by page
    uint8_t *raw_win = malloc(24 * 512);
    if (!raw_win) {
        goto cleanup;
    }
    size_t raw_win_len = 0;
    for (uint16_t page = 0; page < 24; page++) {
        uint8_t page_le[2] = {(uint8_t)(page & 0xFF), (uint8_t)(page >> 8)};
        if (!gatt_write_sync(s_h_crypto_ctrl, page_le, 2)) {
            free(raw_win);
            goto cleanup;
        }
        vTaskDelay(pdMS_TO_TICKS(400));
        size_t chunk_len = 0;
        if (!gatt_read_long_sync(s_h_crypto_data, &chunk_len)) {
            free(raw_win);
            goto cleanup;
        }
        if (chunk_len == 0) {
            break;
        }
        memcpy(raw_win + raw_win_len, s_read_buf, chunk_len);
        raw_win_len += chunk_len;
        if (chunk_len < 512) {
            break;
        }
    }
    ESP_LOGI(TAG, "Read Crypto Window 1a: %u bytes", (unsigned)raw_win_len);

    size_t json_len = 0;
    char *crypto_json = decompress_window_payload(raw_win, raw_win_len, &json_len);
    free(raw_win);
    if (!crypto_json) {
        ESP_LOGE(TAG, "Failed to decompress Crypto Window 1a");
        goto cleanup;
    }

    cJSON *root = cJSON_Parse(crypto_json);
    free(crypto_json);
    cJSON *dev_info = root ? cJSON_GetObjectItem(root, "device_info") : NULL;
    cJSON *pub_item = dev_info ? cJSON_GetObjectItem(dev_info, "public_key") : NULL;
    if (!cJSON_IsString(pub_item)) {
        ESP_LOGE(TAG, "device_info.public_key missing in Crypto Window");
        cJSON_Delete(root);
        goto cleanup;
    }

    char *enc_passwd = encrypt_wifi_password(pub_item->valuestring, POC2_SOFTAP_PASS);
    cJSON_Delete(root);
    if (!enc_passwd) {
        goto cleanup;
    }

    // Build connect_to_network JSON: WPA2-PSK (7), CCMP (4)
    cJSON *conn_obj = cJSON_CreateObject();
    cJSON_AddStringToObject(conn_obj, "ssid", POC2_SOFTAP_SSID);
    cJSON_AddNumberToObject(conn_obj, "wpa_auth", 7);
    cJSON_AddNumberToObject(conn_obj, "wpa_cipher", 4);
    cJSON_AddStringToObject(conn_obj, "enc_passwd", enc_passwd);
    char *conn_json = cJSON_PrintUnformatted(conn_obj);
    cJSON_Delete(conn_obj);
    free(enc_passwd);

    if (!conn_json) {
        goto cleanup;
    }

    ESP_LOGI(TAG, "Writing Wi-Fi credentials for '%s' to CHAR_CONNECT...", POC2_SOFTAP_SSID);
    bool ok = gatt_write_sync(s_h_connect, conn_json, (uint16_t)strlen(conn_json));
    free(conn_json);
    if (!ok) {
        ESP_LOGE(TAG, "Failed to write CHAR_CONNECT");
        goto cleanup;
    }

    ESP_LOGI(TAG, "Credentials sent! Waiting up to 30s for controller to join SoftAP...");
    xEventGroupWaitBits(s_ev, EV_DISCONNECTED, pdTRUE, pdFALSE, pdMS_TO_TICKS(30000));

cleanup:
    if (s_conn_handle != BLE_HS_CONN_HANDLE_NONE) {
        ble_gap_terminate(s_conn_handle, BLE_ERR_REM_USER_CONN_TERM);
    }
    s_busy = false;
}

static void provision_task(void *arg)
{
    (void)arg;
    xEventGroupWaitBits(s_ev, EV_NIMBLE_SYNC, pdFALSE, pdTRUE, portMAX_DELAY);
    ESP_LOGI(TAG, "NimBLE ready. Put controller in setup mode (hold Y + Stadia) and press BOOT.");

    while (1) {
        xEventGroupWaitBits(s_ev, EV_SCAN_REQUEST, pdTRUE, pdFALSE, portMAX_DELAY);
        run_provisioning_flow();
    }
}

static void on_nimble_sync(void)
{
    int rc = ble_hs_id_infer_auto(0, &s_own_addr_type);
    if (rc != 0) {
        ESP_LOGE(TAG, "ble_hs_id_infer_auto failed: %d", rc);
    }
    xEventGroupSetBits(s_ev, EV_NIMBLE_SYNC);
}

static void on_nimble_reset(int reason)
{
    ESP_LOGW(TAG, "NimBLE reset: reason=%d", reason);
}

static void nimble_host_task(void *param)
{
    (void)param;
    nimble_port_run();
    nimble_port_freertos_deinit();
}

esp_err_t ble_provisioner_init(void)
{
    psa_status_t st = psa_crypto_init();
    if (st != PSA_SUCCESS) {
        ESP_LOGE(TAG, "psa_crypto_init failed: %d", (int)st);
        return ESP_FAIL;
    }

    s_ev = xEventGroupCreate();
    ESP_ERROR_CHECK(nimble_port_init());

    ble_hs_cfg.reset_cb = on_nimble_reset;
    ble_hs_cfg.sync_cb = on_nimble_sync;
    ble_att_set_preferred_mtu(517);

    nimble_port_freertos_init(nimble_host_task);
    xTaskCreatePinnedToCore(provision_task, "ble_prov", 24576, NULL, 5, NULL, 0);
    return ESP_OK;
}

esp_err_t ble_provisioner_start_scan(void)
{
    if (s_busy) {
        ESP_LOGW(TAG, "Provisioning already in progress");
        return ESP_ERR_INVALID_STATE;
    }
    xEventGroupSetBits(s_ev, EV_SCAN_REQUEST);
    return ESP_OK;
}

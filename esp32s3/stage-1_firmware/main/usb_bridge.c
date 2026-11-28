#include "usb_bridge.h"
#include "rtc_endpoint.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "esp_audio_dec.h"
#include "esp_audio_enc.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/ringbuf.h"
#include "freertos/task.h"
#include "impl/esp_opus_dec.h"
#include "impl/esp_opus_enc.h"
#include "tusb.h"
#include "usb_descriptors.h"
#include "usb_device_uac.h"

static const char *TAG = "usb_bridge";

#define SAMPLE_RATE_HZ   48000
#define FRAME_SAMPLES    960
#define MONO_PCM_BYTES   (FRAME_SAMPLES * 2)
#define STEREO_PCM_BYTES (FRAME_SAMPLES * 4)

typedef struct __attribute__((packed)) {
    uint8_t lx;
    uint8_t ly;
    uint8_t rx;
    uint8_t ry;
    uint8_t buttons0; // [3:0] D-pad hat (8=released), [4]Square, [5]Cross, [6]Circle, [7]Triangle
    uint8_t buttons1; // [0]L1, [1]R1, [2]L2, [3]R2, [4]Share, [5]Options, [6]L3, [7]R3
    uint8_t buttons2; // [0]PS, [1]Touchpad, [7:2]Counter
    uint8_t l2_trigger;
    uint8_t r2_trigger;
    uint8_t padding[54];
} ds4_input_report_t;

_Static_assert(sizeof(ds4_input_report_t) == 63, "DS4 report payload must be 63 bytes");

static portMUX_TYPE s_lock = portMUX_INITIALIZER_UNLOCKED;
static ds4_input_report_t s_report = {
    .lx = 128,
    .ly = 128,
    .rx = 128,
    .ry = 128,
    .buttons0 = 0x08,
};
static usb_rumble_cb_t s_rumble_cb = NULL;
static volatile uint8_t s_rumble_left = 0;
static volatile uint8_t s_rumble_right = 0;
static volatile bool s_rumble_dirty = false;

static RingbufHandle_t s_spk_rb = NULL;
static RingbufHandle_t s_mic_rb = NULL;
static volatile bool s_mic_primed = false;
static volatile bool s_spk_primed = false;
static volatile int s_mic_mute_frames = 0;

void usb_bridge_notify_mic_start(void)
{
    s_mic_mute_frames = 3;
}

static uint8_t map_dpad_hat(uint32_t btn)
{
    bool up    = (btn & (1U << 0)) != 0;
    bool down  = (btn & (1U << 1)) != 0;
    bool left  = (btn & (1U << 2)) != 0;
    bool right = (btn & (1U << 3)) != 0;

    if (up && right)   return 1;
    if (down && right) return 3;
    if (down && left)  return 5;
    if (up && left)    return 7;
    if (up)            return 0;
    if (right)         return 2;
    if (down)          return 4;
    if (left)          return 6;
    return 8;
}

static volatile uint32_t s_hid_in_updates = 0;
static volatile uint32_t s_hid_done_cnt = 0;
static volatile uint32_t s_hid_get_rep_cnt = 0;
static volatile int64_t s_usb_mounted_us = 0;

void usb_bridge_update_input(const stadia_controller_input_t *in)
{
    if (!in) return;
    s_hid_in_updates++;

    ds4_input_report_t next = {0};
    next.lx = (uint8_t)(((int32_t)in->lx + 32768) >> 8);
    next.ly = (uint8_t)(255 - (((int32_t)in->ly + 32768) >> 8));
    next.rx = (uint8_t)(((int32_t)in->rx + 32768) >> 8);
    next.ry = (uint8_t)(255 - (((int32_t)in->ry + 32768) >> 8));

    uint32_t b = in->buttons;
    uint8_t b0 = map_dpad_hat(b);
    if (b & (1U << 14)) b0 |= (1U << 4); // X -> Square
    if (b & (1U << 12)) b0 |= (1U << 5); // A -> Cross
    if (b & (1U << 13)) b0 |= (1U << 6); // B -> Circle
    if (b & (1U << 15)) b0 |= (1U << 7); // Y -> Triangle
    next.buttons0 = b0;

    uint8_t b1 = 0;
    if (b & (1U << 8))    b1 |= (1U << 0); // L1
    if (b & (1U << 9))    b1 |= (1U << 1); // R1
    if (in->l2 > 20)      b1 |= (1U << 2); // L2 digital
    if (in->r2 > 20)      b1 |= (1U << 3); // R2 digital
    if (b & (1U << 5))    b1 |= (1U << 4); // Options (...) -> Share
    if (b & (1U << 4))    b1 |= (1U << 5); // Menu (=) -> Options
    if (b & (1U << 6))    b1 |= (1U << 6); // L3
    if (b & (1U << 7))    b1 |= (1U << 7); // R3
    next.buttons1 = b1;

    uint8_t b2 = 0;
    if (b & (1U << 16))   b2 |= (1U << 0); // Stadia -> PS
    if (b & (1U << 11))   b2 |= (1U << 1); // Capture -> Touchpad click
    next.buttons2 = b2;

    next.l2_trigger = in->l2;
    next.r2_trigger = in->r2;

    portENTER_CRITICAL(&s_lock);
    s_report = next;
    portEXIT_CRITICAL(&s_lock);
}

void usb_bridge_set_rumble_cb(usb_rumble_cb_t cb)
{
    s_rumble_cb = cb;
}

void tud_mount_cb(void)
{
    s_usb_mounted_us = esp_timer_get_time();
    ESP_LOGI(TAG, "USB Mounted on host (DS4 HID + UAC 2.0)");
}

void tud_umount_cb(void)
{
    s_usb_mounted_us = 0;
    ESP_LOGI(TAG, "USB Unmounted");
}

void tud_hid_report_complete_cb(uint8_t instance, uint8_t const *report, uint16_t len)
{
    (void)instance;
    (void)report;
    (void)len;
    s_hid_done_cnt++;
}

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t report_type,
                               uint8_t *buffer, uint16_t reqlen)
{
    (void)instance;
    s_hid_get_rep_cnt++;
    if (report_type != HID_REPORT_TYPE_FEATURE) return 0;

    if (report_id == 0x02 && reqlen >= 36) {
        memset(buffer, 0, 36);
        return 36;
    }
    if (report_id == 0x12 && reqlen >= 15) {
        static const uint8_t mac_info[15] = {
            0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01, 0x08, 0x25, 0x00,
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66
        };
        memcpy(buffer, mac_info, 15);
        return 15;
    }
    if (report_id == 0xA3 && reqlen >= 48) {
        memset(buffer, 0, 48);
        memcpy(buffer, "Nov 28 2026", 11);
        memcpy(buffer + 16, "21:30:00", 8);
        return 48;
    }
    return 0;
}

void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t report_type,
                           uint8_t const *buffer, uint16_t bufsize)
{
    (void)instance;
    (void)report_type;

    uint8_t const *payload = buffer;
    uint16_t len = bufsize;
    uint8_t effective_id = report_id;

    if (effective_id == 0 && len > 0) {
        effective_id = buffer[0];
        payload = buffer + 1;
        len--;
    }

    if (effective_id == 0x05 && len >= 5) {
        s_rumble_right = payload[3];
        s_rumble_left = payload[4];
        s_rumble_dirty = true;
    }
}

static esp_err_t uac_output_cb(uint8_t *buf, size_t len, void *cb_ctx)
{
    (void)cb_ctx;
    if (s_spk_rb && len > 0) {
        xRingbufferSend(s_spk_rb, buf, len, 0);
    }
    return ESP_OK;
}

static esp_err_t uac_input_cb(uint8_t *buf, size_t len, size_t *bytes_read, void *cb_ctx)
{
    (void)cb_ctx;
    size_t got = 0;
    if (s_mic_rb && s_mic_primed) {
        size_t pos = 0;
        while (pos < len) {
            size_t chunk = 0;
            uint8_t *item = xRingbufferReceiveUpTo(s_mic_rb, &chunk, 0, len - pos);
            if (!item || chunk == 0) {
                break;
            }
            memcpy(buf + pos, item, chunk);
            vRingbufferReturnItem(s_mic_rb, item);
            pos += chunk;
        }
        got = pos;
        if (got == 0) {
            s_mic_primed = false;
        }
    }
    if (got < len) {
        memset(buf + got, 0, len - got);
    }
    *bytes_read = len;
    return ESP_OK;
}

static void usb_hid_task(void *arg)
{
    (void)arg;
    uint8_t seq = 0;
    uint32_t sent_5s = 0;
    uint32_t ticks = 0;

    while (1) {
        bool allow_hid = tud_mounted() && s_usb_mounted_us > 0 &&
                         (esp_timer_get_time() - s_usb_mounted_us >= 2000000);
        if (allow_hid && tud_hid_ready()) {
            ds4_input_report_t out;
            portENTER_CRITICAL(&s_lock);
            out = s_report;
            portEXIT_CRITICAL(&s_lock);

            out.buttons2 = (out.buttons2 & 0x03) | (uint8_t)((seq++ & 0x3F) << 2);
            if (tud_hid_report(0x01, &out, sizeof(out))) {
                sent_5s++;
            }
        }

        if (s_rumble_dirty) {
            s_rumble_dirty = false;
            uint8_t l = s_rumble_left;
            uint8_t r = s_rumble_right;
            if (s_rumble_cb) {
                s_rumble_cb(l, r);
            }
        }

        if (++ticks >= 500) {
            ticks = 0;
            ESP_LOGI(TAG, "HID 5s: sent=%lu, done=%lu, get_rep=%lu, updates=%lu, mounted=%d, ready=%d",
                     (unsigned long)sent_5s, (unsigned long)s_hid_done_cnt,
                     (unsigned long)s_hid_get_rep_cnt, (unsigned long)s_hid_in_updates,
                     tud_mounted(), tud_hid_ready());
            sent_5s = 0;
            s_hid_done_cnt = 0;
            s_hid_in_updates = 0;
        }

        vTaskDelay(pdMS_TO_TICKS(10));
    }
}

static void audio_duplex_task(void *arg)
{
    (void)arg;

    void *enc_hd = NULL;
    void *dec_hd = NULL;
    esp_opus_enc_config_t enc_cfg = {
        .sample_rate = SAMPLE_RATE_HZ,
        .channel = 1,
        .bits_per_sample = 16,
        .bitrate = 64000,
        .frame_duration = ESP_OPUS_ENC_FRAME_DURATION_20_MS,
        .application_mode = ESP_OPUS_ENC_APPLICATION_LOWDELAY,
        .complexity = 0,
        .enable_fec = false,
        .enable_dtx = false,
        .enable_vbr = false,
    };
    esp_opus_dec_cfg_t dec_cfg = {
        .sample_rate = SAMPLE_RATE_HZ,
        .channel = 1,
        .frame_duration = ESP_OPUS_DEC_FRAME_DURATION_20_MS,
        .self_delimited = false,
    };

    if (esp_opus_enc_open(&enc_cfg, sizeof(enc_cfg), &enc_hd) != ESP_AUDIO_ERR_OK ||
        esp_opus_dec_open(&dec_cfg, sizeof(dec_cfg), &dec_hd) != ESP_AUDIO_ERR_OK) {
        ESP_LOGE(TAG, "Failed to open Opus encoder/decoder");
        vTaskDelete(NULL);
        return;
    }

    int in_sz = 0, out_sz = 0;
    esp_opus_enc_get_frame_size(enc_hd, &in_sz, &out_sz);
    uint8_t *spk_pcm = malloc(STEREO_PCM_BYTES);
    int16_t *spk_mono = malloc(MONO_PCM_BYTES);
    uint8_t *spk_opus = malloc((size_t)out_sz);
    uint8_t *mic_opus = malloc(384);
    uint8_t *mic_pcm = malloc(MONO_PCM_BYTES);
    size_t spk_acc = 0;

    ESP_LOGI(TAG, "Core 1 Opus Audio Bridge ready (enc=1ch %dB->%dB, dec=48kHz mono)", in_sz, out_sz);

    TickType_t last_wake = xTaskGetTickCount();
    uint32_t slot = 0;
    uint32_t spk_ts = 0;
    uint32_t mic_ok = 0, mic_err = 0, spk_tx = 0, mic_queued = 0;
    uint64_t enc_us_sum = 0, dec_us_sum = 0;

    while (1) {
        // 1. Headset TX: 60ms pre-buffer, drain up to 2 frames per wake so wrap/drift never stalls
        if (s_spk_rb) {
            if (!s_spk_primed) {
                size_t free_sp = xRingbufferGetCurFreeSize(s_spk_rb);
                size_t used = (STEREO_PCM_BYTES * 8) - free_sp;
                if (used >= STEREO_PCM_BYTES * 3) {
                    s_spk_primed = true;
                }
            }
            if (s_spk_primed) {
                int sent_this_tick = 0;
                while (sent_this_tick < 2) {
                    while (spk_acc < STEREO_PCM_BYTES) {
                        size_t got = 0;
                        uint8_t *item = xRingbufferReceiveUpTo(s_spk_rb, &got, 0, STEREO_PCM_BYTES - spk_acc);
                        if (!item || got == 0) {
                            break;
                        }
                        memcpy(spk_pcm + spk_acc, item, got);
                        vRingbufferReturnItem(s_spk_rb, item);
                        spk_acc += got;
                    }
                    if (spk_acc < STEREO_PCM_BYTES) {
                        if (spk_acc == 0 && sent_this_tick == 0) {
                            s_spk_primed = false;
                        }
                        break;
                    }
                    spk_acc = 0;
                    if (rtc_endpoint_is_srtp_ready()) {
                        const int16_t *st = (const int16_t *)spk_pcm;
                        for (int i = 0; i < FRAME_SAMPLES; i++) {
                            spk_mono[i] = (int16_t)(((int32_t)st[2 * i] + (int32_t)st[2 * i + 1]) >> 1);
                        }
                        esp_audio_enc_in_frame_t inf = {.buffer = (uint8_t *)spk_mono, .len = MONO_PCM_BYTES};
                        esp_audio_enc_out_frame_t outf = {.buffer = spk_opus, .len = (uint32_t)out_sz};
                        int64_t t0 = esp_timer_get_time();
                        esp_audio_err_t eerr = esp_opus_enc_process(enc_hd, &inf, &outf);
                        enc_us_sum += (uint64_t)(esp_timer_get_time() - t0);
                        if (eerr == ESP_AUDIO_ERR_OK && outf.encoded_bytes > 0) {
                            rtc_endpoint_send_audio_opus(spk_opus, outf.encoded_bytes, spk_ts);
                            spk_ts += FRAME_SAMPLES;
                            spk_tx++;
                        }
                    }
                    sent_this_tick++;
                }
            }
        }

        // 2. Mic RX: SRTP -> Opus Dec -> UAC Mic (48kHz mono) with 60ms (3-frame) jitter prime
        size_t mop_len = 0;
        while (rtc_endpoint_recv_mic_opus(mic_opus, 384, &mop_len)) {
            esp_audio_dec_in_raw_t raw = {.buffer = mic_opus, .len = (uint32_t)mop_len};
            esp_audio_dec_out_frame_t decf = {.buffer = mic_pcm, .len = MONO_PCM_BYTES};
            esp_audio_dec_info_t info = {0};
            int64_t t0 = esp_timer_get_time();
            esp_audio_err_t derr = esp_opus_dec_decode(dec_hd, &raw, &decf, &info);
            dec_us_sum += (uint64_t)(esp_timer_get_time() - t0);
            if (derr == ESP_AUDIO_ERR_OK && s_mic_rb) {
                uint32_t out_bytes = decf.decoded_size > 0 ? decf.decoded_size : MONO_PCM_BYTES;
                if (s_mic_mute_frames > 0) {
                    s_mic_mute_frames--;
                    memset(mic_pcm, 0, out_bytes);
                }
                if (xRingbufferSend(s_mic_rb, mic_pcm, out_bytes, 0) == pdTRUE) {
                    mic_ok++;
                    if (!s_mic_primed) {
                        mic_queued++;
                        if (mic_queued >= 3) {
                            s_mic_primed = true;
                            mic_queued = 0;
                        }
                    }
                }
            } else {
                mic_err++;
            }
        }

        slot++;
        if ((slot % 250) == 0 && (mic_ok > 0 || mic_err > 0 || spk_tx > 0)) {
            ESP_LOGI(TAG, "Audio 5s: SpkTX=%lu (%.1fms), MicOK=%lu (%.1fms), MicErr=%lu",
                     (unsigned long)spk_tx,
                     spk_tx ? (double)enc_us_sum / spk_tx / 1000.0 : 0.0,
                     (unsigned long)mic_ok,
                     mic_ok ? (double)dec_us_sum / mic_ok / 1000.0 : 0.0,
                     (unsigned long)mic_err);
            spk_tx = mic_ok = mic_err = 0;
            enc_us_sum = dec_us_sum = 0;
        }
        vTaskDelayUntil(&last_wake, pdMS_TO_TICKS(20));
    }
}

esp_err_t usb_bridge_init(void)
{
    s_spk_rb = xRingbufferCreate(STEREO_PCM_BYTES * 8, RINGBUF_TYPE_BYTEBUF);
    s_mic_rb = xRingbufferCreate(MONO_PCM_BYTES * 8, RINGBUF_TYPE_BYTEBUF);
    if (!s_spk_rb || !s_mic_rb) {
        return ESP_ERR_NO_MEM;
    }

    uac_device_config_t uac_cfg = {
        .skip_tinyusb_init = false,
        .output_cb = uac_output_cb,
        .input_cb = uac_input_cb,
        .set_mute_cb = NULL,
        .set_volume_cb = NULL,
        .cb_ctx = NULL,
        .spk_itf_num = ITF_NUM_AUDIO_STREAMING_SPK,
        .mic_itf_num = ITF_NUM_AUDIO_STREAMING_MIC,
    };
    esp_err_t ret = uac_device_init(&uac_cfg);
    if (ret != ESP_OK) {
        return ret;
    }

    xTaskCreatePinnedToCoreWithCaps(usb_hid_task, "usb_hid", 4096, NULL, 4, NULL, 1,
                                    MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    xTaskCreatePinnedToCoreWithCaps(audio_duplex_task, "opus_duplex", 49152, NULL, 3, NULL, 1,
                                    MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    ESP_LOGI(TAG, "USB Bridge initialized (DS4 HID + UAC 2.0 + Core 1 Opus)");
    return ESP_OK;
}

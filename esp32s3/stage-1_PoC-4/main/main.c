#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "esp_audio_dec.h"
#include "esp_audio_enc.h"
#include "esp_log.h"
#include "esp_psram.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/ringbuf.h"
#include "freertos/task.h"
#include "impl/esp_opus_dec.h"
#include "impl/esp_opus_enc.h"
#include "usb_device_uac.h"

static const char *TAG = "stage1_poc4";

#define SAMPLE_RATE_HZ   48000
#define FRAME_SAMPLES    960   // 20ms at 48kHz
#define MONO_PCM_BYTES   (FRAME_SAMPLES * 2)
#define STEREO_PCM_BYTES (FRAME_SAMPLES * 4)

static RingbufHandle_t s_spk_rb = NULL;
static RingbufHandle_t s_mic_rb = NULL;

static volatile uint64_t s_live_enc_us = 0;
static volatile uint64_t s_live_dec_us = 0;
static volatile uint32_t s_live_frames = 0;
static volatile uint32_t s_live_enc_max_us = 0;
static volatile uint32_t s_live_dec_max_us = 0;

//--------------------------------------------------------------------+
// Benchmark: Measure 48kHz Opus Enc (Stereo/Mono) + Mono Dec on Core 1
//--------------------------------------------------------------------+
static void run_codec_benchmark(int enc_channels, int complexity)
{
    void *enc_hd = NULL;
    void *dec_hd = NULL;

    esp_opus_enc_config_t enc_cfg = {
        .sample_rate = SAMPLE_RATE_HZ,
        .channel = enc_channels,
        .bits_per_sample = 16,
        .bitrate = 64000,
        .frame_duration = ESP_OPUS_ENC_FRAME_DURATION_20_MS,
        .application_mode = ESP_OPUS_ENC_APPLICATION_LOWDELAY,
        .complexity = complexity,
        .enable_fec = false,
        .enable_dtx = false,
        .enable_vbr = false,
    };
    esp_audio_err_t err = esp_opus_enc_open(&enc_cfg, sizeof(enc_cfg), &enc_hd);
    if (err != ESP_AUDIO_ERR_OK || !enc_hd) {
        ESP_LOGE(TAG, "Benchmark enc open failed: %d", err);
        return;
    }

    esp_opus_dec_cfg_t dec_cfg = {
        .sample_rate = SAMPLE_RATE_HZ,
        .channel = 1,
        .frame_duration = ESP_OPUS_DEC_FRAME_DURATION_20_MS,
        .self_delimited = false,
    };
    err = esp_opus_dec_open(&dec_cfg, sizeof(dec_cfg), &dec_hd);
    if (err != ESP_AUDIO_ERR_OK || !dec_hd) {
        ESP_LOGE(TAG, "Benchmark dec open failed: %d", err);
        esp_opus_enc_close(enc_hd);
        return;
    }

    int in_sz = 0, out_sz = 0;
    esp_opus_enc_get_frame_size(enc_hd, &in_sz, &out_sz);

    int16_t *pcm_in = malloc((size_t)in_sz);
    uint8_t *opus_buf = malloc((size_t)out_sz);
    int16_t *pcm_out = malloc(MONO_PCM_BYTES);

    // Prepare a real mono opus frame for the decoder benchmark
    void *mono_enc_hd = NULL;
    esp_opus_enc_config_t mono_cfg = enc_cfg;
    mono_cfg.channel = 1;
    esp_opus_enc_open(&mono_cfg, sizeof(mono_cfg), &mono_enc_hd);
    int16_t *mono_pcm = malloc(MONO_PCM_BYTES);
    uint8_t *mono_opus = malloc(512);
    for (int i = 0; i < FRAME_SAMPLES; i++) {
        int16_t s = (int16_t)(sinf(2.0f * (float)M_PI * 440.0f * i / SAMPLE_RATE_HZ) * 12000.0f);
        mono_pcm[i] = s;
        if (enc_channels == 2) {
            pcm_in[2 * i] = s;
            pcm_in[2 * i + 1] = s;
        } else {
            pcm_in[i] = s;
        }
    }
    esp_audio_enc_in_frame_t m_in = {.buffer = (uint8_t *)mono_pcm, .len = MONO_PCM_BYTES};
    esp_audio_enc_out_frame_t m_out = {.buffer = mono_opus, .len = 512};
    esp_opus_enc_process(mono_enc_hd, &m_in, &m_out);
    esp_opus_enc_close(mono_enc_hd);

    const int N = 100; // 2 seconds of 20ms frames
    uint64_t enc_sum_us = 0, dec_sum_us = 0;
    uint64_t enc_max_us = 0, dec_max_us = 0;

    for (int i = 0; i < N; i++) {
        esp_audio_enc_in_frame_t in_f = {.buffer = (uint8_t *)pcm_in, .len = (uint32_t)in_sz};
        esp_audio_enc_out_frame_t out_f = {.buffer = opus_buf, .len = (uint32_t)out_sz};

        int64_t t0 = esp_timer_get_time();
        err = esp_opus_enc_process(enc_hd, &in_f, &out_f);
        int64_t t1 = esp_timer_get_time();
        if (err != ESP_AUDIO_ERR_OK) {
            ESP_LOGE(TAG, "Benchmark enc error: %d", err);
            break;
        }
        uint64_t dt_enc = (uint64_t)(t1 - t0);
        enc_sum_us += dt_enc;
        if (dt_enc > enc_max_us) enc_max_us = dt_enc;

        esp_audio_dec_in_raw_t raw = {.buffer = mono_opus, .len = m_out.encoded_bytes};
        esp_audio_dec_out_frame_t dec_f = {.buffer = (uint8_t *)pcm_out, .len = MONO_PCM_BYTES};
        esp_audio_dec_info_t dec_info = {0};

        int64_t t2 = esp_timer_get_time();
        err = esp_opus_dec_decode(dec_hd, &raw, &dec_f, &dec_info);
        int64_t t3 = esp_timer_get_time();
        if (err != ESP_AUDIO_ERR_OK) {
            ESP_LOGE(TAG, "Benchmark dec error: %d", err);
            break;
        }
        uint64_t dt_dec = (uint64_t)(t3 - t2);
        dec_sum_us += dt_dec;
        if (dt_dec > dec_max_us) dec_max_us = dt_dec;
    }

    double enc_avg_ms = (double)enc_sum_us / N / 1000.0;
    double dec_avg_ms = (double)dec_sum_us / N / 1000.0;
    double total_avg_ms = enc_avg_ms + dec_avg_ms;
    double budget_pct = (total_avg_ms / 20.0) * 100.0;

    ESP_LOGI(TAG, ">>> BENCHMARK [Enc=%dch, comp=%d + Dec=1ch]: Enc=%.2fms (max %.2fms), "
                  "Dec=%.2fms (max %.2fms) | Total=%.2fms / 20ms (%.1f%% Core1) <<<",
             enc_channels, complexity,
             enc_avg_ms, (double)enc_max_us / 1000.0,
             dec_avg_ms, (double)dec_max_us / 1000.0,
             total_avg_ms, budget_pct);

    free(mono_pcm);
    free(mono_opus);
    free(pcm_in);
    free(opus_buf);
    free(pcm_out);
    esp_opus_enc_close(enc_hd);
    esp_opus_dec_close(dec_hd);
}

//--------------------------------------------------------------------+
// UAC 2.0 Callbacks
//--------------------------------------------------------------------+
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
    if (s_mic_rb) {
        uint8_t *item = xRingbufferReceiveUpTo(s_mic_rb, &got, 0, len);
        if (item) {
            memcpy(buf, item, got);
            vRingbufferReturnItem(s_mic_rb, item);
        }
    }
    if (got < len) {
        memset(buf + got, 0, len - got);
    }
    *bytes_read = len;
    return ESP_OK;
}

//--------------------------------------------------------------------+
// Core 1 Real-Time Full-Duplex Opus Audio Loopback Task
//--------------------------------------------------------------------+
static void audio_duplex_task(void *arg)
{
    (void)arg;
    // Run offline benchmarks first so user sees results immediately on boot
    run_codec_benchmark(2, 0);
    run_codec_benchmark(1, 0);
    run_codec_benchmark(1, 2);

    // Open persistent Mono Opus Encoder + Decoder for live UAC loopback
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
    ESP_ERROR_CHECK(esp_opus_enc_open(&enc_cfg, sizeof(enc_cfg), &enc_hd) == ESP_AUDIO_ERR_OK ? ESP_OK : ESP_FAIL);
    ESP_ERROR_CHECK(esp_opus_dec_open(&dec_cfg, sizeof(dec_cfg), &dec_hd) == ESP_AUDIO_ERR_OK ? ESP_OK : ESP_FAIL);

    int16_t *stereo_pcm = malloc(STEREO_PCM_BYTES);
    int16_t *mono_pcm = malloc(MONO_PCM_BYTES);
    uint8_t *opus_pkt = malloc(512);
    int16_t *dec_pcm = malloc(MONO_PCM_BYTES);

    ESP_LOGI(TAG, "Live UAC Full-Duplex Loopback ready: Host Speaker -> Opus Enc -> Opus Dec -> Host Mic");

    TickType_t last_wake = xTaskGetTickCount();
    uint32_t report_ticks = 0;

    while (1) {
        // Pull 20ms (3840 bytes) stereo PCM from UAC speaker ringbuf, downmix to mono
        size_t collected = 0;
        uint8_t *dst = (uint8_t *)stereo_pcm;
        while (collected < STEREO_PCM_BYTES) {
            size_t got = 0;
            uint8_t *item = xRingbufferReceiveUpTo(s_spk_rb, &got, 0, STEREO_PCM_BYTES - collected);
            if (!item || got == 0) {
                break;
            }
            memcpy(dst + collected, item, got);
            vRingbufferReturnItem(s_spk_rb, item);
            collected += got;
        }

        if (collected == STEREO_PCM_BYTES) {
            for (int i = 0; i < FRAME_SAMPLES; i++) {
                int32_t l = stereo_pcm[2 * i];
                int32_t r = stereo_pcm[2 * i + 1];
                mono_pcm[i] = (int16_t)((l + r) / 2);
            }

            esp_audio_enc_in_frame_t in_f = {.buffer = (uint8_t *)mono_pcm, .len = MONO_PCM_BYTES};
            esp_audio_enc_out_frame_t out_f = {.buffer = opus_pkt, .len = 512};
            int64_t t0 = esp_timer_get_time();
            esp_audio_err_t er = esp_opus_enc_process(enc_hd, &in_f, &out_f);
            int64_t t1 = esp_timer_get_time();

            if (er == ESP_AUDIO_ERR_OK && out_f.encoded_bytes > 0) {
                esp_audio_dec_in_raw_t raw = {.buffer = opus_pkt, .len = out_f.encoded_bytes};
                esp_audio_dec_out_frame_t dec_f = {.buffer = (uint8_t *)dec_pcm, .len = MONO_PCM_BYTES};
                esp_audio_dec_info_t info = {0};
                int64_t t2 = esp_timer_get_time();
                esp_audio_err_t dr = esp_opus_dec_decode(dec_hd, &raw, &dec_f, &info);
                int64_t t3 = esp_timer_get_time();

                if (dr == ESP_AUDIO_ERR_OK) {
                    xRingbufferSend(s_mic_rb, dec_pcm, MONO_PCM_BYTES, 0);
                    uint32_t dt_e = (uint32_t)(t1 - t0);
                    uint32_t dt_d = (uint32_t)(t3 - t2);
                    s_live_enc_us += dt_e;
                    s_live_dec_us += dt_d;
                    if (dt_e > s_live_enc_max_us) s_live_enc_max_us = dt_e;
                    if (dt_d > s_live_dec_max_us) s_live_dec_max_us = dt_d;
                    s_live_frames++;
                }
            }
        }

        if (++report_ticks >= 100 && s_live_frames > 0) {
            report_ticks = 0;
            uint32_t avg_e = (uint32_t)(s_live_enc_us / s_live_frames);
            uint32_t avg_d = (uint32_t)(s_live_dec_us / s_live_frames);
            ESP_LOGI(TAG, "Live Duplex (%lu frames): enc=%.2fms (max %.2fms), dec=%.2fms (max %.2fms), free_heap=%lu",
                     (unsigned long)s_live_frames,
                     avg_e / 1000.0, s_live_enc_max_us / 1000.0,
                     avg_d / 1000.0, s_live_dec_max_us / 1000.0,
                     (unsigned long)esp_get_free_heap_size());
            s_live_enc_us = s_live_dec_us = 0;
            s_live_enc_max_us = s_live_dec_max_us = 0;
            s_live_frames = 0;
        }

        vTaskDelayUntil(&last_wake, pdMS_TO_TICKS(20));
    }
}

void app_main(void)
{
    ESP_LOGI(TAG, "Stage-1 PoC-4 starting (CPU=240MHz, PSRAM=%zu bytes)",
             esp_psram_is_initialized() ? esp_psram_get_size() : 0);

    s_spk_rb = xRingbufferCreate(STEREO_PCM_BYTES * 6, RINGBUF_TYPE_BYTEBUF);
    s_mic_rb = xRingbufferCreate(MONO_PCM_BYTES * 6, RINGBUF_TYPE_BYTEBUF);

    uac_device_config_t uac_cfg = {
        .skip_tinyusb_init = false,
        .output_cb = uac_output_cb,
        .input_cb = uac_input_cb,
        .cb_ctx = NULL,
    };
    ESP_ERROR_CHECK(uac_device_init(&uac_cfg));

    // Large stack (64KB) on Core 1 for Opus CELT encoder/decoder
    xTaskCreatePinnedToCore(audio_duplex_task, "opus_duplex", 65536, NULL, 5, NULL, 1);
}

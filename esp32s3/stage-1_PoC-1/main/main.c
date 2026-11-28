#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "driver/gpio.h"
#include "esp_log.h"
#include "esp_psram.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "tusb.h"
#include "usb_descriptors.h"
#include "usb_device_uac.h"

static const char *TAG = "stage1_poc1";

#define BOOT_BUTTON_GPIO GPIO_NUM_0
#define SAMPLE_RATE_HZ   48000
#define TONE_FREQ_HZ     440.0f

// Audio state
static volatile bool s_tone_enabled = true;
static float s_tone_phase = 0.0f;
static uint32_t s_spk_bytes_total = 0;
static int16_t s_spk_peak = 0;

// Rumble state from host
static volatile uint8_t s_rumble_right = 0;
static volatile uint8_t s_rumble_left = 0;
static volatile bool s_rumble_dirty = false;

//--------------------------------------------------------------------+
// DualShock 4 USB Input Report (64 bytes, Report ID 0x01)
//--------------------------------------------------------------------+
typedef struct __attribute__((packed)) {
    uint8_t lx;
    uint8_t ly;
    uint8_t rx;
    uint8_t ry;
    uint8_t buttons0; // [3:0] D-pad hat (8=released), [4]Square, [5]Cross, [6]Circle, [7]Triangle
    uint8_t buttons1; // L1, R1, L2, R2, Share, Options, L3, R3
    uint8_t buttons2; // [0]PS, [1]Touchpad, [7:2]Counter
    uint8_t l2_trigger;
    uint8_t r2_trigger;
    uint8_t padding[54]; // Timestamp, IMU, Touchpad, Battery (zeroed for PoC)
} ds4_input_report_t;

_Static_assert(sizeof(ds4_input_report_t) == 63, "DS4 report payload must be 63 bytes");

//--------------------------------------------------------------------+
// TinyUSB Device & HID Callbacks
//--------------------------------------------------------------------+
void tud_mount_cb(void)
{
    ESP_LOGI(TAG, "USB Mounted on host (DS4 HID + UAC 2.0 Bidirectional)");
}

void tud_umount_cb(void)
{
    ESP_LOGI(TAG, "USB Unmounted");
}

void tud_suspend_cb(bool remote_wakeup_en)
{
    (void)remote_wakeup_en;
    ESP_LOGI(TAG, "USB Suspended");
}

void tud_resume_cb(void)
{
    ESP_LOGI(TAG, "USB Resumed");
}

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t report_type,
                               uint8_t *buffer, uint16_t reqlen)
{
    (void)instance;
    if (report_type != HID_REPORT_TYPE_FEATURE) {
        return 0;
    }

    if (report_id == 0x02 && reqlen >= 36) {
        // Minimal neutral IMU/Stick calibration block
        memset(buffer, 0, 36);
        return 36;
    }
    if (report_id == 0x12 && reqlen >= 15) {
        // Dummy Controller MAC + Host MAC
        static const uint8_t mac_info[15] = {
            0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01, 0x08, 0x25, 0x00,
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66
        };
        memcpy(buffer, mac_info, 15);
        return 15;
    }
    if (report_id == 0xA3 && reqlen >= 48) {
        // Minimal DS4 firmware/build date string
        memset(buffer, 0, 48);
        memcpy(buffer, "Nov 28 2026", 11);
        memcpy(buffer + 16, "15:30:00", 8);
        return 48;
    }
    return 0;
}

void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t report_type,
                           uint8_t const *buffer, uint16_t bufsize)
{
    (void)instance;
    (void)report_type;

    // TinyUSB strips Report ID when delivered via control/OUT if report_id != 0,
    // or keeps it at buffer[0] when report_id == 0. Handle both cleanly.
    uint8_t const *payload = buffer;
    uint16_t len = bufsize;
    uint8_t effective_id = report_id;

    if (effective_id == 0 && len > 0) {
        effective_id = buffer[0];
        payload = buffer + 1;
        len--;
    }

    if (effective_id == 0x05 && len >= 5) {
        // DS4 Output Report 0x05: payload[3] = right (weak) motor, payload[4] = left (strong) motor
        s_rumble_right = payload[3];
        s_rumble_left = payload[4];
        s_rumble_dirty = true;
    }
}

//--------------------------------------------------------------------+
// UAC 2.0 Audio Callbacks (Speaker OUT + Microphone IN)
//--------------------------------------------------------------------+
static esp_err_t uac_output_cb(uint8_t *buf, size_t len, void *cb_ctx)
{
    (void)cb_ctx;
    s_spk_bytes_total += (uint32_t)len;

    const int16_t *samples = (const int16_t *)buf;
    size_t count = len / sizeof(int16_t);
    int16_t peak = s_spk_peak;
    for (size_t i = 0; i < count; i++) {
        int16_t v = samples[i];
        if (v < 0) {
            v = (v == INT16_MIN) ? INT16_MAX : -v;
        }
        if (v > peak) {
            peak = v;
        }
    }
    s_spk_peak = peak;
    return ESP_OK;
}

static esp_err_t uac_input_cb(uint8_t *buf, size_t len, size_t *bytes_read, void *cb_ctx)
{
    (void)cb_ctx;
    int16_t *samples = (int16_t *)buf;
    size_t count = len / sizeof(int16_t);

    if (!s_tone_enabled) {
        memset(buf, 0, len);
    } else {
        const float phase_step = (2.0f * (float)M_PI * TONE_FREQ_HZ) / (float)SAMPLE_RATE_HZ;
        for (size_t i = 0; i < count; i++) {
            samples[i] = (int16_t)(sinf(s_tone_phase) * 6000.0f);
            s_tone_phase += phase_step;
            if (s_tone_phase >= 2.0f * (float)M_PI) {
                s_tone_phase -= 2.0f * (float)M_PI;
            }
        }
    }
    *bytes_read = count * sizeof(int16_t);
    return ESP_OK;
}

static void uac_set_mute_cb(uint32_t mute, void *cb_ctx)
{
    (void)cb_ctx;
    ESP_LOGI(TAG, "Host set speaker mute: %lu", (unsigned long)mute);
}

static void uac_set_volume_cb(uint32_t volume, void *cb_ctx)
{
    (void)cb_ctx;
    ESP_LOGI(TAG, "Host set speaker volume: %lu", (unsigned long)volume);
}

//--------------------------------------------------------------------+
// Application Task: HID Test Pattern + Telemetry
//--------------------------------------------------------------------+
static void app_demo_task(void *arg)
{
    (void)arg;
    ds4_input_report_t report;
    uint32_t tick_count = 0;
    uint8_t seq = 0;
    int prev_btn = 1;

    while (1) {
        int btn = gpio_get_level(BOOT_BUTTON_GPIO);
        if (prev_btn == 1 && btn == 0) {
            s_tone_enabled = !s_tone_enabled;
            ESP_LOGI(TAG, "BOOT pressed -> Mic 440Hz tone %s, sending Cross button",
                     s_tone_enabled ? "ENABLED" : "MUTED");
        }
        prev_btn = btn;

        if (tud_mounted() && tud_hid_ready()) {
            memset(&report, 0, sizeof(report));
            // Smooth circle on Left Stick for easy visual verification in Gamepad Tester
            float angle = (float)(tick_count % 100) * (2.0f * (float)M_PI / 100.0f);
            report.lx = (uint8_t)(128.0f + 90.0f * cosf(angle));
            report.ly = (uint8_t)(128.0f + 90.0f * sinf(angle));
            report.rx = 128;
            report.ry = 128;
            report.buttons0 = 0x08; // D-pad released
            if (btn == 0) {
                report.buttons0 |= (1 << 5); // Cross button held while BOOT is pressed
            }
            report.buttons2 = (uint8_t)((seq++ & 0x3F) << 2);

            tud_hid_report(0x01, &report, sizeof(report));
        }

        if (s_rumble_dirty) {
            s_rumble_dirty = false;
            ESP_LOGI(TAG, "Host Rumble -> Left(strong)=%u, Right(weak)=%u",
                     s_rumble_left, s_rumble_right);
        }

        // Log speaker audio stream stats once every 2 seconds
        if ((tick_count % 100) == 0 && s_spk_bytes_total > 0) {
            ESP_LOGI(TAG, "UAC Speaker RX: %lu bytes/2s, peak=%d",
                     (unsigned long)s_spk_bytes_total, s_spk_peak);
            s_spk_bytes_total = 0;
            s_spk_peak = 0;
        }

        tick_count++;
        vTaskDelay(pdMS_TO_TICKS(20));
    }
}

void app_main(void)
{
    ESP_LOGI(TAG, "Stage-1 PoC-1 starting (PSRAM size: %zu bytes)",
             esp_psram_is_initialized() ? esp_psram_get_size() : 0);

    gpio_config_t io_conf = {
        .pin_bit_mask = (1ULL << BOOT_BUTTON_GPIO),
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
    };
    gpio_config(&io_conf);

    uac_device_config_t uac_cfg = {
        .skip_tinyusb_init = false,
        .output_cb = uac_output_cb,
        .input_cb = uac_input_cb,
        .set_mute_cb = uac_set_mute_cb,
        .set_volume_cb = uac_set_volume_cb,
        .cb_ctx = NULL,
        .spk_itf_num = ITF_NUM_AUDIO_STREAMING_SPK,
        .mic_itf_num = ITF_NUM_AUDIO_STREAMING_MIC,
    };
    ESP_ERROR_CHECK(uac_device_init(&uac_cfg));

    xTaskCreatePinnedToCore(app_demo_task, "app_demo", 4096, NULL, 4, NULL, 1);
}

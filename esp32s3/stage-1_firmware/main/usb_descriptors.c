#include <string.h>
#include "tusb.h"
#include "uac_descriptors.h"
#include "usb_descriptors.h"

// Sony DualShock 4 v2 (CUH-ZCT2x) — natively supported driverless on macOS, Windows, Linux
#define DS4_VID 0x054C
#define DS4_PID 0x09CC

//--------------------------------------------------------------------+
// Device Descriptor
//--------------------------------------------------------------------+
static tusb_desc_device_t const desc_device = {
    .bLength            = sizeof(tusb_desc_device_t),
    .bDescriptorType    = TUSB_DESC_DEVICE,
    .bcdUSB             = 0x0200,

    // Use IAD for composite Audio + HID
    .bDeviceClass       = TUSB_CLASS_MISC,
    .bDeviceSubClass    = MISC_SUBCLASS_COMMON,
    .bDeviceProtocol    = MISC_PROTOCOL_IAD,
    .bMaxPacketSize0    = CFG_TUD_ENDPOINT0_SIZE,

    .idVendor           = DS4_VID,
    .idProduct          = DS4_PID,
    .bcdDevice          = 0x0100,

    .iManufacturer      = 0x01,
    .iProduct           = 0x02,
    .iSerialNumber      = 0x03,

    .bNumConfigurations = 0x01
};

uint8_t const *tud_descriptor_device_cb(void)
{
    return (uint8_t const *)&desc_device;
}

//--------------------------------------------------------------------+
// DualShock 4 USB HID Report Descriptor
//--------------------------------------------------------------------+
static uint8_t const desc_hid_report[] = {
    0x05, 0x01,        // Usage Page (Generic Desktop Ctrls)
    0x09, 0x05,        // Usage (Game Pad)
    0xA1, 0x01,        // Collection (Application)
    0x85, 0x01,        //   Report ID (1)
    0x09, 0x30,        //   Usage (X)
    0x09, 0x31,        //   Usage (Y)
    0x09, 0x32,        //   Usage (Z)
    0x09, 0x35,        //   Usage (Rz)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x04,        //   Report Count (4)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x09, 0x39,        //   Usage (Hat switch)
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x07,        //   Logical Maximum (7)
    0x35, 0x00,        //   Physical Minimum (0)
    0x46, 0x3B, 0x01,  //   Physical Maximum (315)
    0x65, 0x14,        //   Unit (System: English Rotation, Length: Centimeter)
    0x75, 0x04,        //   Report Size (4)
    0x95, 0x01,        //   Report Count (1)
    0x81, 0x42,        //   Input (Data,Var,Abs,Null)
    0x65, 0x00,        //   Unit (None)
    0x05, 0x09,        //   Usage Page (Button)
    0x19, 0x01,        //   Usage Minimum (0x01)
    0x29, 0x0E,        //   Usage Maximum (0x0E) - 14 buttons
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x01,        //   Logical Maximum (1)
    0x75, 0x01,        //   Report Size (1)
    0x95, 0x0E,        //   Report Count (14)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x06, 0x00, 0xFF,  //   Usage Page (Vendor Defined 0xFF00)
    0x09, 0x20,        //   Usage (0x20)
    0x75, 0x06,        //   Report Size (6)
    0x95, 0x01,        //   Report Count (1)
    0x15, 0x00,        //   Logical Minimum (0)
    0x25, 0x3F,        //   Logical Maximum (63)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x05, 0x01,        //   Usage Page (Generic Desktop Ctrls)
    0x09, 0x33,        //   Usage (Rx) - L2 trigger
    0x09, 0x34,        //   Usage (Ry) - R2 trigger
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x02,        //   Report Count (2)
    0x81, 0x02,        //   Input (Data,Var,Abs)
    0x06, 0x00, 0xFF,  //   Usage Page (Vendor Defined 0xFF00)
    0x09, 0x21,        //   Usage (0x21)
    0x95, 0x36,        //   Report Count (54) - Remaining DS4 input bytes (total 64 bytes with Report ID)
    0x81, 0x02,        //   Input (Data,Var,Abs)

    // Output Report 0x05 (Rumble + Lightbar RGB)
    0x85, 0x05,        //   Report ID (5)
    0x09, 0x22,        //   Usage (0x22)
    0x95, 0x1F,        //   Report Count (31)
    0x91, 0x02,        //   Output (Data,Var,Abs)

    // Feature Report 0x02 (IMU/Stick Calibration)
    0x85, 0x02,        //   Report ID (2)
    0x09, 0x24,        //   Usage (0x24)
    0x95, 0x24,        //   Report Count (36)
    0xB1, 0x02,        //   Feature (Data,Var,Abs)

    // Feature Report 0x12 (Pairing MAC Info)
    0x85, 0x12,        //   Report ID (18)
    0x09, 0x26,        //   Usage (0x26)
    0x95, 0x0F,        //   Report Count (15)
    0xB1, 0x02,        //   Feature (Data,Var,Abs)

    // Feature Report 0xA3 (Firmware Version/Date)
    0x85, 0xA3,        //   Report ID (163)
    0x09, 0x25,        //   Usage (0x25)
    0x95, 0x30,        //   Report Count (48)
    0xB1, 0x02,        //   Feature (Data,Var,Abs)
    0xC0               // End Collection
};

uint8_t const *tud_hid_descriptor_report_cb(uint8_t instance)
{
    (void)instance;
    return desc_hid_report;
}

//--------------------------------------------------------------------+
// Configuration Descriptor (UAC 2.0 Stereo Spk + Mono Mic + DS4 HID)
//--------------------------------------------------------------------+
// Endpoint Budget on ESP32-S3 (Limit: EP0 + 6 EPs, max 5 IN EPs):
//   EP0: Control (1 IN, 1 OUT)
//   0x01: UAC Speaker OUT
//   0x81: UAC Speaker Feedback IN
//   0x82: UAC Microphone IN
//   0x02: DS4 HID OUT (Rumble / LED)
//   0x83: DS4 HID IN  (Buttons / Sticks)
// Total active IN = 4 (<= 5), Total active OUT = 3.
#define EPNUM_AUDIO_OUT   0x01
#define EPNUM_AUDIO_FB    0x81
#define EPNUM_AUDIO_IN    0x82
#define EPNUM_HID_OUT     0x03
#define EPNUM_HID_IN      0x83

#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_AUDIO_DEVICE_DESC_LEN + TUD_HID_INOUT_DESC_LEN)

static uint8_t const desc_configuration[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_NUM_TOTAL, 0, CONFIG_TOTAL_LEN, TUSB_DESC_CONFIG_ATT_REMOTE_WAKEUP, 250),

    // UAC 2.0 Interfaces (0, 1, 2)
    TUD_AUDIO_DESCRIPTOR(ITF_NUM_AUDIO_CONTROL, 4, EPNUM_AUDIO_OUT, EPNUM_AUDIO_IN, EPNUM_AUDIO_FB),

    // DS4 HID Interface (3)
    TUD_HID_INOUT_DESCRIPTOR(ITF_NUM_HID, 7, HID_ITF_PROTOCOL_NONE, sizeof(desc_hid_report),
                             EPNUM_HID_OUT, EPNUM_HID_IN, CFG_TUD_HID_EP_BUFSIZE, 5),
};

uint8_t const *tud_descriptor_configuration_cb(uint8_t index)
{
    (void)index;
    return desc_configuration;
}

//--------------------------------------------------------------------+
// String Descriptors
//--------------------------------------------------------------------+
static char const *string_desc_arr[] = {
    (const char[]){0x09, 0x04},       // 0: English (0x0409)
    "StadiaWave (Sony DS4 Emul)",     // 1: Manufacturer
    "Stadia Wireless Adapter PoC-1",  // 2: Product
    "SW-POC1-0001",                   // 3: Serial
    "StadiaWave Audio",               // 4: UAC Control
    "StadiaWave Headset",             // 5: UAC Speaker
    "StadiaWave Microphone",          // 6: UAC Mic
    "StadiaWave Gamepad",             // 7: DS4 HID
};

static uint16_t _desc_str[32];

uint16_t const *tud_descriptor_string_cb(uint8_t index, uint16_t langid)
{
    (void)langid;
    uint8_t chr_count;

    if (index == 0) {
        memcpy(&_desc_str[1], string_desc_arr[0], 2);
        chr_count = 1;
    } else {
        if (index >= sizeof(string_desc_arr) / sizeof(string_desc_arr[0])) {
            return NULL;
        }
        const char *str = string_desc_arr[index];
        chr_count = (uint8_t)strlen(str);
        if (chr_count > 31) {
            chr_count = 31;
        }
        for (uint8_t i = 0; i < chr_count; i++) {
            _desc_str[1 + i] = str[i];
        }
    }

    _desc_str[0] = (uint16_t)((TUSB_DESC_STRING << 8) | (2 * chr_count + 2));
    return _desc_str;
}

#ifndef ExtenderProtocol_h
#define ExtenderProtocol_h

#include <stdint.h>

// Maximum number of simultaneous virtual devices
#define EXTENDER_MAX_DEVICES 32

// Maximum HID report descriptor size (4 KB)
#define EXTENDER_MAX_REPORT_DESCRIPTOR_SIZE 4096

// Maximum data transfer size per ExternalMethod call (64 KB)
#define EXTENDER_MAX_TRANSFER_SIZE 65536

// Maximum pending output queue depth per device
#define EXTENDER_MAX_PENDING_OUTPUTS 64

// Device types corresponding to USB class codes
enum ExtenderDeviceType : uint32_t {
    kDeviceTypeHID     = 1,
    kDeviceTypeStorage = 2,
    kDeviceTypeSerial  = 3,
    kDeviceTypeNetwork = 4,
    kDeviceTypeAudio   = 5,
};

// UserClient selectors (daemon -> driver)
enum ExtenderSelector : uint64_t {
    // Import side (virtual device, daemon -> virtual device -> macOS)
    kCreateDevice            = 0,  // Create a virtual device
    kDestroyDevice           = 1,  // Remove a virtual device
    kSubmitInput             = 2,  // Send data from network to virtual device (URB response)
    kGetPendingOutput        = 3,  // Poll for data from virtual device to send over network (URB request)
    kGetDeviceCount          = 4,  // Query number of active virtual devices
    kConfigureDevice         = 5,  // Set device descriptors/parameters

    // Export side (real USB device, macOS -> exported via daemon -> network)
    kExportClaimDevice       = 6,  // Claim a real USB device by locationID for export. In: locationID. Out: exportSlotId.
    kExportReleaseDevice     = 7,  // Release an exported device. In: exportSlotId.
    kExportSubmitURB         = 8,  // Send a URB (e.g., control transfer, bulk OUT) to the claimed device. In: exportSlotId, endpoint, urbType + data buffer.
    kExportGetPendingResponse = 9, // Poll for asynchronous responses from the claimed device (e.g., interrupt IN). In: exportSlotId.

    // Import-side async completion. Daemon calls this to fulfill a request that
    // was enqueued via kGetPendingOutput. In: deviceId, requestId, status. data:
    // payload to satisfy the original getReport / setReport / SCSI READ.
    kCompleteRequest         = 10,
};

// Device configuration subtypes for kConfigureDevice
enum ExtenderConfigType : uint32_t {
    kConfigHIDReportDescriptor = 1,  // Set HID report descriptor
    kConfigStorageGeometry     = 2,  // Set block count + block size
    kConfigSerialLineState     = 3,  // Set baud/parity/stop
    kConfigNetworkMAC          = 4,  // Set MAC address
    kConfigAudioFormat         = 5,  // Set sample rate, channels, bit depth
    kConfigDeviceDescriptor    = 6,  // Set USB device descriptor info (VID/PID/name)
};

// Shared structure for USB device descriptor configuration
struct ExtenderDeviceDescriptorConfig {
    uint16_t vendorId;
    uint16_t productId;
    uint16_t bcdDevice;
    char     manufacturer[64];
    char     product[64];
    char     serialNumber[64];
};

// Shared structure for storage geometry
struct ExtenderStorageGeometry {
    uint64_t blockCount;
    uint32_t blockSize;
    uint32_t reserved;
};

// Shared structure for serial line state
struct ExtenderSerialLineState {
    uint32_t baudRate;
    uint8_t  dataBits;
    uint8_t  parity;    // 0=none, 1=odd, 2=even
    uint8_t  stopBits;  // 1 or 2
    uint8_t  reserved;
};

// Shared structure for network MAC address
struct ExtenderNetworkMAC {
    uint8_t addr[6];
    uint8_t reserved[2];
};

// Shared structure for audio format
struct ExtenderAudioFormat {
    uint32_t sampleRate;
    uint16_t channels;
    uint16_t bitDepth;
};

// Pending output request header (returned by kGetPendingOutput)
struct ExtenderPendingOutputHeader {
    uint32_t deviceId;
    uint32_t endpoint;
    uint32_t dataLength;
    uint32_t requestType;  // 0 = bulk out, 1 = interrupt out, 2 = control
    uint32_t requestId;    // Daemon echoes this back via kCompleteRequest to satisfy async ops
    uint32_t reserved;
    // Followed by dataLength bytes of data
};

#endif /* ExtenderProtocol_h */

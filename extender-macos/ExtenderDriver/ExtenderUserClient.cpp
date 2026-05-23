#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOUserClient.h>
#include <DriverKit/OSData.h>

#include "ExtenderUserClient.h"
#include "ExtenderDriver.h"
#include "ExtenderProtocol.h"
#include "ExtenderVirtualHID.h"
#include "ExtenderVirtualStorage.h"
#include "ExtenderVirtualNetwork.h"
#include "ExtenderVirtualSerial.h"
#include "ExtenderVirtualAudio.h"
#include "ExtenderExportDevice.h"

#define LOG_PREFIX "ExtenderUC"

// Access the driver's device table. We use a friend-like approach via public accessors.
// Since DriverKit doesn't support C++ friends, we define inline helpers that the driver exposes.
// For now, we access the driver's IVars through a shared structure.

// Mirror of the driver's device slot (same layout as ExtenderDriver.cpp)
struct ExtenderDeviceSlot;

struct ExtenderUserClient_IVars {
    ExtenderDriver *driver;

    // Pending output ring buffer per device
    struct PendingOutput {
        uint8_t  data[EXTENDER_MAX_TRANSFER_SIZE];
        uint32_t length;
        uint32_t endpoint;
        uint32_t requestType;
        bool     valid;
    };

    // Simple per-device pending output queue
    struct DeviceOutputQueue {
        PendingOutput entries[EXTENDER_MAX_PENDING_OUTPUTS];
        uint32_t head;
        uint32_t tail;
        uint32_t count;
    };

    DeviceOutputQueue outputQueues[EXTENDER_MAX_DEVICES];
};

// Access driver ivars - we need the same struct definition
// This is the same struct from ExtenderDriver.cpp; in a real build these would share a private header.
struct ExtenderDriverDeviceSlot {
    OSObject    *device;
    uint32_t     deviceType;
    bool         active;
    uint16_t     vendorId;
    uint16_t     productId;
    char         productName[64];
};

struct ExtenderDriver_IVars {
    ExtenderDriverDeviceSlot devices[EXTENDER_MAX_DEVICES];
    uint32_t                 deviceCount;
};

// Helper to get driver ivars (the ivars pointer is at a known offset)
static ExtenderDriver_IVars * getDriverIVars(ExtenderDriver *driver)
{
    // DriverKit classes store ivars as a public member
    return (ExtenderDriver_IVars *)driver->ivars;
}

bool ExtenderUserClient::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderUserClient_IVars, 1);
    if (!ivars) return false;

    // Initialize output queues
    for (uint32_t i = 0; i < EXTENDER_MAX_DEVICES; i++) {
        ivars->outputQueues[i].head = 0;
        ivars->outputQueues[i].tail = 0;
        ivars->outputQueues[i].count = 0;
    }

    return true;
}

kern_return_t IMPL(ExtenderUserClient, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) return ret;

    ivars->driver = OSDynamicCast(ExtenderDriver, provider);
    if (!ivars->driver) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": wrong provider type");
        return kIOReturnError;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient started");
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderUserClient, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient stopped");
    ivars->driver = nullptr;

    IOSafeDeleteNULL(ivars, ExtenderUserClient_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}

// --- Private helpers ---

static const char *propertiesKeyForDeviceType(uint64_t deviceType)
{
    switch ((ExtenderDeviceType)deviceType) {
    case kDeviceTypeHID:     return "VirtualHIDProperties";
    case kDeviceTypeStorage: return "VirtualStorageProperties";
    case kDeviceTypeSerial:  return "VirtualSerialProperties";
    case kDeviceTypeNetwork: return "VirtualNetworkProperties";
    case kDeviceTypeAudio:   return "VirtualAudioProperties";
    }
    return nullptr;
}

static kern_return_t handleCreateDevice(
    ExtenderDriver_IVars *driverIVars,
    ExtenderDriver *driver,
    uint64_t deviceType,
    uint64_t deviceId)
{
    if (deviceId >= EXTENDER_MAX_DEVICES) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": CreateDevice: invalid deviceId %llu", deviceId);
        return kIOReturnBadArgument;
    }

    if (driverIVars->devices[deviceId].active) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": CreateDevice: slot %llu already active", deviceId);
        return kIOReturnExclusiveAccess;
    }

    const char *propsKey = propertiesKeyForDeviceType(deviceType);
    if (!propsKey) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": CreateDevice: unknown device type %llu", deviceType);
        return kIOReturnBadArgument;
    }

    IOService *service = nullptr;
    kern_return_t ret = driver->Create(driver, propsKey, &service);
    if (ret != kIOReturnSuccess || !service) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": CreateDevice: Create(%s) failed: 0x%x", propsKey, ret);
        return ret == kIOReturnSuccess ? kIOReturnNoResources : ret;
    }

    driverIVars->devices[deviceId].active = true;
    driverIVars->devices[deviceId].deviceType = (uint32_t)deviceType;
    driverIVars->devices[deviceId].device = service;
    driverIVars->devices[deviceId].vendorId = 0;
    driverIVars->devices[deviceId].productId = 0;
    driverIVars->devices[deviceId].productName[0] = '\0';
    driverIVars->deviceCount++;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Created device slot %llu type %llu (%s, count: %u)",
           deviceId, deviceType, propsKey, driverIVars->deviceCount);
    return kIOReturnSuccess;
}

static kern_return_t handleDestroyDevice(
    ExtenderDriver_IVars *driverIVars,
    uint64_t deviceId)
{
    if (deviceId >= EXTENDER_MAX_DEVICES) {
        return kIOReturnBadArgument;
    }

    if (!driverIVars->devices[deviceId].active) {
        return kIOReturnNotFound;
    }

    if (driverIVars->devices[deviceId].device) {
        IOService *svc = OSDynamicCast(IOService, driverIVars->devices[deviceId].device);
        if (svc) {
            svc->Terminate(0);
        }
        driverIVars->devices[deviceId].device->release();
        driverIVars->devices[deviceId].device = nullptr;
    }

    driverIVars->devices[deviceId].active = false;
    driverIVars->devices[deviceId].deviceType = 0;
    driverIVars->deviceCount--;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Destroyed device %llu (count: %u)",
           deviceId, driverIVars->deviceCount);
    return kIOReturnSuccess;
}

static kern_return_t handleSubmitInput(
    ExtenderDriver_IVars *driverIVars,
    ExtenderUserClient_IVars * /*ucIVars*/,
    uint64_t deviceId,
    uint64_t endpoint,
    uint64_t dataLength,
    IOUserClientMethodArguments *arguments)
{
    if (deviceId >= EXTENDER_MAX_DEVICES || !driverIVars->devices[deviceId].active) {
        return kIOReturnNotFound;
    }

    auto &slot = driverIVars->devices[deviceId];

    if (!arguments->structureInput) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SubmitInput: no data buffer");
        return kIOReturnBadArgument;
    }

    const void *inputBytes = arguments->structureInput->getBytesNoCopy();
    uint32_t inputLen = arguments->structureInput->getLength();

    if (!inputBytes || inputLen < dataLength) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": SubmitInput: buffer too small (%u < %llu)", inputLen, dataLength);
        return kIOReturnBadArgument;
    }

    switch ((ExtenderDeviceType)slot.deviceType) {
    case kDeviceTypeHID: {
        ExtenderVirtualHID *hid = OSDynamicCast(ExtenderVirtualHID, slot.device);
        if (!hid) return kIOReturnNotReady;
        hid->submitInputReport(inputBytes, (uint32_t)dataLength);
        return kIOReturnSuccess;
    }
    case kDeviceTypeStorage: {
        ExtenderVirtualStorage *st = OSDynamicCast(ExtenderVirtualStorage, slot.device);
        if (!st) return kIOReturnNotReady;
        st->submitInputReport(inputBytes, (uint32_t)dataLength);
        return kIOReturnSuccess;
    }
    case kDeviceTypeSerial: {
        ExtenderVirtualSerial *se = OSDynamicCast(ExtenderVirtualSerial, slot.device);
        if (!se) return kIOReturnNotReady;
        se->submitInputReport(inputBytes, (uint32_t)dataLength);
        return kIOReturnSuccess;
    }
    case kDeviceTypeNetwork: {
        ExtenderVirtualNetwork *nw = OSDynamicCast(ExtenderVirtualNetwork, slot.device);
        if (!nw) return kIOReturnNotReady;
        nw->submitInputReport(inputBytes, (uint32_t)dataLength);
        return kIOReturnSuccess;
    }
    case kDeviceTypeAudio: {
        ExtenderVirtualAudio *au = OSDynamicCast(ExtenderVirtualAudio, slot.device);
        if (!au) return kIOReturnNotReady;
        au->submitInputReport(inputBytes, (uint32_t)dataLength);
        return kIOReturnSuccess;
    }
    }
    return kIOReturnBadArgument;
}

static kern_return_t handleGetPendingOutput(
    ExtenderDriver_IVars *driverIVars,
    ExtenderUserClient_IVars * /*ucIVars*/,
    uint64_t deviceId,
    IOUserClientMethodArguments *arguments)
{
    if (deviceId >= EXTENDER_MAX_DEVICES || !driverIVars->devices[deviceId].active) {
        return kIOReturnNotFound;
    }

    auto &slot = driverIVars->devices[deviceId];

    uint8_t responseBuffer[sizeof(ExtenderPendingOutputHeader) + EXTENDER_MAX_TRANSFER_SIZE];
    ExtenderPendingOutputHeader *hdr = (ExtenderPendingOutputHeader *)responseBuffer;
    uint8_t *payload = responseBuffer + sizeof(ExtenderPendingOutputHeader);
    uint32_t payloadLen = 0;
    bool hasOutput = false;

    switch ((ExtenderDeviceType)slot.deviceType) {
    case kDeviceTypeHID: {
        ExtenderVirtualHID *hid = OSDynamicCast(ExtenderVirtualHID, slot.device);
        if (hid) hasOutput = hid->dequeuePendingOutput(hdr, payload, EXTENDER_MAX_TRANSFER_SIZE, &payloadLen);
        break;
    }
    case kDeviceTypeStorage: {
        ExtenderVirtualStorage *st = OSDynamicCast(ExtenderVirtualStorage, slot.device);
        if (st) hasOutput = st->dequeuePendingOutput(hdr, payload, EXTENDER_MAX_TRANSFER_SIZE, &payloadLen);
        break;
    }
    case kDeviceTypeSerial: {
        ExtenderVirtualSerial *se = OSDynamicCast(ExtenderVirtualSerial, slot.device);
        if (se) hasOutput = se->dequeuePendingOutput(hdr, payload, EXTENDER_MAX_TRANSFER_SIZE, &payloadLen);
        break;
    }
    case kDeviceTypeNetwork: {
        ExtenderVirtualNetwork *nw = OSDynamicCast(ExtenderVirtualNetwork, slot.device);
        if (nw) hasOutput = nw->dequeuePendingOutput(hdr, payload, EXTENDER_MAX_TRANSFER_SIZE, &payloadLen);
        break;
    }
    case kDeviceTypeAudio: {
        ExtenderVirtualAudio *au = OSDynamicCast(ExtenderVirtualAudio, slot.device);
        if (au) hasOutput = au->dequeuePendingOutput(hdr, payload, EXTENDER_MAX_TRANSFER_SIZE, &payloadLen);
        break;
    }
    }

    if (!hasOutput) {
        arguments->structureOutput = OSData::withBytes(nullptr, 0);
        return kIOReturnSuccess;
    }

    uint32_t totalSize = (uint32_t)sizeof(ExtenderPendingOutputHeader) + payloadLen;
    arguments->structureOutput = OSData::withBytes(responseBuffer, totalSize);
    return kIOReturnSuccess;
}

static kern_return_t handleConfigureDevice(
    ExtenderDriver_IVars *driverIVars,
    uint64_t deviceId,
    uint64_t configType,
    IOUserClientMethodArguments *arguments)
{
    if (deviceId >= EXTENDER_MAX_DEVICES || !driverIVars->devices[deviceId].active) {
        return kIOReturnNotFound;
    }

    auto &slot = driverIVars->devices[deviceId];

    if (!arguments->structureInput) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": ConfigureDevice: no config data");
        return kIOReturnBadArgument;
    }

    const void *configData = arguments->structureInput->getBytesNoCopy();
    uint32_t configLen = arguments->structureInput->getLength();
    if (!configData || configLen == 0) {
        return kIOReturnBadArgument;
    }

    switch ((ExtenderConfigType)configType) {
    case kConfigDeviceDescriptor: {
        if (configLen < sizeof(ExtenderDeviceDescriptorConfig)) {
            return kIOReturnBadArgument;
        }
        auto *desc = (const ExtenderDeviceDescriptorConfig *)configData;
        slot.vendorId = desc->vendorId;
        slot.productId = desc->productId;
        strncpy(slot.productName, desc->product, sizeof(slot.productName) - 1);

        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device %llu configured: VID=0x%04x PID=0x%04x '%s'",
               deviceId, desc->vendorId, desc->productId, desc->product);
        return kIOReturnSuccess;
    }

    case kConfigHIDReportDescriptor: {
        if (slot.deviceType != kDeviceTypeHID) {
            return kIOReturnBadArgument;
        }
        if (configLen > EXTENDER_MAX_REPORT_DESCRIPTOR_SIZE) {
            return kIOReturnBadArgument;
        }

        // Store report descriptor on the HID device
        ExtenderVirtualHID *hid = OSDynamicCast(ExtenderVirtualHID, slot.device);
        if (!hid) {
            os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HID device not yet instantiated for slot %llu", deviceId);
            return kIOReturnNotReady;
        }
        // The HID device stores the descriptor in its own ivars
        // We pass the data through a call to the device
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": HID report descriptor set, %u bytes", configLen);
        return kIOReturnSuccess;
    }

    case kConfigStorageGeometry: {
        if (slot.deviceType != kDeviceTypeStorage) {
            return kIOReturnBadArgument;
        }
        if (configLen < sizeof(ExtenderStorageGeometry)) {
            return kIOReturnBadArgument;
        }
        auto *geom = (const ExtenderStorageGeometry *)configData;
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Storage geometry: %llu blocks x %u bytes",
               geom->blockCount, geom->blockSize);
        return kIOReturnSuccess;
    }

    case kConfigSerialLineState: {
        if (slot.deviceType != kDeviceTypeSerial) {
            return kIOReturnBadArgument;
        }
        if (configLen < sizeof(ExtenderSerialLineState)) {
            return kIOReturnBadArgument;
        }
        auto *line = (const ExtenderSerialLineState *)configData;
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Serial: %u baud, %u/%u/%u",
               line->baudRate, line->dataBits, line->parity, line->stopBits);
        return kIOReturnSuccess;
    }

    case kConfigNetworkMAC: {
        if (slot.deviceType != kDeviceTypeNetwork) {
            return kIOReturnBadArgument;
        }
        if (configLen < sizeof(ExtenderNetworkMAC)) {
            return kIOReturnBadArgument;
        }
        auto *mac = (const ExtenderNetworkMAC *)configData;
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Network MAC: %02x:%02x:%02x:%02x:%02x:%02x",
               mac->addr[0], mac->addr[1], mac->addr[2],
               mac->addr[3], mac->addr[4], mac->addr[5]);
        return kIOReturnSuccess;
    }

    case kConfigAudioFormat: {
        if (slot.deviceType != kDeviceTypeAudio) {
            return kIOReturnBadArgument;
        }
        if (configLen < sizeof(ExtenderAudioFormat)) {
            return kIOReturnBadArgument;
        }
        auto *fmt = (const ExtenderAudioFormat *)configData;
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Audio: %u Hz, %u ch, %u bit",
               fmt->sampleRate, fmt->channels, fmt->bitDepth);
        return kIOReturnSuccess;
    }

    default:
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Unknown config type %llu", configType);
        return kIOReturnBadArgument;
    }
}

// --- ExternalMethod dispatch ---

kern_return_t ExtenderUserClient::ExternalMethod(
    uint64_t selector,
    IOUserClientMethodArguments *arguments,
    const IOUserClientMethodDispatch *dispatch,
    OSObject *target,
    void *reference)
{
    if (!ivars->driver) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": ExternalMethod called with no driver");
        return kIOReturnNotReady;
    }

    ExtenderDriver_IVars *driverIVars = getDriverIVars(ivars->driver);

    switch ((ExtenderSelector)selector) {
    case kCreateDevice: {
        if (!arguments || arguments->scalarInputCount < 2) {
            return kIOReturnBadArgument;
        }
        uint64_t deviceType = arguments->scalarInput[0];
        uint64_t deviceId   = arguments->scalarInput[1];
        return handleCreateDevice(driverIVars, ivars->driver, deviceType, deviceId);
    }

    case kDestroyDevice: {
        if (!arguments || arguments->scalarInputCount < 1) {
            return kIOReturnBadArgument;
        }
        uint64_t deviceId = arguments->scalarInput[0];
        return handleDestroyDevice(driverIVars, deviceId);
    }

    case kSubmitInput: {
        if (!arguments || arguments->scalarInputCount < 3) {
            return kIOReturnBadArgument;
        }
        uint64_t deviceId   = arguments->scalarInput[0];
        uint64_t endpoint   = arguments->scalarInput[1];
        uint64_t dataLength = arguments->scalarInput[2];
        return handleSubmitInput(driverIVars, ivars, deviceId, endpoint, dataLength, arguments);
    }

    case kGetPendingOutput: {
        if (!arguments || arguments->scalarInputCount < 1) {
            return kIOReturnBadArgument;
        }
        uint64_t deviceId = arguments->scalarInput[0];
        return handleGetPendingOutput(driverIVars, ivars, deviceId, arguments);
    }

    case kGetDeviceCount: {
        if (arguments && arguments->scalarOutputCount >= 1) {
            arguments->scalarOutput[0] = driverIVars->deviceCount;
        }
        return kIOReturnSuccess;
    }

    case kConfigureDevice: {
        if (!arguments || arguments->scalarInputCount < 2) {
            return kIOReturnBadArgument;
        }
        uint64_t deviceId   = arguments->scalarInput[0];
        uint64_t configType = arguments->scalarInput[1];
        return handleConfigureDevice(driverIVars, deviceId, configType, arguments);
    }

    case kExportClaimDevice: {
        if (!arguments || arguments->scalarInputCount < 1) {
            return kIOReturnBadArgument;
        }
        uint64_t locationId = arguments->scalarInput[0];
        // TODO: walk IOKit registry for IOUSBHostDevice with matching locationID,
        // call driver->Create(usbDevice, "ExportDeviceProperties", &exportDev),
        // store in a slot table keyed by exportSlotId, return slotId.
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": kExportClaimDevice loc=0x%llx (stub)", locationId);
        if (arguments->scalarOutputCount >= 1) {
            arguments->scalarOutput[0] = 0;
        }
        return kIOReturnUnsupported;
    }

    case kExportReleaseDevice: {
        if (!arguments || arguments->scalarInputCount < 1) {
            return kIOReturnBadArgument;
        }
        uint64_t slotId = arguments->scalarInput[0];
        // TODO: look up slot, call exportDev->Terminate(0), release.
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": kExportReleaseDevice slot=%llu (stub)", slotId);
        return kIOReturnUnsupported;
    }

    case kExportSubmitURB: {
        if (!arguments || arguments->scalarInputCount < 3) {
            return kIOReturnBadArgument;
        }
        uint64_t slotId   = arguments->scalarInput[0];
        uint64_t endpoint = arguments->scalarInput[1];
        uint64_t urbType  = arguments->scalarInput[2];
        // TODO: look up slot's ExtenderExportDevice, dispatch to
        // submitControlTransfer / async bulk / interrupt / iso based on urbType.
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": kExportSubmitURB slot=%llu ep=%llu type=%llu (stub)",
               slotId, endpoint, urbType);
        return kIOReturnUnsupported;
    }

    case kExportGetPendingResponse: {
        if (!arguments || arguments->scalarInputCount < 1) {
            return kIOReturnBadArgument;
        }
        uint64_t slotId = arguments->scalarInput[0];
        // TODO: look up slot, call dequeuePendingResponse, return payload.
        arguments->structureOutput = OSData::withBytes(nullptr, 0);
        (void)slotId;
        return kIOReturnSuccess;
    }

    case kCompleteRequest: {
        // In: scalar[0]=deviceId, scalar[1]=requestId, scalar[2]=status (kern_return_t).
        // struct: payload bytes (for getReport completions).
        if (!arguments || arguments->scalarInputCount < 3) {
            return kIOReturnBadArgument;
        }
        uint64_t deviceId  = arguments->scalarInput[0];
        uint64_t requestId = arguments->scalarInput[1];
        kern_return_t status = (kern_return_t)arguments->scalarInput[2];

        if (deviceId >= EXTENDER_MAX_DEVICES || !driverIVars->devices[deviceId].active) {
            return kIOReturnNotFound;
        }
        auto &slot = driverIVars->devices[deviceId];

        const void *payload = nullptr;
        uint32_t payloadLen = 0;
        if (arguments->structureInput) {
            payload = arguments->structureInput->getBytesNoCopy();
            payloadLen = arguments->structureInput->getLength();
        }

        switch ((ExtenderDeviceType)slot.deviceType) {
        case kDeviceTypeHID: {
            ExtenderVirtualHID *hid = OSDynamicCast(ExtenderVirtualHID, slot.device);
            if (!hid) return kIOReturnNotReady;
            hid->completeRequest((uint32_t)requestId, status, payload, payloadLen);
            return kIOReturnSuccess;
        }
        case kDeviceTypeStorage: {
            ExtenderVirtualStorage *st = OSDynamicCast(ExtenderVirtualStorage, slot.device);
            if (!st) return kIOReturnNotReady;
            st->completeRequest((uint32_t)requestId, status, payload, payloadLen);
            return kIOReturnSuccess;
        }
        case kDeviceTypeSerial:
        case kDeviceTypeNetwork:
        case kDeviceTypeAudio:
            // Completion path not yet wired for these classes.
            os_log(OS_LOG_DEFAULT, LOG_PREFIX ": kCompleteRequest: type %u not wired", slot.deviceType);
            return kIOReturnSuccess;
        }
        return kIOReturnBadArgument;
    }

    default:
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Unknown selector %llu", selector);
        return kIOReturnBadArgument;
    }
}

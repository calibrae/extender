#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOMemoryDescriptor.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/IOMemoryMap.h>
#include <DriverKit/OSData.h>
#include <DriverKit/OSDictionary.h>
#include <DriverKit/OSString.h>
#include <DriverKit/OSNumber.h>
#include <HIDDriverKit/IOHIDDeviceKeys.h>

#include "ExtenderVirtualHID.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderHID"

namespace {

// Per-device ring buffer for host->device requests (getReport, setReport)
// the daemon will poll via kGetPendingOutput.
struct PendingOutputEntry {
    uint8_t  data[EXTENDER_MAX_TRANSFER_SIZE];
    uint32_t length;
    uint32_t endpoint;
    uint32_t requestType;  // 0 bulk-out, 1 interrupt-out, 2 control
    uint32_t requestId;    // Daemon echoes this in kCompleteRequest
    bool     valid;
};

// In-flight table: a getReport or setReport that we've returned-success on but
// is waiting for the daemon to deliver a response via kCompleteRequest. When
// the daemon calls back, we look up the entry by requestId and call CompleteReport
// on the OSAction.
struct InFlightEntry {
    uint32_t           requestId;
    OSAction          *action;            // retained while in flight
    IOMemoryDescriptor *report;            // retained while in flight; written into on completion (for getReport)
    bool               isGetReport;        // true = getReport (write to `report`), false = setReport (no write)
    bool               valid;
};

constexpr uint32_t kPendingOutputCapacity = EXTENDER_MAX_PENDING_OUTPUTS;
constexpr uint32_t kInFlightCapacity      = 16;

}  // namespace

struct ExtenderVirtualHID_IVars {
    // Report descriptor provided by the daemon
    uint8_t  reportDescriptor[EXTENDER_MAX_REPORT_DESCRIPTOR_SIZE];
    uint32_t reportDescriptorLength;

    // Device identity
    uint16_t vendorId;
    uint16_t productId;
    uint16_t versionNumber;
    char     productName[64];
    char     manufacturer[64];
    char     serialNumber[64];

    // State
    bool     started;
    uint32_t deviceId;

    // Pending host->device requests (drained by the daemon)
    PendingOutputEntry pending[kPendingOutputCapacity];
    uint32_t           pendingHead;
    uint32_t           pendingTail;
    uint32_t           pendingCount;

    // In-flight async getReport/setReport waiting for daemon completion
    InFlightEntry      inFlight[kInFlightCapacity];

    // Monotonic request-ID counter
    uint32_t           nextRequestId;
};

bool ExtenderVirtualHID::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualHID_IVars, 1);
    if (!ivars) return false;

    ivars->pendingHead = 0;
    ivars->pendingTail = 0;
    ivars->pendingCount = 0;
    ivars->nextRequestId = 1;
    for (uint32_t i = 0; i < kInFlightCapacity; i++) {
        ivars->inFlight[i].valid = false;
        ivars->inFlight[i].action = nullptr;
        ivars->inFlight[i].report = nullptr;
    }

    ivars->reportDescriptorLength = 0;
    ivars->vendorId = 0x1234;
    ivars->productId = 0x5678;
    ivars->versionNumber = 0x0100;
    strncpy(ivars->productName, "Extender Virtual HID", sizeof(ivars->productName) - 1);
    strncpy(ivars->manufacturer, "Extender USB/IP", sizeof(ivars->manufacturer) - 1);
    ivars->serialNumber[0] = '\0';
    ivars->started = false;
    ivars->deviceId = 0;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualHID, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: 0x%x", ret);
        return ret;
    }

    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device started (VID=0x%04x PID=0x%04x '%s')",
           ivars->vendorId, ivars->productId, ivars->productName);

    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualHID, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device stopped");
    ivars->started = false;

    // Release any still-in-flight actions/reports so we don't leak references.
    for (uint32_t i = 0; i < kInFlightCapacity; i++) {
        if (ivars->inFlight[i].valid) {
            if (ivars->inFlight[i].action) ivars->inFlight[i].action->release();
            if (ivars->inFlight[i].report) ivars->inFlight[i].report->release();
            ivars->inFlight[i].valid = false;
        }
    }

    IOSafeDeleteNULL(ivars, ExtenderVirtualHID_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}

OSDictionary * ExtenderVirtualHID::newDeviceDescription()
{
    OSDictionary *desc = OSDictionary::withCapacity(12);
    if (!desc) return nullptr;

    auto setString = [&](const char *key, const char *value) {
        OSString *s = OSString::withCString(value);
        if (s) { desc->setObject(key, s); s->release(); }
    };
    auto setNumber = [&](const char *key, uint32_t value) {
        OSNumber *n = OSNumber::withNumber(value, 32);
        if (n) { desc->setObject(key, n); n->release(); }
    };

    setString(kIOHIDTransportKey, "Virtual");
    setNumber(kIOHIDVendorIDKey, ivars->vendorId);
    setNumber(kIOHIDProductIDKey, ivars->productId);
    setNumber(kIOHIDVersionNumberKey, ivars->versionNumber);
    setString(kIOHIDProductKey, ivars->productName);
    setString(kIOHIDManufacturerKey, ivars->manufacturer);
    setNumber(kIOHIDLocationIDKey, ivars->deviceId);

    return desc;
}

OSData * ExtenderVirtualHID::newReportDescriptor()
{
    if (ivars->reportDescriptorLength == 0) {
        // Default: generic vendor-defined HID descriptor
        static const uint8_t defaultDesc[] = {
            0x06, 0x00, 0xFF,  // Usage Page (Vendor Defined)
            0x09, 0x01,        // Usage (Vendor Usage 1)
            0xA1, 0x01,        // Collection (Application)
            0x09, 0x01,        //   Usage (Vendor Usage 1)
            0x15, 0x00,        //   Logical Minimum (0)
            0x26, 0xFF, 0x00,  //   Logical Maximum (255)
            0x75, 0x08,        //   Report Size (8)
            0x95, 0x40,        //   Report Count (64)
            0x81, 0x02,        //   Input (Data, Variable, Absolute)
            0x09, 0x01,        //   Usage (Vendor Usage 1)
            0x91, 0x02,        //   Output (Data, Variable, Absolute)
            0xC0               // End Collection
        };
        return OSData::withBytes(defaultDesc, sizeof(defaultDesc));
    }
    return OSData::withBytes(ivars->reportDescriptor, ivars->reportDescriptorLength);
}

// Enqueue a host->device request for the daemon to forward over USB/IP.
// Returns the assigned requestId on success, or 0 if the queue is full.
static uint32_t enqueuePending(ExtenderVirtualHID_IVars *ivars,
                               uint32_t requestType,
                               const void *bytes,
                               uint32_t length)
{
    if (length > EXTENDER_MAX_TRANSFER_SIZE) return 0;
    if (ivars->pendingCount >= kPendingOutputCapacity) return 0;
    uint32_t rid = ivars->nextRequestId++;
    if (rid == 0) rid = ivars->nextRequestId++;  // never use 0 as a valid id
    PendingOutputEntry &entry = ivars->pending[ivars->pendingTail];
    entry.requestType = requestType;
    entry.endpoint = 0;
    entry.length = length;
    entry.requestId = rid;
    if (length > 0 && bytes) memcpy(entry.data, bytes, length);
    entry.valid = true;
    ivars->pendingTail = (ivars->pendingTail + 1) % kPendingOutputCapacity;
    ivars->pendingCount++;
    return rid;
}

// Find a free slot in the in-flight table and store (action, report). Returns
// the slot index, or -1 if the table is full.
static int trackInFlight(ExtenderVirtualHID_IVars *ivars,
                         uint32_t requestId,
                         OSAction *action,
                         IOMemoryDescriptor *report,
                         bool isGetReport)
{
    for (uint32_t i = 0; i < kInFlightCapacity; i++) {
        if (!ivars->inFlight[i].valid) {
            ivars->inFlight[i].requestId = requestId;
            ivars->inFlight[i].action = action;
            ivars->inFlight[i].report = report;
            ivars->inFlight[i].isGetReport = isGetReport;
            ivars->inFlight[i].valid = true;
            if (action) action->retain();
            if (report) report->retain();
            return (int)i;
        }
    }
    return -1;
}

kern_return_t ExtenderVirtualHID::getReport(
    IOMemoryDescriptor * report,
    IOHIDReportType reportType,
    IOOptionBits options,
    uint32_t completionTimeout,
    OSAction * action)
{
    // getReport asks the device for the current state of a feature/input report.
    // We forward the request to the remote USB device by queueing a control-OUT
    // entry the daemon will pick up via kGetPendingOutput. The response path
    // (handleReport from the daemon) will satisfy the host; for now we return
    // success synchronously without populating `report`.
    uint8_t requestHeader[2];
    requestHeader[0] = (uint8_t)reportType;
    requestHeader[1] = 0;
    uint32_t rid = enqueuePending(ivars, /*requestType=*/2, requestHeader, sizeof(requestHeader));
    if (rid == 0) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": getReport: pending queue full");
        return kIOReturnNoSpace;
    }
    if (trackInFlight(ivars, rid, action, report, /*isGetReport=*/true) < 0) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": getReport: in-flight table full");
        return kIOReturnNoResources;
    }
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": getReport type=%d rid=%u queued", (int)reportType, rid);
    return kIOReturnSuccess;
}

kern_return_t ExtenderVirtualHID::setReport(
    IOMemoryDescriptor * report,
    IOHIDReportType reportType,
    IOOptionBits options,
    uint32_t completionTimeout,
    OSAction * action)
{
    uint64_t length = 0;
    if (report) {
        report->GetLength(&length);
    }
    if (length > EXTENDER_MAX_TRANSFER_SIZE) {
        return kIOReturnNoSpace;
    }

    // Copy the host's payload out of the descriptor and queue it for the daemon.
    uint8_t tmp[EXTENDER_MAX_TRANSFER_SIZE];
    uint64_t copied = 0;
    if (length > 0 && report) {
        IOMemoryMap *map = nullptr;
        if (report->CreateMapping(0, 0, 0, 0, 0, &map) == kIOReturnSuccess && map) {
            uint64_t mapped = map->GetAddress();
            uint64_t mappedLen = map->GetLength();
            copied = (length < mappedLen) ? length : mappedLen;
            if (copied > 0 && mapped) {
                memcpy(tmp, (const void *)mapped, (size_t)copied);
            }
            map->release();
        }
    }

    uint32_t rid = enqueuePending(ivars, /*requestType=*/2, tmp, (uint32_t)copied);
    if (rid == 0) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": setReport: pending queue full");
        return kIOReturnNoSpace;
    }
    if (trackInFlight(ivars, rid, action, report, /*isGetReport=*/false) < 0) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": setReport: in-flight table full");
        return kIOReturnNoResources;
    }
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": setReport type=%d len=%llu rid=%u queued", (int)reportType, copied, rid);
    return kIOReturnSuccess;
}

void ExtenderVirtualHID::completeRequest(uint32_t requestId,
                                         kern_return_t status,
                                         const void *bytes,
                                         uint32_t length)
{
    if (!ivars) return;
    for (uint32_t i = 0; i < kInFlightCapacity; i++) {
        InFlightEntry &entry = ivars->inFlight[i];
        if (!entry.valid || entry.requestId != requestId) continue;

        // For getReport, copy bytes into the host's report descriptor.
        if (entry.isGetReport && entry.report && bytes && length > 0) {
            IOMemoryMap *map = nullptr;
            if (entry.report->CreateMapping(0, 0, 0, 0, 0, &map) == kIOReturnSuccess && map) {
                uint64_t mapped    = map->GetAddress();
                uint64_t mappedLen = map->GetLength();
                uint32_t writeLen  = length;
                if ((uint64_t)writeLen > mappedLen) writeLen = (uint32_t)mappedLen;
                if (mapped && writeLen > 0) memcpy((void *)mapped, bytes, writeLen);
                map->release();
            }
        }

        // Fire the completion. CompleteReport is required-pure on IOHIDDevice; the
        // generated subclass dispatches to the kext side which fires the action.
        if (entry.action) {
            CompleteReport(entry.action, status, length);
            entry.action->release();
            entry.action = nullptr;
        }
        if (entry.report) {
            entry.report->release();
            entry.report = nullptr;
        }
        entry.valid = false;
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": completed rid=%u status=0x%x len=%u", requestId, status, length);
        return;
    }
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": completeRequest: unknown rid=%u", requestId);
}

void ExtenderVirtualHID::submitInputReport(const void *data, uint32_t length)
{
    if (!ivars || !ivars->started || length == 0 || !data) {
        return;
    }
    if (length > EXTENDER_MAX_TRANSFER_SIZE) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInputReport: oversize %u", length);
        return;
    }

    IOBufferMemoryDescriptor *buf = nullptr;
    kern_return_t ret = IOBufferMemoryDescriptor::Create(kIOMemoryDirectionIn, length, 0, &buf);
    if (ret != kIOReturnSuccess || !buf) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInputReport: buf alloc failed 0x%x", ret);
        return;
    }

    IOAddressSegment range;
    if (buf->GetAddressRange(&range) == kIOReturnSuccess && range.address) {
        memcpy((void *)range.address, data, length);
        buf->SetLength(length);
        // Pass 0 for timestamp; the HID stack assigns one when not supplied.
        handleReport(0, buf, length, kIOHIDReportTypeInput, 0);
    } else {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": submitInputReport: GetAddressRange failed");
    }
    buf->release();
}

bool ExtenderVirtualHID::dequeuePendingOutput(void *hdrOut,
                                              void *bufOut,
                                              uint32_t maxLen,
                                              uint32_t *outLen)
{
    if (!ivars || !hdrOut || !outLen) return false;
    if (ivars->pendingCount == 0) { *outLen = 0; return false; }
    PendingOutputEntry &entry = ivars->pending[ivars->pendingHead];

    ExtenderPendingOutputHeader *hdr = (ExtenderPendingOutputHeader *)hdrOut;
    hdr->deviceId = ivars->deviceId;
    hdr->endpoint = entry.endpoint;
    hdr->requestType = entry.requestType;
    hdr->requestId = entry.requestId;
    hdr->reserved = 0;

    uint32_t copyLen = entry.length < maxLen ? entry.length : maxLen;
    hdr->dataLength = copyLen;
    if (copyLen > 0 && bufOut) memcpy(bufOut, entry.data, copyLen);
    *outLen = copyLen;

    entry.valid = false;
    ivars->pendingHead = (ivars->pendingHead + 1) % kPendingOutputCapacity;
    ivars->pendingCount--;
    return true;
}

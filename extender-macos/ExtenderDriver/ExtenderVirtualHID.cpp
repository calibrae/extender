#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOMemoryDescriptor.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/OSData.h>
#include <DriverKit/OSDictionary.h>
#include <DriverKit/OSString.h>
#include <DriverKit/OSNumber.h>
#include <HIDDriverKit/IOHIDDeviceKeys.h>

#include "ExtenderVirtualHID.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderHID"

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
};

bool ExtenderVirtualHID::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualHID_IVars, 1);
    if (!ivars) return false;

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

kern_return_t ExtenderVirtualHID::getReport(
    IOMemoryDescriptor * report,
    IOHIDReportType reportType,
    IOOptionBits options,
    uint32_t completionTimeout,
    OSAction * action)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": getReport type=%d", (int)reportType);
    return kIOReturnSuccess;
}

kern_return_t ExtenderVirtualHID::setReport(
    IOMemoryDescriptor * report,
    IOHIDReportType reportType,
    IOOptionBits options,
    uint32_t completionTimeout,
    OSAction * action)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": setReport type=%d", (int)reportType);
    return kIOReturnSuccess;
}

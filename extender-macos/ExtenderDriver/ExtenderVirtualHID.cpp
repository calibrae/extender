#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOMemoryDescriptor.h>
#include <DriverKit/IOBufferMemoryDescriptor.h>
#include <DriverKit/OSData.h>
#include <DriverKit/OSDictionary.h>
#include <DriverKit/OSString.h>
#include <DriverKit/OSNumber.h>

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

#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOUserClient.h>
#include <DriverKit/IOService.h>

#include "ExtenderDriver.h"
#include "ExtenderProtocol.h"
#include "ExtenderVirtualHID.h"
#include "ExtenderVirtualStorage.h"
#include "ExtenderVirtualNetwork.h"
#include "ExtenderVirtualSerial.h"
#include "ExtenderVirtualAudio.h"

#define LOG_PREFIX "Extender"

// Forward declarations for device base - we use void* and track type separately
struct ExtenderDeviceSlot {
    OSObject    *device;
    uint32_t     deviceType;
    bool         active;
    uint16_t     vendorId;
    uint16_t     productId;
    char         productName[64];
};

struct ExtenderDriver_IVars {
    ExtenderDeviceSlot devices[EXTENDER_MAX_DEVICES];
    uint32_t           deviceCount;
};

bool ExtenderDriver::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderDriver_IVars, 1);
    if (!ivars) return false;

    ivars->deviceCount = 0;
    for (uint32_t i = 0; i < EXTENDER_MAX_DEVICES; i++) {
        ivars->devices[i].device = nullptr;
        ivars->devices[i].active = false;
        ivars->devices[i].deviceType = 0;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderDriver, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: 0x%x", ret);
        return ret;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Driver started, ready for connections");
    RegisterService();

    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderDriver, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Driver stopping, cleaning up %u devices", ivars->deviceCount);

    // Release all active devices
    for (uint32_t i = 0; i < EXTENDER_MAX_DEVICES; i++) {
        if (ivars->devices[i].active && ivars->devices[i].device) {
            ivars->devices[i].device->release();
            ivars->devices[i].device = nullptr;
            ivars->devices[i].active = false;
        }
    }
    ivars->deviceCount = 0;

    IOSafeDeleteNULL(ivars, ExtenderDriver_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}

kern_return_t IMPL(ExtenderDriver, NewUserClient)
{
    IOService *client = nullptr;

    auto ret = Create(this, "UserClientProperties", &client);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Failed to create UserClient: 0x%x", ret);
        return ret;
    }

    *userClient = OSDynamicCast(IOUserClient, client);
    if (!*userClient) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": UserClient cast failed");
        client->release();
        return kIOReturnError;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": New UserClient connected");
    return kIOReturnSuccess;
}

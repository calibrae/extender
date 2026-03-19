#include <os/log.h>
#include <DriverKit/IOLib.h>
#include <DriverKit/IOUserClient.h>

#include "ExtenderVirtualStorageDriver.h"

#define LOG_PREFIX "ExtenderDriver"

struct ExtenderVirtualStorageDriver_IVars {
    uint64_t blockCount;
    uint32_t blockSize;
    char vendorID[9];
    char productID[17];
};

bool ExtenderVirtualStorageDriver::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualStorageDriver_IVars, 1);
    if (!ivars) return false;

    ivars->blockCount = 0;
    ivars->blockSize = 512;
    strncpy(ivars->vendorID, "Extender", 8);
    strncpy(ivars->productID, "Virtual Disk", 16);

    return true;
}

kern_return_t IMPL(ExtenderVirtualStorageDriver, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: %d", ret);
        return ret;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Driver started");
    RegisterService();

    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualStorageDriver, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Driver stopped");

    if (ivars) {
        IOSafeDeleteNULL(ivars, ExtenderVirtualStorageDriver_IVars, 1);
    }

    return Stop(provider, SUPERDISPATCH);
}

kern_return_t IMPL(ExtenderVirtualStorageDriver, NewUserClient)
{
    IOService *client = nullptr;

    auto ret = Create(this, "UserClientProperties", &client);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Failed to create UserClient: %d", ret);
        return ret;
    }

    *userClient = OSDynamicCast(IOUserClient, client);
    if (!*userClient) {
        client->release();
        return kIOReturnError;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": New UserClient connected");
    return kIOReturnSuccess;
}

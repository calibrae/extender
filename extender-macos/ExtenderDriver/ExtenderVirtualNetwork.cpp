#include <os/log.h>
#include <DriverKit/IOLib.h>

#include "ExtenderVirtualNetwork.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderNet"

#define EXTENDER_MTU 1500

struct ExtenderVirtualNetwork_IVars {
    uint8_t  macAddress[6];
    uint16_t vendorId;
    uint16_t productId;
    char     productName[64];
    bool     started;
    uint32_t deviceId;
    bool     promiscuous;
    uint32_t mtu;
};

bool ExtenderVirtualNetwork::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualNetwork_IVars, 1);
    if (!ivars) return false;

    // Default MAC: locally administered
    ivars->macAddress[0] = 0x02;
    ivars->macAddress[1] = 0xCA;
    ivars->macAddress[2] = 0x1B;
    ivars->macAddress[3] = 0x00;
    ivars->macAddress[4] = 0x00;
    ivars->macAddress[5] = 0x01;

    ivars->vendorId = 0;
    ivars->productId = 0;
    strncpy(ivars->productName, "Extender Virtual Ethernet", sizeof(ivars->productName) - 1);
    ivars->started = false;
    ivars->deviceId = 0;
    ivars->promiscuous = false;
    ivars->mtu = EXTENDER_MTU;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualNetwork, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: 0x%x", ret);
        return ret;
    }

    ivars->started = true;
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device started (MAC=%02x:%02x:%02x:%02x:%02x:%02x)",
           ivars->macAddress[0], ivars->macAddress[1], ivars->macAddress[2],
           ivars->macAddress[3], ivars->macAddress[4], ivars->macAddress[5]);

    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualNetwork, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device stopped");
    ivars->started = false;

    IOSafeDeleteNULL(ivars, ExtenderVirtualNetwork_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}

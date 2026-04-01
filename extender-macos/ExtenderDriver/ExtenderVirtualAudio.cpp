#include <os/log.h>
#include <DriverKit/IOLib.h>

#include "ExtenderVirtualAudio.h"
#include "ExtenderProtocol.h"

#define LOG_PREFIX "ExtenderAudio"

struct ExtenderVirtualAudio_IVars {
    uint32_t sampleRate;
    uint16_t channels;
    uint16_t bitDepth;
    bool     ioRunning;
    uint32_t deviceId;
};

bool ExtenderVirtualAudio::init()
{
    if (!super::init()) return false;

    ivars = IONewZero(ExtenderVirtualAudio_IVars, 1);
    if (!ivars) return false;

    ivars->sampleRate = 44100;
    ivars->channels = 2;
    ivars->bitDepth = 16;
    ivars->ioRunning = false;
    ivars->deviceId = 0;

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": init");
    return true;
}

kern_return_t IMPL(ExtenderVirtualAudio, Start)
{
    kern_return_t ret = Start(provider, SUPERDISPATCH);
    if (ret != kIOReturnSuccess) {
        os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Start failed: 0x%x", ret);
        return ret;
    }

    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device started (%u Hz, %u ch, %u bit)",
           ivars->sampleRate, ivars->channels, ivars->bitDepth);

    RegisterService();
    return kIOReturnSuccess;
}

kern_return_t IMPL(ExtenderVirtualAudio, Stop)
{
    os_log(OS_LOG_DEFAULT, LOG_PREFIX ": Device stopped");
    ivars->ioRunning = false;

    IOSafeDeleteNULL(ivars, ExtenderVirtualAudio_IVars, 1);

    return Stop(provider, SUPERDISPATCH);
}

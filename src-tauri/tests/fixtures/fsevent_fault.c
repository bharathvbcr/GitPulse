#include <CoreServices/CoreServices.h>
#include <unistd.h>
#include <dlfcn.h>
#include <stdlib.h>

#define INTERPOSE(replacement, original) \
    __attribute__((used)) static struct { const void *new_fn; const void *old_fn; } \
    interpose_##original __attribute__((section("__DATA,__interpose"))) = { \
        (const void *)&replacement, (const void *)&original }

// One fault per dylib avoids recursive forwarding through interposed symbols.
#if defined(GITPULSE_FAULT_START)
static Boolean probe_start(FSEventStreamRef stream) {
    (void)stream;
    return false;
}
INTERPOSE(probe_start, FSEventStreamStart);
#elif defined(GITPULSE_FAULT_CREATE)
static FSEventStreamRef probe_create(CFAllocatorRef allocator, FSEventStreamCallback callback,
    FSEventStreamContext *context, CFArrayRef paths, FSEventStreamEventId since,
    CFTimeInterval latency, FSEventStreamCreateFlags flags) {
    return NULL;
}
INTERPOSE(probe_create, FSEventStreamCreate);

#elif defined(GITPULSE_FAULT_PURGE)
static Boolean probe_purge(dev_t device, FSEventStreamEventId event) {
    (void)device;
    (void)event;
    sleep(10);
    return false;
}
INTERPOSE(probe_purge, FSEventsPurgeEventsForDeviceUpToEventId);

#elif defined(GITPULSE_FAULT_BUSY)
static Boolean probe_waiting(CFRunLoopRef runloop) {
    (void)runloop;
    return false;
}
INTERPOSE(probe_waiting, CFRunLoopIsWaiting);

#elif defined(GITPULSE_FAULT_SOURCE)
static CFRunLoopSourceRef probe_source(CFAllocatorRef allocator, CFIndex order,
    CFRunLoopSourceContext *context) {
    // Fail only the callback-only source used by notify for sticky shutdown.
    // Core Foundation and FSEvents also allocate sources while starting up.
    if (context && context->version == 0 && !context->info && !context->retain &&
        !context->release && !context->copyDescription && !context->equal &&
        !context->hash && !context->schedule && !context->cancel && context->perform) {
        static const char evidence[] = "GITPULSE_SHUTDOWN_SOURCE_FAULT\n";
        write(STDERR_FILENO, evidence, sizeof(evidence) - 1);
        return NULL;
    }
    typedef CFRunLoopSourceRef (*CreateSource)(CFAllocatorRef, CFIndex, CFRunLoopSourceContext *);
    CreateSource original = (CreateSource)dlsym(RTLD_NEXT, "CFRunLoopSourceCreate");
    if (!original || original == probe_source) abort();
    return original(allocator, order, context);
}
INTERPOSE(probe_source, CFRunLoopSourceCreate);

#elif defined(GITPULSE_FAULT_EARLY)
static void probe_run(void) {
    // Let Drop request shutdown before entering the native loop. Calling the
    // distinct RunInMode API avoids interposer forwarding recursion.
    usleep(100000);
    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 10, false);
}
INTERPOSE(probe_run, CFRunLoopRun);

#endif

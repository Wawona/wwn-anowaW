//
// AnowawMacBridge.m — ScreenCaptureKit capture + CGEvent injection shim.
//
// See AnowawMacBridge.h. Drives the Rust bridge core through the C ABI
// (anowaw.h). Compiled by dependencies/libs/anowaw/macos.nix and linked into
// the Wawona macOS app.
//
#import "AnowawMacBridge.h"
#import "anowaw.h"

#import <AppKit/AppKit.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreVideo/CoreVideo.h>
#import <ApplicationServices/ApplicationServices.h>

// evdev codes we care about (from <linux/input-event-codes.h>).
enum {
    EVDEV_BTN_LEFT = 0x110,
    EVDEV_BTN_RIGHT = 0x111,
    EVDEV_BTN_MIDDLE = 0x112,
};

#pragma mark - AnowawMacApp

@implementation AnowawMacApp
@end

#pragma mark - Per-app capture context

@interface AnowawMacWindow : NSObject <SCStreamDelegate, SCStreamOutput>
@property (nonatomic, assign) uint64_t handle;
@property (nonatomic, assign) pid_t pid;
@property (nonatomic, assign) CGWindowID windowId;
@property (nonatomic, strong, nullable) SCStream *stream;
@property (nonatomic, weak) AnowawMacBridge *owner;
/// Origin of the source window in global screen coords (for input mapping).
@property (nonatomic, assign) CGPoint windowOrigin;
@end

#pragma mark - Bridge

@interface AnowawMacBridge ()
@property (nonatomic, assign) AnowawBridge *core; // Rust bridge, owned by bridgeThread
@property (nonatomic, copy) NSString *socketName;
@property (nonatomic, strong) NSThread *bridgeThread;
@property (nonatomic, assign) BOOL running;
@property (nonatomic, strong) NSMutableDictionary<NSNumber *, AnowawMacWindow *> *windows;
@property (nonatomic, strong) dispatch_queue_t captureQueue;
@end

@implementation AnowawMacBridge

+ (NSArray<AnowawMacApp *> *)enumerateApps {
    NSMutableArray<AnowawMacApp *> *out = [NSMutableArray array];
    NSMutableSet<NSString *> *seen = [NSMutableSet set];

    // Running, regular-activation-policy apps first (these have windows).
    for (NSRunningApplication *ra in NSWorkspace.sharedWorkspace.runningApplications) {
        if (ra.activationPolicy != NSApplicationActivationPolicyRegular) continue;
        if (!ra.bundleIdentifier) continue;
        AnowawMacApp *app = [AnowawMacApp new];
        app.bundleId = ra.bundleIdentifier;
        app.localizedName = ra.localizedName ?: ra.bundleIdentifier;
        app.appURL = ra.bundleURL;
        app.pid = ra.processIdentifier;
        [out addObject:app];
        [seen addObject:ra.bundleIdentifier];
    }

    // Installed apps not currently running.
    NSArray<NSString *> *dirs = @[ @"/Applications", @"/System/Applications" ];
    for (NSString *dir in dirs) {
        NSArray<NSString *> *entries =
            [NSFileManager.defaultManager contentsOfDirectoryAtPath:dir error:nil];
        for (NSString *name in entries) {
            if (![name hasSuffix:@".app"]) continue;
            NSURL *url = [NSURL fileURLWithPath:[dir stringByAppendingPathComponent:name]];
            NSBundle *b = [NSBundle bundleWithURL:url];
            NSString *bid = b.bundleIdentifier;
            if (!bid || [seen containsObject:bid]) continue;
            AnowawMacApp *app = [AnowawMacApp new];
            app.bundleId = bid;
            app.localizedName = [name stringByDeletingPathExtension];
            app.appURL = url;
            app.pid = 0;
            [out addObject:app];
            [seen addObject:bid];
        }
    }
    return out;
}

+ (BOOL)hasCapturePermission {
    // CGPreflightScreenCaptureAccess is the lightweight probe for the Screen
    // Recording TCC grant that ScreenCaptureKit requires.
    return CGPreflightScreenCaptureAccess();
}

+ (BOOL)hasInputPermission {
    return AXIsProcessTrusted();
}

- (nullable instancetype)initWithSocketName:(NSString *)socketName {
    self = [super init];
    if (!self) return nil;
    _socketName = [socketName copy];
    _windows = [NSMutableDictionary dictionary];
    _captureQueue = dispatch_queue_create("com.aspauldingcode.Wawona.anowaw.capture",
                                          DISPATCH_QUEUE_SERIAL);

    if (anowaw_abi_version() != 1) {
        NSLog(@"anowaW: ABI version mismatch (got %u, expected 1)", anowaw_abi_version());
        return nil;
    }

    _running = YES;
    _bridgeThread = [[NSThread alloc] initWithTarget:self selector:@selector(bridgeThreadMain) object:nil];
    _bridgeThread.name = @"anowaw-bridge";
    [_bridgeThread start];
    return self;
}

// Owns the Rust bridge and pumps its event queue + input queue.
- (void)bridgeThreadMain {
    @autoreleasepool {
        self.core = anowaw_start(self.socketName.UTF8String);
        if (self.core == NULL) {
            NSLog(@"anowaW: failed to connect to nested Weston socket %@", self.socketName);
            self.running = NO;
            return;
        }
    }
    const size_t CAP = 256;
    AnowawInputEvent events[CAP];
    while (self.running) {
        @autoreleasepool {
            anowaw_dispatch(self.core);
            int n = anowaw_poll_input(self.core, events, CAP);
            for (int i = 0; i < n; i++) {
                [self injectEvent:&events[i]];
            }
            // Close-requested toplevels: tear down the corresponding capture.
            for (NSNumber *key in [self.windows.allKeys copy]) {
                uint64_t h = key.unsignedLongLongValue;
                if (anowaw_close_requested(self.core, h)) {
                    [self closeApp:h];
                }
            }
            // ~120 Hz pump; frames are pushed independently from the capture queue.
            usleep(8000);
        }
    }
    anowaw_stop(self.core);
    self.core = NULL;
}

#pragma mark - Bridging an app

- (void)bridgeAppWithBundleId:(NSString *)bundleId
                   completion:(void (^)(uint64_t, NSError *_Nullable))completion {
    // Ensure the app is running, then locate its main SCWindow.
    NSArray<NSRunningApplication *> *running =
        [NSRunningApplication runningApplicationsWithBundleIdentifier:bundleId];
    void (^afterLaunch)(NSRunningApplication *) = ^(NSRunningApplication *ra) {
        [self locateAndBridgeForApp:ra bundleId:bundleId completion:completion];
    };

    if (running.count > 0) {
        afterLaunch(running.firstObject);
        return;
    }
    NSURL *url = [NSWorkspace.sharedWorkspace URLForApplicationWithBundleIdentifier:bundleId];
    if (!url) {
        completion(0, [NSError errorWithDomain:@"anowaw" code:404 userInfo:@{
            NSLocalizedDescriptionKey: @"app not found"
        }]);
        return;
    }
    NSWorkspaceOpenConfiguration *cfg = [NSWorkspaceOpenConfiguration configuration];
    cfg.activates = NO;
    [NSWorkspace.sharedWorkspace openApplicationAtURL:url configuration:cfg
        completionHandler:^(NSRunningApplication *ra, NSError *err) {
            if (err || !ra) { completion(0, err); return; }
            // Give the app a beat to create its window.
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 0.6 * NSEC_PER_SEC),
                           dispatch_get_main_queue(), ^{ afterLaunch(ra); });
        }];
}

- (void)locateAndBridgeForApp:(NSRunningApplication *)ra
                     bundleId:(NSString *)bundleId
                   completion:(void (^)(uint64_t, NSError *_Nullable))completion {
    [SCShareableContent getShareableContentWithCompletionHandler:^(SCShareableContent *content, NSError *error) {
        if (error) { completion(0, error); return; }
        SCWindow *target = nil;
        for (SCWindow *w in content.windows) {
            if (w.owningApplication.processID == ra.processIdentifier &&
                w.isOnScreen && w.frame.size.width > 1 && w.frame.size.height > 1) {
                target = w;
                break;
            }
        }
        if (!target) {
            completion(0, [NSError errorWithDomain:@"anowaw" code:410 userInfo:@{
                NSLocalizedDescriptionKey: @"no capturable window for app"
            }]);
            return;
        }
        [self startCaptureForWindow:target
                           bundleId:bundleId
                                pid:ra.processIdentifier
                         completion:completion];
    }];
}

- (void)startCaptureForWindow:(SCWindow *)scWindow
                     bundleId:(NSString *)bundleId
                          pid:(pid_t)pid
                   completion:(void (^)(uint64_t, NSError *_Nullable))completion {
    uint32_t w = (uint32_t)scWindow.frame.size.width;
    uint32_t h = (uint32_t)scWindow.frame.size.height;

    // Create the Wayland toplevel on the bridge thread.
    __block uint64_t handle = 0;
    [self performOnBridgeThreadSync:^{
        handle = anowaw_bridge_app(self.core, bundleId.UTF8String,
                                   scWindow.title.UTF8String ?: bundleId.UTF8String, w, h);
    }];
    if (handle == 0) {
        completion(0, [NSError errorWithDomain:@"anowaw" code:500 userInfo:@{
            NSLocalizedDescriptionKey: @"anowaw_bridge_app failed"
        }]);
        return;
    }

    SCContentFilter *filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:scWindow];
    SCStreamConfiguration *cfg = [[SCStreamConfiguration alloc] init];
    cfg.width = w;
    cfg.height = h;
    cfg.pixelFormat = kCVPixelFormatType_32BGRA;
    cfg.showsCursor = NO;
    cfg.queueDepth = 3;

    AnowawMacWindow *ctx = [AnowawMacWindow new];
    ctx.handle = handle;
    ctx.pid = pid;
    ctx.windowId = (CGWindowID)scWindow.windowID;
    ctx.windowOrigin = scWindow.frame.origin;
    ctx.owner = self;

    SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:cfg delegate:ctx];
    NSError *addErr = nil;
    [stream addStreamOutput:ctx type:SCStreamOutputTypeScreen
        sampleHandlerQueue:self.captureQueue error:&addErr];
    if (addErr) { [self closeApp:handle]; completion(0, addErr); return; }

    ctx.stream = stream;
    @synchronized (self.windows) {
        self.windows[@(handle)] = ctx;
    }
    [stream startCaptureWithCompletionHandler:^(NSError *startErr) {
        if (startErr) { [self closeApp:handle]; completion(0, startErr); return; }
        completion(handle, nil);
    }];
}

#pragma mark - Frame push (called from capture queue via AnowawMacWindow)

- (void)pushFrameForHandle:(uint64_t)handle pixelBuffer:(CVPixelBufferRef)pb {
    if (self.core == NULL) return;
    CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
    void *base = CVPixelBufferGetBaseAddress(pb);
    size_t stride = CVPixelBufferGetBytesPerRow(pb);
    size_t width = CVPixelBufferGetWidth(pb);
    size_t height = CVPixelBufferGetHeight(pb);
    size_t len = stride * height;
    if (base) {
        anowaw_push_frame(self.core, handle, (const uint8_t *)base, len,
                          (uint32_t)width, (uint32_t)height, (uint32_t)stride,
                          ANOWAW_FORMAT_BGRA8888);
    }
    CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
}

#pragma mark - Input injection

- (void)injectEvent:(const AnowawInputEvent *)ev {
    AnowawMacWindow *ctx;
    @synchronized (self.windows) {
        ctx = self.windows[@(ev->handle)];
    }
    if (!ctx) return;

    switch ((AnowawInputKind)ev->kind) {
        case ANOWAW_INPUT_POINTER_MOTION: {
            CGPoint p = CGPointMake(ctx.windowOrigin.x + ev->x, ctx.windowOrigin.y + ev->y);
            CGEventRef e = CGEventCreateMouseEvent(NULL, kCGEventMouseMoved, p, kCGMouseButtonLeft);
            [self post:e toPid:ctx.pid];
            break;
        }
        case ANOWAW_INPUT_POINTER_BUTTON: {
            CGMouseButton btn = kCGMouseButtonLeft;
            CGEventType down = kCGEventLeftMouseDown, up = kCGEventLeftMouseUp;
            if (ev->code == EVDEV_BTN_RIGHT) { btn = kCGMouseButtonRight; down = kCGEventRightMouseDown; up = kCGEventRightMouseUp; }
            else if (ev->code == EVDEV_BTN_MIDDLE) { btn = kCGMouseButtonCenter; down = kCGEventOtherMouseDown; up = kCGEventOtherMouseUp; }
            CGPoint p = CGPointMake(ctx.windowOrigin.x + ev->x, ctx.windowOrigin.y + ev->y);
            CGEventRef e = CGEventCreateMouseEvent(NULL, ev->value ? down : up, p, btn);
            [self post:e toPid:ctx.pid];
            break;
        }
        case ANOWAW_INPUT_POINTER_AXIS: {
            int32_t dy = (int32_t)(-ev->y);
            int32_t dx = (int32_t)(-ev->x);
            CGEventRef e = CGEventCreateScrollWheelEvent(NULL, kCGScrollEventUnitPixel, 2, dy, dx);
            [self post:e toPid:ctx.pid];
            break;
        }
        case ANOWAW_INPUT_KEY: {
            CGKeyCode kc = [self macKeyCodeForEvdev:ev->code];
            if (kc == 0xFFFF) break;
            CGEventRef e = CGEventCreateKeyboardEvent(NULL, kc, ev->value != 0);
            [self post:e toPid:ctx.pid];
            break;
        }
        case ANOWAW_INPUT_POINTER_FOCUS:
            if (ev->value == 1) {
                NSRunningApplication *ra =
                    [NSRunningApplication runningApplicationWithProcessIdentifier:ctx.pid];
                [ra activateWithOptions:0];
            }
            break;
        default:
            break;
    }
}

- (void)post:(CGEventRef)event toPid:(pid_t)pid {
    if (!event) return;
    // Deliver directly to the source process so the event lands on the captured
    // window even though it is off-screen / not frontmost.
    CGEventPostToPid(pid, event);
    CFRelease(event);
}

// Minimal evdev KEY_* → macOS virtual keycode map. The full table lives in a
// generated header; this covers the common letters/navigation for v1.
- (CGKeyCode)macKeyCodeForEvdev:(uint32_t)code {
    switch (code) {
        case 1:  return 53;  // ESC
        case 28: return 36;  // ENTER
        case 14: return 51;  // BACKSPACE
        case 15: return 48;  // TAB
        case 57: return 49;  // SPACE
        case 30: return 0;   // A
        case 48: return 11;  // B
        case 46: return 8;   // C
        case 32: return 2;   // D
        case 18: return 14;  // E
        case 33: return 3;   // F
        case 105: return 123; // LEFT
        case 106: return 124; // RIGHT
        case 103: return 126; // UP
        case 108: return 125; // DOWN
        default: return 0xFFFF;
    }
}

#pragma mark - Lifecycle

- (void)closeApp:(uint64_t)handle {
    AnowawMacWindow *ctx;
    @synchronized (self.windows) {
        ctx = self.windows[@(handle)];
        [self.windows removeObjectForKey:@(handle)];
    }
    if (ctx.stream) {
        [ctx.stream stopCaptureWithCompletionHandler:^(NSError *e) { (void)e; }];
        ctx.stream = nil;
    }
    [self performOnBridgeThreadAsync:^{
        anowaw_close_app(self.core, handle);
    }];
}

- (void)stop {
    self.running = NO;
    for (NSNumber *key in [self.windows.allKeys copy]) {
        [self closeApp:key.unsignedLongLongValue];
    }
}

#pragma mark - Bridge-thread hops

- (void)performOnBridgeThreadSync:(void (^)(void))block {
    if (NSThread.currentThread == self.bridgeThread) { block(); return; }
    [self performSelector:@selector(runBlock:) onThread:self.bridgeThread
               withObject:[block copy] waitUntilDone:YES];
}
- (void)performOnBridgeThreadAsync:(void (^)(void))block {
    if (NSThread.currentThread == self.bridgeThread) { block(); return; }
    [self performSelector:@selector(runBlock:) onThread:self.bridgeThread
               withObject:[block copy] waitUntilDone:NO];
}
- (void)runBlock:(void (^)(void))block { if (block) block(); }

@end

#pragma mark - AnowawMacWindow (SCStreamOutput)

@implementation AnowawMacWindow

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
        ofType:(SCStreamOutputType)type {
    if (type != SCStreamOutputTypeScreen) return;
    if (!CMSampleBufferIsValid(sampleBuffer)) return;
    CVImageBufferRef pb = CMSampleBufferGetImageBuffer(sampleBuffer);
    if (!pb) return;
    [self.owner pushFrameForHandle:self.handle pixelBuffer:pb];
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    NSLog(@"anowaW: SCStream stopped for handle %llu: %@", self.handle, error);
    [self.owner closeApp:self.handle];
}

@end

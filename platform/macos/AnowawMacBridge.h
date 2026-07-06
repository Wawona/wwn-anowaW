//
// AnowawMacBridge.h — macOS capture/inject shim for the anowaW bridge.
//
// Pairs ScreenCaptureKit per-window capture with the Rust bridge core
// (include/anowaw.h): each bridged AppKit window becomes an xdg_toplevel in
// Wawona's nested-Weston desktop, and Wayland seat input is injected back via
// CGEvent + Accessibility.
//
// Requirements (Developer ID distribution, NOT App Store):
//   * "Screen Recording" TCC permission (ScreenCaptureKit)
//   * "Accessibility" TCC permission (CGEvent posting / AX activation)
//
// Threading: the class owns a dedicated bridge thread. All anowaw_* calls
// happen on that thread (the C ABI is thread-affine). Public methods are safe
// to call from any thread.
//
#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

/// One launchable/bridgeable application, as shown in the app picker.
@interface AnowawMacApp : NSObject
@property (nonatomic, copy) NSString *bundleId;
@property (nonatomic, copy) NSString *localizedName;
@property (nonatomic, copy, nullable) NSURL *appURL;
/// Nonzero when the app is already running.
@property (nonatomic, assign) pid_t pid;
@end

@interface AnowawMacBridge : NSObject

/// Enumerate bridgeable apps: running regular-activation-policy apps first,
/// then installed apps from /Applications (via NSWorkspace/Launch Services).
+ (NSArray<AnowawMacApp *> *)enumerateApps;

/// True when both Screen Recording and Accessibility permissions are granted.
+ (BOOL)hasCapturePermission;
+ (BOOL)hasInputPermission;

/// Connect the bridge to the nested Weston socket (e.g. @"wayland-1").
/// Returns nil if the Wayland connection fails.
- (nullable instancetype)initWithSocketName:(NSString *)socketName;

/// Launch (if needed) and bridge an app's main window into the nested desktop.
/// Completion delivers the nonzero anowaW app handle, or 0 on failure.
- (void)bridgeAppWithBundleId:(NSString *)bundleId
                   completion:(void (^)(uint64_t handle, NSError *_Nullable error))completion;

/// Stop bridging one app (tears down its SCStream + Wayland toplevel).
- (void)closeApp:(uint64_t)handle;

/// Disconnect everything and join the bridge thread.
- (void)stop;

@end

NS_ASSUME_NONNULL_END

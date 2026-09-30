// Private CoreGraphics API used to create virtual displays (same API used by Chromium's test
// utilities, DeskPad and BetterDisplay). Verified present on macOS 26.5 (M1).
// Not public: may change with OS updates.
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>

NS_ASSUME_NONNULL_BEGIN

@class CGVirtualDisplay;

@interface CGVirtualDisplayMode : NSObject
/// With hiDPI enabled, width/height are logical points and the backing store is 2x.
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)refreshRate;
@property (readonly, nonatomic) unsigned int width;
@property (readonly, nonatomic) unsigned int height;
@property (readonly, nonatomic) double refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property (nonatomic) unsigned int hiDPI;
@property (retain, nonatomic) NSArray<CGVirtualDisplayMode *> *modes;
@end

@interface CGVirtualDisplayDescriptor : NSObject
@property (retain, nonatomic, nullable) dispatch_queue_t queue;
@property (retain, nonatomic) NSString *name;
@property (nonatomic) unsigned int maxPixelsWide;
@property (nonatomic) unsigned int maxPixelsHigh;
@property (nonatomic) CGSize sizeInMillimeters;
@property (nonatomic) unsigned int serialNum;
@property (nonatomic) unsigned int productID;
@property (nonatomic) unsigned int vendorID;
@property (nonatomic) CGPoint whitePoint;
@property (nonatomic) CGPoint redPrimary;
@property (nonatomic) CGPoint greenPrimary;
@property (nonatomic) CGPoint bluePrimary;
@property (copy, nonatomic, nullable) void (^terminationHandler)(id _Nullable, CGVirtualDisplay * _Nullable);
@end

@interface CGVirtualDisplay : NSObject
- (nullable instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@property (readonly, nonatomic) unsigned int displayID;
@end

NS_ASSUME_NONNULL_END

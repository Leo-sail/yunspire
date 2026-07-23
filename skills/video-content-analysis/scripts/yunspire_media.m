#import <AVFoundation/AVFoundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>

static void emit(NSDictionary *result) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:result options:0 error:nil];
    if (data) {
        [[NSFileHandle fileHandleWithStandardOutput] writeData:data];
    }
}

static BOOL savePNG(CGImageRef image, NSURL *url) {
    CGImageDestinationRef destination = CGImageDestinationCreateWithURL(
        (__bridge CFURLRef)url,
        CFSTR("public.png"),
        1,
        NULL
    );
    if (!destination) return NO;
    CGImageDestinationAddImage(destination, image, NULL);
    BOOL saved = CGImageDestinationFinalize(destination);
    CFRelease(destination);
    return saved;
}

static double frameDifference(
    CGImageRef image,
    unsigned char current[256],
    const unsigned char previous[256],
    BOOL hasPrevious,
    double *meanLuminance,
    double *luminanceDeviation
) {
    memset(current, 0, 256);
    CGColorSpaceRef colorSpace = CGColorSpaceCreateDeviceGray();
    CGContextRef context = CGBitmapContextCreate(current, 16, 16, 8, 16, colorSpace, (CGBitmapInfo)kCGImageAlphaNone);
    CGColorSpaceRelease(colorSpace);
    if (!context) return 255.0;
    CGContextSetInterpolationQuality(context, kCGInterpolationLow);
    CGContextDrawImage(context, CGRectMake(0, 0, 16, 16), image);
    CGContextRelease(context);
    double luminanceTotal = 0.0;
    for (NSInteger index = 0; index < 256; index += 1) {
        luminanceTotal += current[index];
    }
    double mean = luminanceTotal / 256.0;
    double variance = 0.0;
    for (NSInteger index = 0; index < 256; index += 1) {
        double delta = current[index] - mean;
        variance += delta * delta;
    }
    if (meanLuminance) *meanLuminance = mean;
    if (luminanceDeviation) *luminanceDeviation = sqrt(variance / 256.0);
    if (!hasPrevious) return 255.0;
    double total = 0.0;
    for (NSInteger index = 0; index < 256; index += 1) {
        total += abs((int)current[index] - (int)previous[index]);
    }
    return total / 256.0;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 3) {
            emit(@{@"duration_seconds": @0, @"audio_path": @"", @"frames": @[], @"warnings": @[], @"errors": @[@"media_arguments_missing"]});
            return 0;
        }
        NSString *mediaPath = [NSString stringWithUTF8String:argv[1]];
        NSString *outputPath = [NSString stringWithUTF8String:argv[2]];
        NSURL *mediaURL = [NSURL fileURLWithPath:mediaPath];
        NSURL *outputURL = [NSURL fileURLWithPath:outputPath isDirectory:YES];
        [[NSFileManager defaultManager] createDirectoryAtURL:outputURL withIntermediateDirectories:YES attributes:nil error:nil];

        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:mediaURL options:nil];
        Float64 seconds = CMTimeGetSeconds(asset.duration);
        if (!isfinite(seconds) || seconds < 0) seconds = 0;
        NSMutableArray *warnings = [NSMutableArray array];
        if (seconds <= 0) [warnings addObject:@"无法读取媒体时长"];

        NSString *audioPath = @"";
        NSURL *audioURL = [outputURL URLByAppendingPathComponent:@"speech-audio.m4a"];
        [[NSFileManager defaultManager] removeItemAtURL:audioURL error:nil];
        AVAssetExportSession *exporter = [[AVAssetExportSession alloc] initWithAsset:asset presetName:AVAssetExportPresetAppleM4A];
        if (exporter) {
            exporter.outputURL = audioURL;
            exporter.outputFileType = AVFileTypeAppleM4A;
            dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
            [exporter exportAsynchronouslyWithCompletionHandler:^{ dispatch_semaphore_signal(semaphore); }];
            long status = dispatch_semaphore_wait(semaphore, dispatch_time(DISPATCH_TIME_NOW, 300LL * NSEC_PER_SEC));
            if (status == 0 && exporter.status == AVAssetExportSessionStatusCompleted) {
                audioPath = audioURL.path;
            }
        }
        if (audioPath.length == 0) [warnings addObject:@"媒体没有可导出的音轨"];

        AVAssetImageGenerator *generator = [[AVAssetImageGenerator alloc] initWithAsset:asset];
        generator.appliesPreferredTrackTransform = YES;
        generator.maximumSize = CGSizeMake(1280, 1280);
        generator.requestedTimeToleranceBefore = CMTimeMakeWithSeconds(1, 600);
        generator.requestedTimeToleranceAfter = CMTimeMakeWithSeconds(1, 600);
        NSMutableArray *frames = [NSMutableArray array];
        NSMutableArray *frameTimestamps = [NSMutableArray array];
        NSMutableArray *frameDifferenceScores = [NSMutableArray array];
        if (seconds > 0) {
            // Candidate density is time-based, so valuable scenes can grow with video length
            // without imposing a frame-count ceiling.
            const Float64 sampleIntervalSeconds = 2.0;
            NSInteger sampleCount = MAX(1, (NSInteger)ceil(seconds / sampleIntervalSeconds));
            unsigned char previousFingerprint[256] = {0};
            BOOL hasPreviousFingerprint = NO;
            for (NSInteger index = 0; index < sampleCount; index += 1) {
                Float64 position = seconds * ((double)index + 0.5) / (double)sampleCount;
                dispatch_semaphore_t frameReady = dispatch_semaphore_create(0);
                __block CGImageRef image = NULL;
                [generator generateCGImageAsynchronouslyForTime:CMTimeMakeWithSeconds(position, 600) completionHandler:^(CGImageRef generated, CMTime actualTime, NSError *error) {
                    if (generated) image = CGImageRetain(generated);
                    dispatch_semaphore_signal(frameReady);
                }];
                dispatch_semaphore_wait(frameReady, dispatch_time(DISPATCH_TIME_NOW, 60LL * NSEC_PER_SEC));
                if (!image) continue;
                unsigned char fingerprint[256] = {0};
                double meanLuminance = 0.0;
                double luminanceDeviation = 0.0;
                double difference = frameDifference(
                    image,
                    fingerprint,
                    previousFingerprint,
                    hasPreviousFingerprint,
                    &meanLuminance,
                    &luminanceDeviation
                );
                BOOL containsVisibleInformation = meanLuminance >= 4.0
                    && meanLuminance <= 251.0
                    && luminanceDeviation >= 4.0;
                BOOL shouldKeep = containsVisibleInformation
                    && (!hasPreviousFingerprint || difference >= 9.0);
                if (!shouldKeep) {
                    CGImageRelease(image);
                    continue;
                }
                memcpy(previousFingerprint, fingerprint, 256);
                hasPreviousFingerprint = YES;
                NSString *name = [NSString stringWithFormat:@"frame-%06ld.png", (long)frames.count + 1];
                NSURL *frameURL = [outputURL URLByAppendingPathComponent:name];
                if (savePNG(image, frameURL)) {
                    [frames addObject:frameURL.path];
                    [frameTimestamps addObject:@((NSInteger)llround(position * 1000.0))];
                    [frameDifferenceScores addObject:@(difference)];
                }
                CGImageRelease(image);
            }
        }
        if (frames.count == 0) [warnings addObject:@"媒体没有可提取的视频画面"];
        emit(@{
            @"duration_seconds": @(seconds),
            @"audio_path": audioPath,
            @"frames": frames,
            @"frame_timestamps_ms": frameTimestamps,
            @"frame_difference_scores": frameDifferenceScores,
            @"frame_candidate_count": @(seconds > 0 ? MAX(1, (NSInteger)ceil(seconds / 2.0)) : 0),
            @"frame_selection_method": @"yunspire-scene-change-v2",
            @"warnings": warnings,
            @"errors": @[],
        });
    }
    return 0;
}

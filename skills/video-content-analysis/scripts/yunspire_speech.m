#import <Foundation/Foundation.h>
#import <Speech/Speech.h>

static NSString *resultPath = nil;

static void emit(NSDictionary *result) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:result options:0 error:nil];
    if (!data) return;
    if (resultPath.length > 0) [data writeToFile:resultPath options:NSDataWritingAtomic error:nil];
    [[NSFileHandle fileHandleWithStandardOutput] writeData:data];
}

static NSDictionary *emptyResult(NSString *locale, NSArray *warnings, NSArray *errors) {
    return @{
        @"transcript": @"",
        @"locale": locale ?: @"zh-CN",
        @"on_device": @NO,
        @"segments": @[],
        @"warnings": warnings ?: @[],
        @"errors": errors ?: @[],
    };
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 2) {
            emit(emptyResult(@"zh-CN", @[], @[@"audio_path_missing"]));
            return 0;
        }
        NSString *audioPath = [NSString stringWithUTF8String:argv[1]];
        NSString *localeIdentifier = argc >= 3 ? [NSString stringWithUTF8String:argv[2]] : @"zh-CN";
        if (argc >= 4) resultPath = [NSString stringWithUTF8String:argv[3]];
        SFSpeechRecognizer *recognizer = [[SFSpeechRecognizer alloc] initWithLocale:[NSLocale localeWithLocaleIdentifier:localeIdentifier]];
        if (!recognizer || !recognizer.available) {
            emit(emptyResult(localeIdentifier, @[@"macOS 本机语音识别当前不可用"], @[@"speech_recognizer_unavailable"]));
            return 0;
        }
        if (@available(macOS 10.15, *)) {
            if (!recognizer.supportsOnDeviceRecognition) {
                emit(emptyResult(localeIdentifier, @[@"当前语言或系统不支持本机语音识别，云枢不会把音频发送到云端"], @[@"on_device_speech_unavailable"]));
                return 0;
            }
        }

        __block SFSpeechRecognizerAuthorizationStatus authorizationStatus = [SFSpeechRecognizer authorizationStatus];
        if (authorizationStatus == SFSpeechRecognizerAuthorizationStatusNotDetermined) {
            [SFSpeechRecognizer requestAuthorization:^(SFSpeechRecognizerAuthorizationStatus status) {
                authorizationStatus = status;
            }];
            NSDate *authorizationDeadline = [NSDate dateWithTimeIntervalSinceNow:20.0];
            while (authorizationStatus == SFSpeechRecognizerAuthorizationStatusNotDetermined
                   && authorizationDeadline.timeIntervalSinceNow > 0) {
                [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                         beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.1]];
            }
            if (authorizationStatus == SFSpeechRecognizerAuthorizationStatusNotDetermined) {
                emit(emptyResult(localeIdentifier, @[@"macOS 语音识别授权请求超时，请重新执行本次音视频任务"], @[@"speech_permission_timeout"]));
                return 0;
            }
        }
        if (authorizationStatus != SFSpeechRecognizerAuthorizationStatusAuthorized) {
            emit(emptyResult(localeIdentifier, @[@"请在 macOS 隐私与安全性中允许 Yunspire 使用语音识别"], @[@"speech_permission_required"]));
            return 0;
        }

        NSURL *audioURL = [NSURL fileURLWithPath:audioPath];
        SFSpeechURLRecognitionRequest *request = [[SFSpeechURLRecognitionRequest alloc] initWithURL:audioURL];
        request.shouldReportPartialResults = NO;
        request.addsPunctuation = YES;
        if (@available(macOS 10.15, *)) request.requiresOnDeviceRecognition = YES;

        __block BOOL finished = NO;
        __block NSDictionary *finalResult = emptyResult(localeIdentifier, @[], @[@"speech_recognition_failed"]);
        SFSpeechRecognitionTask *task = [recognizer recognitionTaskWithRequest:request resultHandler:^(SFSpeechRecognitionResult *result, NSError *error) {
            @synchronized (recognizer) {
                if (finished) return;
                if (result && result.final) {
                    NSMutableArray *segments = [NSMutableArray array];
                    for (SFTranscriptionSegment *segment in result.bestTranscription.segments) {
                        [segments addObject:@{
                            @"start_ms": @((NSInteger)llround(segment.timestamp * 1000.0)),
                            @"end_ms": @((NSInteger)llround((segment.timestamp + segment.duration) * 1000.0)),
                            @"text": segment.substring ?: @"",
                            @"confidence": @(segment.confidence),
                        }];
                    }
                    finalResult = @{
                        @"transcript": result.bestTranscription.formattedString ?: @"",
                        @"locale": localeIdentifier,
                        @"on_device": @YES,
                        @"segments": segments,
                        @"warnings": @[],
                        @"errors": @[],
                    };
                    finished = YES;
                } else if (error) {
                    NSString *message = error.localizedDescription ?: @"语音识别失败";
                    if ([message containsString:@"Siri and Dictation are disabled"]) {
                        message = @"Siri 与听写已关闭，请在 macOS 系统设置中启用听写后重试";
                    }
                    finalResult = emptyResult(localeIdentifier, @[message], @[@"speech_recognition_failed"]);
                    finished = YES;
                }
            }
        }];
        NSDate *recognitionDeadline = [NSDate dateWithTimeIntervalSinceNow:600.0];
        while (!finished && recognitionDeadline.timeIntervalSinceNow > 0) {
            [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                     beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.1]];
        }
        if (!finished) {
            [task cancel];
            finalResult = emptyResult(localeIdentifier, @[@"语音识别超过 10 分钟"], @[@"speech_recognition_timeout"]);
        }
        emit(finalResult);
    }
    return 0;
}

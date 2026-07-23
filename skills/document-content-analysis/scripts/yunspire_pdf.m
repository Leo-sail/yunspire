#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#import <PDFKit/PDFKit.h>

static void emit(NSDictionary *result) {
    NSData *data = [NSJSONSerialization dataWithJSONObject:result options:0 error:nil];
    if (data) [[NSFileHandle fileHandleWithStandardOutput] writeData:data];
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 2) {
            emit(@{@"text": @"", @"attachments": @[], @"warnings": @[], @"errors": @[@"pdf_path_missing"]});
            return 0;
        }
        NSString *path = [NSString stringWithUTF8String:argv[1]];
        PDFDocument *document = [[PDFDocument alloc] initWithURL:[NSURL fileURLWithPath:path]];
        if (!document) {
            emit(@{@"text": @"", @"attachments": @[], @"warnings": @[@"PDFKit 无法打开文件"], @"errors": @[@"pdf_open_failed"]});
            return 0;
        }
        NSMutableArray *pages = [NSMutableArray array];
        NSMutableArray *attachments = [NSMutableArray array];
        for (NSInteger index = 0; index < document.pageCount; index += 1) {
            PDFPage *page = [document pageAtIndex:index];
            NSString *text = [page.string stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceAndNewlineCharacterSet]];
            if (text.length > 0) {
                [pages addObject:text];
                continue;
            }
            NSRect bounds = [page boundsForBox:kPDFDisplayBoxMediaBox];
            NSInteger width = MIN(1600, MAX(800, (NSInteger)llround(bounds.size.width * 2.0)));
            NSInteger height = MIN(2200, MAX(1000, (NSInteger)llround(bounds.size.height * 2.0)));
            NSImage *thumbnail = [page thumbnailOfSize:NSMakeSize(width, height) forBox:kPDFDisplayBoxMediaBox];
            NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc] initWithData:thumbnail.TIFFRepresentation];
            NSData *png = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
            if (!png || png.length > 8 * 1024 * 1024) continue;
            [attachments addObject:@{
                @"name": [NSString stringWithFormat:@"pdf-page-%03ld.png", (long)index + 1],
                @"mime_type": @"image/png",
                @"size": @(png.length),
                @"data_base64": [png base64EncodedStringWithOptions:0],
            }];
        }
        NSArray *warnings = pages.count == 0 ? @[@"PDF 没有可提取文本，已渲染页面图片供本地视觉分析"] : @[];
        NSArray *errors = pages.count == 0 && attachments.count == 0 ? @[@"pdf_content_unavailable"] : @[];
        emit(@{
            @"text": [pages componentsJoinedByString:@"\n\n"],
            @"attachments": attachments,
            @"warnings": warnings,
            @"errors": errors,
        });
    }
    return 0;
}

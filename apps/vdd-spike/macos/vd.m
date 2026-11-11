// Spike do monitor virtual (docs/monitor-virtual.md, marco 0), macOS.
//
// Cria um monitor virtual com CGVirtualDisplay (API não documentada do
// CoreGraphics), confere se o ScreenCaptureKit o enxerga, salva uma captura
// dele em vd-captura.png e o mantém ligado por alguns segundos (para conectar
// com o Hext e ver). Depois o remove e confere que sumiu.
//
//   clang -fobjc-arc -framework Foundation -framework CoreGraphics \
//     -framework ScreenCaptureKit -framework ImageIO \
//     -framework UniformTypeIdentifiers vd.m -o vd
//   ./vd [segundos]          (padrão: 60)
//
// O Terminal precisa da permissão de Gravação de Tela.

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>

// As classes não têm cabeçalho público: só as declarações usadas aqui. Elas
// são obtidas com NSClassFromString, então o programa não falha ao abrir se
// um macOS futuro as remover.
@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsWide;
@property(nonatomic) unsigned int maxPixelsHigh;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) unsigned int productID;
@property(nonatomic) unsigned int vendorID;
@property(nonatomic) unsigned int serialNum;
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width
                       height:(unsigned int)height
                  refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray *modes;
@property(nonatomic) unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
@property(readonly, nonatomic) CGDirectDisplayID displayID;
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@end

static NSArray<SCDisplay *> *shareable(void) {
  __block NSArray<SCDisplay *> *result = nil;
  dispatch_semaphore_t done = dispatch_semaphore_create(0);
  [SCShareableContent getShareableContentWithCompletionHandler:^(
                          SCShareableContent *content, NSError *error) {
    if (error) {
      printf("SCShareableContent: erro %s\n", error.localizedDescription.UTF8String);
    }
    result = content.displays;
    dispatch_semaphore_signal(done);
  }];
  dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
  return result ?: @[];
}

static void list(NSArray<SCDisplay *> *displays) {
  printf("monitores no ScreenCaptureKit: %lu\n", (unsigned long)displays.count);
  for (SCDisplay *d in displays) {
    printf("  id %u: %ldx%ld pontos\n", d.displayID, (long)d.width, (long)d.height);
  }
}

static SCDisplay *find(NSArray<SCDisplay *> *displays, CGDirectDisplayID id) {
  for (SCDisplay *d in displays) {
    if (d.displayID == id) return d;
  }
  return nil;
}

static void save_capture(SCDisplay *display) {
  if (@available(macOS 14.0, *)) {
    SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display
                                                      excludingWindows:@[]];
    SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
    config.width = display.width;
    config.height = display.height;
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    [SCScreenshotManager
        captureImageWithFilter:filter
                 configuration:config
             completionHandler:^(CGImageRef image, NSError *error) {
               if (error || !image) {
                 printf("captura: erro %s\n", error.localizedDescription.UTF8String);
               } else {
                 NSURL *url = [NSURL fileURLWithPath:@"vd-captura.png"];
                 CGImageDestinationRef dest = CGImageDestinationCreateWithURL(
                     (__bridge CFURLRef)url, (__bridge CFStringRef)UTTypePNG.identifier, 1,
                     NULL);
                 CGImageDestinationAddImage(dest, image, NULL);
                 CGImageDestinationFinalize(dest);
                 CFRelease(dest);
                 printf("OK: captura %zux%zu salva em vd-captura.png\n", CGImageGetWidth(image),
                        CGImageGetHeight(image));
               }
               dispatch_semaphore_signal(done);
             }];
    dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
  } else {
    printf("captura: precisa do macOS 14 (o monitor aparecer já é o resultado principal)\n");
  }
}

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    int seconds = argc > 1 ? atoi(argv[1]) : 60;
    printf("macOS %s\n", NSProcessInfo.processInfo.operatingSystemVersionString.UTF8String);
    if (!CGPreflightScreenCaptureAccess()) {
      printf("Sem permissão de Gravação de Tela para o Terminal: Ajustes do Sistema > "
             "Privacidade e Segurança > Gravação de Tela.\n");
      CGRequestScreenCaptureAccess();
      return 1;
    }
    printf("antes:\n");
    list(shareable());

    Class descriptorClass = NSClassFromString(@"CGVirtualDisplayDescriptor");
    Class displayClass = NSClassFromString(@"CGVirtualDisplay");
    Class settingsClass = NSClassFromString(@"CGVirtualDisplaySettings");
    Class modeClass = NSClassFromString(@"CGVirtualDisplayMode");
    if (!descriptorClass || !displayClass || !settingsClass || !modeClass) {
      printf("FALHOU: este macOS não tem a API CGVirtualDisplay\n");
      return 1;
    }

    CGVirtualDisplayDescriptor *descriptor = [[descriptorClass alloc] init];
    descriptor.queue = dispatch_queue_create("hext.vd", DISPATCH_QUEUE_SERIAL);
    descriptor.name = @"Hext Remote (virtual)";
    descriptor.maxPixelsWide = 1920;
    descriptor.maxPixelsHigh = 1080;
    // 24": o tamanho define a densidade que o macOS assume.
    descriptor.sizeInMillimeters = CGSizeMake(531, 299);
    descriptor.vendorID = 0x4858;  // "HX"
    descriptor.productID = 0x0001;
    descriptor.serialNum = 0x0001;

    NSDate *start = [NSDate date];
    CGVirtualDisplay *display = [[displayClass alloc] initWithDescriptor:descriptor];
    if (!display || display.displayID == 0) {
      printf("FALHOU: o macOS não criou o monitor virtual\n");
      return 1;
    }
    CGVirtualDisplaySettings *settings = [[settingsClass alloc] init];
    settings.hiDPI = 0;
    settings.modes = @[
      [[modeClass alloc] initWithWidth:1920 height:1080 refreshRate:60],
      [[modeClass alloc] initWithWidth:1280 height:720 refreshRate:60],
    ];
    if (![display applySettings:settings]) {
      printf("FALHOU: applySettings recusado\n");
      return 1;
    }
    CGDirectDisplayID id = display.displayID;
    printf("monitor virtual criado: id %u\n", id);

    SCDisplay *found = nil;
    while (!found && -[start timeIntervalSinceNow] < 15) {
      found = find(shareable(), id);
      if (!found) [NSThread sleepForTimeInterval:0.2];
    }
    if (!found) {
      printf("FALHOU: o ScreenCaptureKit não listou o monitor em 15 s\n");
      list(shareable());
      return 1;
    }
    printf("apareceu no ScreenCaptureKit em %.1f s\n", -[start timeIntervalSinceNow]);
    list(shareable());
    save_capture(found);

    printf("mantendo o monitor ligado por %d s: conecte com o Hext agora\n", seconds);
    [NSThread sleepForTimeInterval:seconds];

    display = nil;  // liberado: o monitor some
    [NSThread sleepForTimeInterval:2];
    NSArray<SCDisplay *> *after = shareable();
    printf("depois de remover: %s\n", find(after, id) ? "AINDA LISTADO" : "removido");
    list(after);
  }
  return 0;
}

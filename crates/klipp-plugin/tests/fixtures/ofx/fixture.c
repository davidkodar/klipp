/* Deterministic, CPU-only native OFX fixtures. No Klipp headers or implementation
 * are linked here. These are the OFX v1 C ABI slots used by the fixture. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stddef.h>

typedef void *Handle;
typedef int Status;
typedef struct { double x1, y1, x2, y2; } Rect;
typedef struct {
    Status (*set_pointer)(Handle, const char *, int, void *);
    Status (*set_string)(Handle, const char *, int, const char *);
    Status (*set_double)(Handle, const char *, int, double);
    Status (*set_int)(Handle, const char *, int, int);
    void (*set_pointer_n)(void), (*set_string_n)(void), (*set_double_n)(void), (*set_int_n)(void);
    Status (*get_pointer)(Handle, const char *, int, void **);
    Status (*get_string)(Handle, const char *, int, char **);
    Status (*get_double)(Handle, const char *, int, double *);
    Status (*get_int)(Handle, const char *, int, int *);
    void (*get_pointer_n)(void), (*get_string_n)(void), (*get_double_n)(void), (*get_int_n)(void);
    void (*reset)(void), (*dimension)(void);
} Properties;
typedef struct {
    Status (*get_properties)(Handle, Handle *);
    Status (*get_parameters)(Handle, Handle *);
    Status (*define_clip)(Handle, const char *, Handle *);
    Status (*get_clip)(Handle, const char *, Handle *, Handle *);
    Status (*clip_properties)(Handle, Handle *);
    Status (*get_image)(Handle, double, const Rect *, Handle *);
    Status (*release_image)(Handle);
    Status (*region)(Handle, double, Rect *);
    int (*abort)(Handle);
    Status (*allocate)(Handle, size_t, Handle *);
    Status (*free_memory)(Handle);
    Status (*lock)(Handle, void **);
    Status (*unlock)(Handle);
} Images;
typedef struct { Handle host; const void *(*fetch)(Handle, const char *, int); } Host;
typedef struct {
    const char *api; int api_version; const char *id;
    unsigned major, minor;
    void (*set_host)(Host *);
    Status (*entry)(const char *, const void *, Handle, Handle);
} Plugin;
static Host *host;
static const char *names[] = { "identity", "channels", "alpha", "time" };
static void set_host(Host *value) { host = value; }
#define TRY(expr) do { if ((expr) != 0) return 1; } while (0)

static Status action(int kind, const char *name, const void *effect, Handle in, Handle out) {
    (void)out;
    if (!host) return 1;
    const Properties *p = host->fetch(host->host, "OfxPropertySuite", 1);
    const Images *im = host->fetch(host->host, "OfxImageEffectSuite", 1);
    if (!p || !im) return 1;
    if (!strcmp(name, "OfxActionLoad") || !strcmp(name, "OfxActionUnload") ||
        !strcmp(name, "OfxActionCreateInstance") || !strcmp(name, "OfxActionDestroyInstance")) return 0;
    if (!strcmp(name, "OfxActionDescribe")) {
        Handle properties = NULL;
        TRY(im->get_properties((Handle)effect, &properties));
        TRY(p->set_string(properties, "OfxImageEffectPropSupportedContexts", 0, "OfxImageEffectContextFilter"));
        TRY(p->set_string(properties, "OfxImageEffectPropSupportedPixelDepths", 0, "OfxBitDepthByte"));
        TRY(p->set_string(properties, "OfxPropLabel", 0, names[kind]));
        return 0;
    }
    if (!strcmp(name, "OfxImageEffectActionDescribeInContext")) {
        Handle clip = NULL;
        TRY(im->define_clip((Handle)effect, "Source", &clip));
        TRY(p->set_string(clip, "OfxImageEffectPropSupportedComponents", 0, "OfxImageComponentRGBA"));
        TRY(im->define_clip((Handle)effect, "Output", &clip));
        TRY(p->set_string(clip, "OfxImageEffectPropSupportedComponents", 0, "OfxImageComponentRGBA"));
        TRY(p->set_string(clip, "OfxImageEffectPropPreMultiplication", 0, "OfxImageUnPreMultiplied"));
        return 0;
    }
    if (strcmp(name, "OfxImageEffectActionRender")) return 14; /* ReplyDefault */
    const char *mode = getenv("KLIPP_FIXTURE_MODE");
    if (!mode) mode = "normal";
    if (!strcmp(mode, "hang")) { for (;;) {} }
    if (!strcmp(mode, "render-error")) return 1;
    double time = 0;
    TRY(p->get_double(in, "OfxPropTime", 0, &time));
    Handle source = NULL, output = NULL, src = NULL, dst = NULL;
    TRY(im->get_clip((Handle)effect, "Source", &source, NULL));
    TRY(im->get_clip((Handle)effect, "Output", &output, NULL));
    TRY(im->get_image(source, time, NULL, &src));
    if (im->get_image(output, time, NULL, &dst) != 0) { im->release_image(src); return 1; }
    void *src_data = NULL, *dst_data = NULL;
    int sr = 0, dr = 0, bounds[4] = {0}, dst_bounds[4] = {0}, window[4] = {0};
    char *src_depth = NULL, *dst_depth = NULL;
    Status status = 1;
    if (p->get_pointer(src, "OfxImagePropData", 0, &src_data) ||
        p->get_pointer(dst, "OfxImagePropData", 0, &dst_data) ||
        p->get_int(src, "OfxImagePropRowBytes", 0, &sr) ||
        p->get_int(dst, "OfxImagePropRowBytes", 0, &dr) ||
        p->get_string(src, "OfxImageEffectPropPixelDepth", 0, &src_depth) ||
        p->get_string(dst, "OfxImageEffectPropPixelDepth", 0, &dst_depth)) goto cleanup;
    if (!src_depth || !dst_depth || strcmp(src_depth, "OfxBitDepthByte") || strcmp(dst_depth, "OfxBitDepthByte")) goto cleanup;
    for (int i = 0; i < 4; ++i) {
        if (p->get_int(src, "OfxImagePropBounds", i, &bounds[i]) ||
            p->get_int(dst, "OfxImagePropBounds", i, &dst_bounds[i]) ||
            p->get_int(in, "OfxImageEffectPropRenderWindow", i, &window[i])) goto cleanup;
    }
    if (!src_data || !dst_data || window[0] < bounds[0] || window[1] < bounds[1] ||
        window[2] > bounds[2] || window[3] > bounds[3] ||
        window[0] < dst_bounds[0] || window[1] < dst_bounds[1] ||
        window[2] > dst_bounds[2] || window[3] > dst_bounds[3] ||
        sr < (bounds[2]-bounds[0])*4 || dr < (dst_bounds[2]-dst_bounds[0])*4) goto cleanup;
    for (int y = window[1]; y < window[3]; ++y) {
        for (int x = window[0]; x < window[2]; ++x) {
            const uint8_t *s = (uint8_t *)src_data + (y-bounds[1])*sr + (x-bounds[0])*4;
            uint8_t *d = (uint8_t *)dst_data + (y-dst_bounds[1])*dr + (x-dst_bounds[0])*4;
            memcpy(d, s, 4);
            if (strcmp(mode, "passthrough")) {
                if (kind == 1) { d[0] = s[2]; d[1] = 255-s[0]; d[2] = s[1]; }
                if (kind == 2) d[3] = s[3]/2;
                if (kind == 3) d[0] = (uint8_t)((s[0] + (int)(time*16)) & 255);
                if (!strcmp(mode, "wrong-output")) d[0] ^= 1;
            }
        }
    }
    fprintf(stderr, "KLIPP_FIXTURE_RENDER %s %.17g\n", names[kind], time);
    status = 0;
cleanup:
    im->release_image(src);
    im->release_image(dst);
    return status;
}
#define ENTRY(n) static Status entry##n(const char *a, const void *h, Handle i, Handle o) { return action(n,a,h,i,o); }
ENTRY(0) ENTRY(1) ENTRY(2) ENTRY(3)
#define PLUGIN(n, label) {"OfxImageEffectPluginAPI", 1, "org.klipp.fixture." label, 1, 0, set_host, entry##n}
static Plugin plugins[] = {PLUGIN(0,"identity"),PLUGIN(1,"channels"),PLUGIN(2,"alpha"),PLUGIN(3,"time")};
int OfxGetNumberOfPlugins(void) { return 4; }
Plugin *OfxGetPlugin(int index) { return index >= 0 && index < 4 ? &plugins[index] : NULL; }

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
/* The first six slots of OfxParameterSuiteV1; the getters are variadic. */
typedef struct {
    Status (*define)(Handle, const char *, const char *, Handle *);
    Status (*get_handle)(Handle, const char *, Handle *, Handle *);
    Status (*set_props)(Handle, Handle *);
    Status (*props)(Handle, Handle *);
    Status (*get_value)(Handle, ...);
    Status (*get_value_at_time)(Handle, double, ...);
} Params;
typedef struct { Handle host; const void *(*fetch)(Handle, const char *, int); } Host;
typedef struct {
    const char *api; int api_version; const char *id;
    unsigned major, minor;
    void (*set_host)(Host *);
    Status (*entry)(const char *, const void *, Handle, Handle);
} Plugin;
static Host *host;
static const char *names[] = { "identity", "channels", "alpha", "time", "temporal", "count" };
/* The temporal fixture reads the frames either side, in OFX time units
 * (frames): red from the one before, green its own, blue the one after;
 * a frame the host doesn't give is 0. `mode` 5 asks 40 either side, 6 a
 * million. */
#define TEMPORAL 4
/* The count fixture keeps state: red is how many frames its instance has
 * rendered (1 on its first), so a fresh instance, or one shared, shows. */
#define COUNT 5
/* A build with -DKLIPP_FIXTURE_VERSION=2 declares version 2 and one
 * parameter more (`gain`): the same plug-ins under another schema. */
#ifndef KLIPP_FIXTURE_VERSION
#define KLIPP_FIXTURE_VERSION 1
#endif
static void set_host(Host *value) { host = value; }
/* Each instance made and destroyed, one line to the file KLIPP_FIXTURE_TRACE_FILE
 * names, when it names one: a test counts them to see a slot's drop destroy
 * its instance. */
static void trace(const char *what, int kind) {
    const char *path = getenv("KLIPP_FIXTURE_TRACE_FILE");
    FILE *f = path ? fopen(path, "a") : NULL;
    if (f) { fprintf(f, "%s %s\n", what, names[kind]); fclose(f); }
}
#define TRY(expr) do { if ((expr) != 0) return 1; } while (0)

static Status action(int kind, const char *name, const void *effect, Handle in, Handle out) {
    (void)out;
    if (!host) return 1;
    const Properties *p = host->fetch(host->host, "OfxPropertySuite", 1);
    const Images *im = host->fetch(host->host, "OfxImageEffectSuite", 1);
    if (!p || !im) return 1;
    const Params *pa = host->fetch(host->host, "OfxParameterSuite", 1);
    if (!pa) return 1;
    if (!strcmp(name, "OfxActionCreateInstance")) {
        trace("create", kind);
        if (kind == COUNT) {
            Handle props = NULL;
            int *count = calloc(1, sizeof *count);
            if (!count) return 1;
            TRY(im->get_properties((Handle)effect, &props));
            TRY(p->set_pointer(props, "OfxPropInstanceData", 0, count));
        }
        return 0;
    }
    if (!strcmp(name, "OfxActionDestroyInstance")) {
        trace("destroy", kind);
        if (kind == COUNT) {
            Handle props = NULL;
            void *count = NULL;
            if (!im->get_properties((Handle)effect, &props) && !p->get_pointer(props, "OfxPropInstanceData", 0, &count)) free(count);
        }
        return 0;
    }
    if (!strcmp(name, "OfxActionLoad") || !strcmp(name, "OfxActionUnload")) return 0;
    if (!strcmp(name, "OfxActionDescribe")) {
        Handle properties = NULL;
        TRY(im->get_properties((Handle)effect, &properties));
        TRY(p->set_string(properties, "OfxImageEffectPropSupportedContexts", 0, "OfxImageEffectContextFilter"));
        TRY(p->set_string(properties, "OfxImageEffectPropSupportedPixelDepths", 0, "OfxBitDepthByte"));
        if (kind == TEMPORAL) TRY(p->set_int(properties, "OfxImageEffectPropTemporalClipAccess", 0, 1));
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
        /* `mode`: a case's failure mode, per instance, so cases running
         * side by side in one process never share it (0 = the runner's
         * KLIPP_FIXTURE_MODE, else normal). */
        Handle params = NULL, props = NULL;
        TRY(im->get_parameters((Handle)effect, &params));
        TRY(pa->define(params, "OfxParamTypeInteger", "mode", &props));
        TRY(p->set_int(props, "OfxParamPropDefault", 0, 0));
#if KLIPP_FIXTURE_VERSION >= 2
        TRY(pa->define(params, "OfxParamTypeDouble", "gain", &props));
#endif
        return 0;
    }
    if (kind == TEMPORAL && !strcmp(name, "OfxImageEffectActionGetFramesNeeded")) {
        double t = 0;
        Handle params = NULL, param = NULL;
        int chosen = 0;
        TRY(p->get_double(in, "OfxPropTime", 0, &t));
        if (!im->get_parameters((Handle)effect, &params) && !pa->get_handle(params, "mode", &param, NULL)) pa->get_value(param, &chosen);
        double reach = chosen == 5 ? 40.0 : chosen == 6 ? 1.0e6 : 1.0;
        TRY(p->set_double(out, "OfxImageClipPropFrameRange_Source", 0, t - reach));
        TRY(p->set_double(out, "OfxImageClipPropFrameRange_Source", 1, t + reach));
        fprintf(stderr, "KLIPP_FIXTURE_FRAMES_NEEDED %.17g %.17g\n", t - reach, t + reach);
        return 0;
    }
    if (strcmp(name, "OfxImageEffectActionRender")) return 14; /* ReplyDefault */
    static const char *modes[] = { NULL, "passthrough", "render-error", "hang", "wrong-output" };
    const char *mode = NULL;
    Handle params = NULL, param = NULL;
    int chosen = 0;
    if (!im->get_parameters((Handle)effect, &params) && !pa->get_handle(params, "mode", &param, NULL) &&
        !pa->get_value(param, &chosen) && chosen > 0 && chosen < 5) mode = modes[chosen];
    if (!mode) mode = getenv("KLIPP_FIXTURE_MODE");
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
    /* The temporal fixture's frames either side, when the host gives them
     * (the same bounds and row bytes as the source's frame). */
    Handle prev = NULL, next = NULL;
    uint8_t *prev_data = NULL, *next_data = NULL;
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
    if (kind == TEMPORAL) {
        double reach = chosen == 5 ? 40.0 : 1.0;
        void *data = NULL;
        if (!im->get_image(source, time - reach, NULL, &prev) && prev && !p->get_pointer(prev, "OfxImagePropData", 0, &data)) prev_data = data;
        data = NULL;
        if (!im->get_image(source, time + reach, NULL, &next) && next && !p->get_pointer(next, "OfxImagePropData", 0, &data)) next_data = data;
        fprintf(stderr, "KLIPP_FIXTURE_GOT %.17g %s %s\n", time, prev_data ? "before" : "-", next_data ? "after" : "-");
    }
    int rendered = 0;
    if (kind == COUNT) {
        Handle props = NULL;
        void *data = NULL;
        if (im->get_properties((Handle)effect, &props) || p->get_pointer(props, "OfxPropInstanceData", 0, &data) || !data) goto cleanup;
        rendered = ++*(int *)data;
    }
    for (int y = window[1]; y < window[3]; ++y) {
        for (int x = window[0]; x < window[2]; ++x) {
            const uint8_t *s = (uint8_t *)src_data + (y-bounds[1])*sr + (x-bounds[0])*4;
            uint8_t *d = (uint8_t *)dst_data + (y-dst_bounds[1])*dr + (x-dst_bounds[0])*4;
            memcpy(d, s, 4);
            if (strcmp(mode, "passthrough")) {
                if (kind == 1) { d[0] = s[2]; d[1] = 255-s[0]; d[2] = s[1]; }
                if (kind == 2) d[3] = s[3]/2;
                if (kind == 3) d[0] = (uint8_t)((s[0] + (int)(time*16)) & 255);
                if (kind == COUNT) d[0] = (uint8_t)(rendered > 255 ? 255 : rendered);
                if (kind == TEMPORAL) {
                    size_t at = (size_t)(y-bounds[1])*sr + (size_t)(x-bounds[0])*4;
                    d[0] = prev_data ? prev_data[at] : 0;
                    d[2] = next_data ? next_data[at + 2] : 0;
                }
                if (!strcmp(mode, "wrong-output")) d[0] ^= 1;
            }
        }
    }
    fprintf(stderr, "KLIPP_FIXTURE_RENDER %s %.17g\n", names[kind], time);
    status = 0;
cleanup:
    if (prev) im->release_image(prev);
    if (next) im->release_image(next);
    im->release_image(src);
    im->release_image(dst);
    return status;
}
#define ENTRY(n) static Status entry##n(const char *a, const void *h, Handle i, Handle o) { return action(n,a,h,i,o); }
ENTRY(0) ENTRY(1) ENTRY(2) ENTRY(3) ENTRY(4) ENTRY(5)
#define PLUGIN(n, label) {"OfxImageEffectPluginAPI", 1, "org.klipp.fixture." label, KLIPP_FIXTURE_VERSION, 0, set_host, entry##n}
static Plugin plugins[] = {PLUGIN(0,"identity"),PLUGIN(1,"channels"),PLUGIN(2,"alpha"),PLUGIN(3,"time"),PLUGIN(4,"temporal"),PLUGIN(5,"count")};
int OfxGetNumberOfPlugins(void) { return 6; }
Plugin *OfxGetPlugin(int index) { return index >= 0 && index < 6 ? &plugins[index] : NULL; }

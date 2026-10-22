/* Deterministic frei0r 1.x filters for scripts/plugin-tests.py --format frei0r.
 *
 * One source, one plug-in per build: compile with -DKLIPP_KIND=<n> (frei0r
 * has one plug-in per shared object). Self-contained: the ABI below is
 * declared here, not taken from an installed frei0r.h, so the fixture never
 * depends on what the machine has installed. No oracle lives here; the
 * runner computes every expected picture on its own.
 *
 * Fixture version 1. Change KLIPP_FIXTURE_VERSION with any behaviour change.
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define KLIPP_FIXTURE_VERSION 1

#ifndef KLIPP_KIND
#error "build with -DKLIPP_KIND=<n>"
#endif

/* The frei0r 1.2 ABI, as the host binds it. */
typedef struct { const char *name, *author; int plugin_type, color_model, frei0r_version, major_version, minor_version, num_params; const char *explanation; } f0r_plugin_info_t;
typedef struct { const char *name; int type; const char *explanation; } f0r_param_info_t;
typedef struct { float r, g, b; } f0r_param_color_t;
typedef struct { double x, y; } f0r_param_position_t;
enum { BGRA8888 = 0, RGBA8888 = 1, PACKED32 = 2 };
enum { P_BOOL = 0, P_DOUBLE = 1, P_COLOR = 2, P_POSITION = 3, P_STRING = 4 };

/* Kinds. Each byte layout is the plug-in's own: a BGRA plug-in reads B first. */
#define COLOUR 0     /* RGBA: (B + 16t, 255 - R, G, A) */
#define ALPHA 1      /* RGBA: A / 2 */
#define PARAMS 2     /* RGBA: one parameter of each type, applied in order */
#define HISTORY 3    /* RGBA: outputs the previous frame's input (the first frame's own) */
#define BGRA 4       /* BGRA: inverts its byte 0 (blue) */
#define PACKED 5     /* PACKED32: inverts the low byte of each pixel word */
#define NEIGHBOUR 6  /* RGBA: 3x3 box, edges clamped to the instance */
#define REFUSES 7    /* never makes an instance: a render that fails */

#if KLIPP_KIND == COLOUR
#define NAME "klipp-fixture-colour"
#elif KLIPP_KIND == ALPHA
#define NAME "klipp-fixture-alpha"
#elif KLIPP_KIND == PARAMS
#define NAME "klipp-fixture-params"
#elif KLIPP_KIND == HISTORY
#define NAME "klipp-fixture-history"
#elif KLIPP_KIND == BGRA
#define NAME "klipp-fixture-bgra"
#elif KLIPP_KIND == PACKED
#define NAME "klipp-fixture-packed32"
#elif KLIPP_KIND == NEIGHBOUR
#define NAME "klipp-fixture-neighbour"
#elif KLIPP_KIND == REFUSES
#define NAME "klipp-fixture-refuses"
#else
#error "unknown KLIPP_KIND"
#endif

typedef struct {
    unsigned width, height;
    double invert, gain;
    f0r_param_color_t tint;
    f0r_param_position_t cut;
    char order[8];
    uint32_t *previous;
} instance_t;

int f0r_init(void) { return 1; }
void f0r_deinit(void) {}

void f0r_get_plugin_info(f0r_plugin_info_t *info) {
    info->name = NAME;
    info->author = "Klipp";
    info->plugin_type = 0; /* filter */
    info->color_model = KLIPP_KIND == BGRA ? BGRA8888 : KLIPP_KIND == PACKED ? PACKED32 : RGBA8888;
    info->frei0r_version = 1;
    info->major_version = KLIPP_FIXTURE_VERSION;
    info->minor_version = 0;
    info->num_params = KLIPP_KIND == PARAMS ? 5 : 0;
    info->explanation = "Klipp test fixture";
}

void f0r_get_param_info(f0r_param_info_t *info, int index) {
    static const f0r_param_info_t params[5] = {
        {"invert", P_BOOL, "invert red"},
        {"gain", P_DOUBLE, "green times 2 x gain"},
        {"tint", P_COLOR, "multiply each channel"},
        {"cut", P_POSITION, "clear alpha right of and below"},
        {"order", P_STRING, "rgb, or bgr to swap red and blue"},
    };
    if (index >= 0 && index < 5) *info = params[index];
}

void *f0r_construct(unsigned width, unsigned height) {
    if (KLIPP_KIND == REFUSES) return NULL;
    instance_t *inst = calloc(1, sizeof *inst);
    if (!inst) return NULL;
    inst->width = width;
    inst->height = height;
    inst->gain = 0.5;
    inst->tint = (f0r_param_color_t){1.0f, 1.0f, 1.0f};
    inst->cut = (f0r_param_position_t){1.0, 1.0};
    strcpy(inst->order, "rgb");
    return inst;
}

void f0r_destruct(void *instance) {
    instance_t *inst = instance;
    free(inst->previous);
    free(inst);
}

void f0r_set_param_value(void *instance, void *param, int index) {
    instance_t *inst = instance;
    switch (index) {
    case 0: inst->invert = *(double *)param; break;
    case 1: inst->gain = *(double *)param; break;
    case 2: inst->tint = *(f0r_param_color_t *)param; break;
    case 3: inst->cut = *(f0r_param_position_t *)param; break;
    case 4: snprintf(inst->order, sizeof inst->order, "%s", *(const char **)param); break;
    }
}

void f0r_get_param_value(void *instance, void *param, int index) {
    instance_t *inst = instance;
    switch (index) {
    case 0: *(double *)param = inst->invert; break;
    case 1: *(double *)param = inst->gain; break;
    case 2: *(f0r_param_color_t *)param = inst->tint; break;
    case 3: *(f0r_param_position_t *)param = inst->cut; break;
    case 4: *(const char **)param = inst->order; break;
    }
}

#if KLIPP_KIND == PARAMS
static uint8_t clamp8(double v) { return v <= 0 ? 0 : v >= 255 ? 255 : (uint8_t)(v + 0.5); }
#endif

static void render(instance_t *inst, double time, const uint8_t *src, uint8_t *dst) {
    unsigned w = inst->width, h = inst->height;
    for (unsigned y = 0; y < h; ++y) {
        for (unsigned x = 0; x < w; ++x) {
            const uint8_t *s = src + ((size_t)y * w + x) * 4;
            uint8_t *d = dst + ((size_t)y * w + x) * 4;
            memcpy(d, s, 4);
#if KLIPP_KIND == COLOUR
            d[0] = (uint8_t)((s[2] + (int)(time * 16)) & 255);
            d[1] = 255 - s[0];
            d[2] = s[1];
#elif KLIPP_KIND == ALPHA
            d[3] = s[3] / 2;
#elif KLIPP_KIND == PARAMS
            if (inst->invert >= 0.5) d[0] = 255 - d[0];
            d[1] = clamp8(d[1] * inst->gain * 2.0);
            d[0] = clamp8(d[0] * (double)inst->tint.r);
            d[1] = clamp8(d[1] * (double)inst->tint.g);
            d[2] = clamp8(d[2] * (double)inst->tint.b);
            if (x >= inst->cut.x * w && y >= inst->cut.y * h) d[3] = 0;
            if (!strcmp(inst->order, "bgr")) { uint8_t r = d[0]; d[0] = d[2]; d[2] = r; }
#elif KLIPP_KIND == BGRA || KLIPP_KIND == PACKED
            d[0] = 255 - s[0];
#elif KLIPP_KIND == NEIGHBOUR
            for (int c = 0; c < 4; ++c) {
                unsigned sum = 0;
                for (int dy = -1; dy <= 1; ++dy) {
                    for (int dx = -1; dx <= 1; ++dx) {
                        long sx = (long)x + dx, sy = (long)y + dy;
                        sx = sx < 0 ? 0 : sx >= (long)w ? (long)w - 1 : sx;
                        sy = sy < 0 ? 0 : sy >= (long)h ? (long)h - 1 : sy;
                        sum += src[((size_t)sy * w + (size_t)sx) * 4 + c];
                    }
                }
                d[c] = (uint8_t)((sum + 4) / 9);
            }
#endif
        }
    }
#if KLIPP_KIND == HISTORY
    size_t bytes = (size_t)w * h * 4;
    if (inst->previous) memcpy(dst, inst->previous, bytes);
    else if (!(inst->previous = malloc(bytes))) return;
    memcpy(inst->previous, src, bytes);
#endif
    (void)time;
}

void f0r_update(void *instance, double time, const uint32_t *in, uint32_t *out) {
    instance_t *inst = instance;
    const char *mode = getenv("KLIPP_FIXTURE_MODE");
    if (!mode) mode = "normal";
    if (!strcmp(mode, "hang")) { for (;;) {} }
    size_t bytes = (size_t)inst->width * inst->height * 4;
    if (!strcmp(mode, "passthrough")) memcpy(out, in, bytes);
    else render(inst, time, (const uint8_t *)in, (uint8_t *)out);
    if (!strcmp(mode, "wrong-output")) ((uint8_t *)out)[0] ^= 1;
    if (strcmp(mode, "no-trace"))
        fprintf(stderr, "KLIPP_FIXTURE_RENDER %s %.17g %ux%u\n", NAME, time, inst->width, inst->height);
}

/* The OFX parameter suite's variadic entry points (paramGetValue,
 * paramSetValue and their AtTime, Derivative and Integral forms), as C
 * defines them: a plug-in passes one pointer per component to read a
 * value into, and the values themselves, promoted as C promotes them, to
 * set one. Only C can read a va_list, so each entry point here asks Klipp
 * what the parameter holds, reads or writes that many arguments, and
 * hands Klipp a plain array (param.rs). */

#include <stdarg.h>
#include <stddef.h>

typedef int OfxStatus;
typedef void *OfxParamHandle;

#define KLIPP_OFX_STAT_OK 0
#define KLIPP_OFX_STAT_ERR_VALUE 11

/* What a parameter's value is, from param.rs. */
#define KLIPP_PARAM_NONE 0
#define KLIPP_PARAM_DOUBLE 1
#define KLIPP_PARAM_INT 2
#define KLIPP_PARAM_STRING 3

/* The most components a value has: an RGBA. */
#define KLIPP_PARAM_MAX 4

/* In param.rs. */
OfxStatus klipp_ofx_param_shape(OfxParamHandle h, int *kind, int *count);
OfxStatus klipp_ofx_param_get_typed(OfxParamHandle h, int at_time, double time, void *out);
OfxStatus klipp_ofx_param_set_typed(OfxParamHandle h, int at_time, double time, const void *in);
OfxStatus klipp_ofx_param_derivative_typed(OfxParamHandle h, double time, double *out);
OfxStatus klipp_ofx_param_integral_typed(OfxParamHandle h, double time1, double time2, double *out);

/* The parameter's value at `time` (now when !at_time), one component
 * through each pointer in `ap`. A null pointer is skipped. */
static OfxStatus get_into(OfxParamHandle h, int at_time, double time, va_list ap) {
    int kind = KLIPP_PARAM_NONE, count = 0;
    OfxStatus st = klipp_ofx_param_shape(h, &kind, &count);
    if (st != KLIPP_OFX_STAT_OK || count > KLIPP_PARAM_MAX) {
        return st;
    }
    if (kind == KLIPP_PARAM_DOUBLE) {
        double v[KLIPP_PARAM_MAX] = {0};
        st = klipp_ofx_param_get_typed(h, at_time, time, v);
        for (int i = 0; i < count; i++) {
            double *p = va_arg(ap, double *);
            if (p) *p = v[i];
        }
    } else if (kind == KLIPP_PARAM_INT) {
        int v[KLIPP_PARAM_MAX] = {0};
        st = klipp_ofx_param_get_typed(h, at_time, time, v);
        for (int i = 0; i < count; i++) {
            int *p = va_arg(ap, int *);
            if (p) *p = v[i];
        }
    } else if (kind == KLIPP_PARAM_STRING) {
        const char *s = NULL;
        st = klipp_ofx_param_get_typed(h, at_time, time, &s);
        const char **p = va_arg(ap, const char **);
        if (p) *p = s;
    }
    return st;
}

/* The value given in `ap`, set now, or keyed at `time` when at_time. */
static OfxStatus set_from(OfxParamHandle h, int at_time, double time, va_list ap) {
    int kind = KLIPP_PARAM_NONE, count = 0;
    OfxStatus st = klipp_ofx_param_shape(h, &kind, &count);
    if (st != KLIPP_OFX_STAT_OK || count > KLIPP_PARAM_MAX) {
        return st;
    }
    if (kind == KLIPP_PARAM_DOUBLE) {
        double v[KLIPP_PARAM_MAX] = {0};
        for (int i = 0; i < count; i++) v[i] = va_arg(ap, double);
        return klipp_ofx_param_set_typed(h, at_time, time, v);
    }
    if (kind == KLIPP_PARAM_INT) {
        int v[KLIPP_PARAM_MAX] = {0};
        for (int i = 0; i < count; i++) v[i] = va_arg(ap, int);
        return klipp_ofx_param_set_typed(h, at_time, time, v);
    }
    if (kind == KLIPP_PARAM_STRING) {
        const char *s = va_arg(ap, const char *);
        return klipp_ofx_param_set_typed(h, at_time, time, &s);
    }
    return st;
}

/* A double parameter's derivative or integral, one component through
 * each pointer in `ap`; refused for any other kind, which has none. */
static OfxStatus doubles_into(OfxParamHandle h, OfxStatus (*f)(OfxParamHandle, double, double, double *), double t1, double t2, va_list ap) {
    int kind = KLIPP_PARAM_NONE, count = 0;
    OfxStatus st = klipp_ofx_param_shape(h, &kind, &count);
    if (st != KLIPP_OFX_STAT_OK) {
        return st;
    }
    if (kind != KLIPP_PARAM_DOUBLE || count > KLIPP_PARAM_MAX) {
        return KLIPP_OFX_STAT_ERR_VALUE;
    }
    double v[KLIPP_PARAM_MAX] = {0};
    st = f(h, t1, t2, v);
    for (int i = 0; i < count; i++) {
        double *p = va_arg(ap, double *);
        if (p) *p = v[i];
    }
    return st;
}

static OfxStatus derivative(OfxParamHandle h, double t1, double t2, double *out) {
    (void)t2;
    return klipp_ofx_param_derivative_typed(h, t1, out);
}

OfxStatus klipp_ofx_param_get_value(OfxParamHandle h, ...) {
    va_list ap;
    va_start(ap, h);
    OfxStatus st = get_into(h, 0, 0.0, ap);
    va_end(ap);
    return st;
}

OfxStatus klipp_ofx_param_get_value_at_time(OfxParamHandle h, double time, ...) {
    va_list ap;
    va_start(ap, time);
    OfxStatus st = get_into(h, 1, time, ap);
    va_end(ap);
    return st;
}

OfxStatus klipp_ofx_param_get_derivative(OfxParamHandle h, double time, ...) {
    va_list ap;
    va_start(ap, time);
    OfxStatus st = doubles_into(h, derivative, time, 0.0, ap);
    va_end(ap);
    return st;
}

OfxStatus klipp_ofx_param_get_integral(OfxParamHandle h, double time1, double time2, ...) {
    va_list ap;
    va_start(ap, time2);
    OfxStatus st = doubles_into(h, klipp_ofx_param_integral_typed, time1, time2, ap);
    va_end(ap);
    return st;
}

OfxStatus klipp_ofx_param_set_value(OfxParamHandle h, ...) {
    va_list ap;
    va_start(ap, h);
    OfxStatus st = set_from(h, 0, 0.0, ap);
    va_end(ap);
    return st;
}

OfxStatus klipp_ofx_param_set_value_at_time(OfxParamHandle h, double time, ...) {
    va_list ap;
    va_start(ap, time);
    OfxStatus st = set_from(h, 1, time, ap);
    va_end(ap);
    return st;
}

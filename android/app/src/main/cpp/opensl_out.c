// OpenSL ES output for Airwave: a ring buffer that Kotlin writes into, and a
// buffer queue that OpenSL ES drains in 10 ms pieces.

#include <jni.h>
#include <SLES/OpenSLES.h>
#include <SLES/OpenSLES_Android.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define NBUF 4

typedef struct {
    SLObjectItf engine_obj;
    SLEngineItf engine;
    SLObjectItf mix_obj;
    SLObjectItf player_obj;
    SLPlayItf play;
    SLAndroidSimpleBufferQueueItf queue;

    int16_t *ring;
    int ring_cap;   // samples
    int ring_head;  // next sample to play
    int ring_count; // samples waiting

    int16_t *bufs[NBUF];
    int buf_samples;
    int next_buf;

    pthread_mutex_t lock;
    pthread_cond_t space;
    float gain;
    int closed;
} Out;

static void fill_and_enqueue(Out *o) {
    int16_t *b = o->bufs[o->next_buf];
    o->next_buf = (o->next_buf + 1) % NBUF;

    pthread_mutex_lock(&o->lock);
    int n = o->ring_count < o->buf_samples ? o->ring_count : o->buf_samples;
    float g = o->gain;
    for (int i = 0; i < n; i++) {
        float v = o->ring[o->ring_head] * g;
        if (v > 32767.f) v = 32767.f;
        if (v < -32768.f) v = -32768.f;
        b[i] = (int16_t) v;
        o->ring_head = (o->ring_head + 1) % o->ring_cap;
    }
    o->ring_count -= n;
    pthread_cond_signal(&o->space);
    pthread_mutex_unlock(&o->lock);

    if (n < o->buf_samples) {
        memset(b + n, 0, (size_t) (o->buf_samples - n) * sizeof(int16_t));
    }
    (*o->queue)->Enqueue(o->queue, b, (SLuint32) (o->buf_samples * sizeof(int16_t)));
}

static void on_buffer_done(SLAndroidSimpleBufferQueueItf q, void *ctx) {
    (void) q;
    Out *o = (Out *) ctx;
    if (!o->closed) fill_and_enqueue(o);
}

static void destroy_out(Out *o) {
    if (!o) return;
    pthread_mutex_lock(&o->lock);
    o->closed = 1;
    pthread_cond_broadcast(&o->space);
    pthread_mutex_unlock(&o->lock);
    if (o->player_obj) (*o->player_obj)->Destroy(o->player_obj);
    if (o->mix_obj) (*o->mix_obj)->Destroy(o->mix_obj);
    if (o->engine_obj) (*o->engine_obj)->Destroy(o->engine_obj);
    for (int i = 0; i < NBUF; i++) free(o->bufs[i]);
    free(o->ring);
    pthread_cond_destroy(&o->space);
    pthread_mutex_destroy(&o->lock);
    free(o);
}

JNIEXPORT jlong JNICALL
Java_com_airwave_client_NativeAudio_create(JNIEnv *env, jobject thiz, jint rate, jint channels, jint buffer_frames) {
    (void) env;
    (void) thiz;
    if (rate <= 0 || channels < 1 || channels > 2) return 0;
    Out *o = (Out *) calloc(1, sizeof(Out));
    if (!o) return 0;
    pthread_mutex_init(&o->lock, NULL);
    pthread_cond_init(&o->space, NULL);
    o->gain = 1.f;
    o->buf_samples = (rate / 100) * channels;
    int frames = buffer_frames > rate / 50 ? buffer_frames : rate / 50;
    o->ring_cap = frames * channels * 2;
    o->ring = (int16_t *) calloc((size_t) o->ring_cap, sizeof(int16_t));
    for (int i = 0; i < NBUF; i++) o->bufs[i] = (int16_t *) calloc((size_t) o->buf_samples, sizeof(int16_t));

    SLresult r = slCreateEngine(&o->engine_obj, 0, NULL, 0, NULL, NULL);
    if (r != SL_RESULT_SUCCESS) goto fail;
    if ((*o->engine_obj)->Realize(o->engine_obj, SL_BOOLEAN_FALSE) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->engine_obj)->GetInterface(o->engine_obj, SL_IID_ENGINE, &o->engine) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->engine)->CreateOutputMix(o->engine, &o->mix_obj, 0, NULL, NULL) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->mix_obj)->Realize(o->mix_obj, SL_BOOLEAN_FALSE) != SL_RESULT_SUCCESS) goto fail;

    SLDataLocator_AndroidSimpleBufferQueue loc_bq = {SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE, NBUF};
    SLDataFormat_PCM fmt = {
        SL_DATAFORMAT_PCM,
        (SLuint32) channels,
        (SLuint32) rate * 1000, // milliHertz
        SL_PCMSAMPLEFORMAT_FIXED_16,
        SL_PCMSAMPLEFORMAT_FIXED_16,
        channels == 1 ? SL_SPEAKER_FRONT_CENTER : (SL_SPEAKER_FRONT_LEFT | SL_SPEAKER_FRONT_RIGHT),
        SL_BYTEORDER_LITTLEENDIAN,
    };
    SLDataSource src = {&loc_bq, &fmt};
    SLDataLocator_OutputMix loc_mix = {SL_DATALOCATOR_OUTPUTMIX, o->mix_obj};
    SLDataSink sink = {&loc_mix, NULL};
    const SLInterfaceID ids[1] = {SL_IID_ANDROIDSIMPLEBUFFERQUEUE};
    const SLboolean req[1] = {SL_BOOLEAN_TRUE};
    if ((*o->engine)->CreateAudioPlayer(o->engine, &o->player_obj, &src, &sink, 1, ids, req) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->player_obj)->Realize(o->player_obj, SL_BOOLEAN_FALSE) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->player_obj)->GetInterface(o->player_obj, SL_IID_PLAY, &o->play) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->player_obj)->GetInterface(o->player_obj, SL_IID_ANDROIDSIMPLEBUFFERQUEUE, &o->queue) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->queue)->RegisterCallback(o->queue, on_buffer_done, o) != SL_RESULT_SUCCESS) goto fail;
    if ((*o->play)->SetPlayState(o->play, SL_PLAYSTATE_PLAYING) != SL_RESULT_SUCCESS) goto fail;
    // Prime the queue; each finished buffer asks for the next one.
    for (int i = 0; i < NBUF; i++) fill_and_enqueue(o);
    return (jlong) (intptr_t) o;

fail:
    destroy_out(o);
    return 0;
}

JNIEXPORT jint JNICALL
Java_com_airwave_client_NativeAudio_write(JNIEnv *env, jobject thiz, jlong handle, jshortArray data, jint count) {
    (void) thiz;
    Out *o = (Out *) (intptr_t) handle;
    if (!o) return 0;
    jshort tmp[2048];
    int done = 0;
    while (done < count) {
        int k = count - done;
        if (k > 2048) k = 2048;
        (*env)->GetShortArrayRegion(env, data, done, k, tmp);
        pthread_mutex_lock(&o->lock);
        while (!o->closed && o->ring_cap - o->ring_count < k) {
            struct timespec ts;
            clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_nsec += 200 * 1000000L;
            if (ts.tv_nsec >= 1000000000L) {
                ts.tv_sec += 1;
                ts.tv_nsec -= 1000000000L;
            }
            pthread_cond_timedwait(&o->space, &o->lock, &ts);
        }
        if (o->closed) {
            pthread_mutex_unlock(&o->lock);
            break;
        }
        int tail = (o->ring_head + o->ring_count) % o->ring_cap;
        for (int i = 0; i < k; i++) {
            o->ring[tail] = tmp[i];
            tail = (tail + 1) % o->ring_cap;
        }
        o->ring_count += k;
        pthread_mutex_unlock(&o->lock);
        done += k;
    }
    return done;
}

JNIEXPORT void JNICALL
Java_com_airwave_client_NativeAudio_setGain(JNIEnv *env, jobject thiz, jlong handle, jfloat gain) {
    (void) env;
    (void) thiz;
    Out *o = (Out *) (intptr_t) handle;
    if (!o) return;
    pthread_mutex_lock(&o->lock);
    o->gain = gain;
    pthread_mutex_unlock(&o->lock);
}

JNIEXPORT void JNICALL
Java_com_airwave_client_NativeAudio_destroy(JNIEnv *env, jobject thiz, jlong handle) {
    (void) env;
    (void) thiz;
    destroy_out((Out *) (intptr_t) handle);
}
